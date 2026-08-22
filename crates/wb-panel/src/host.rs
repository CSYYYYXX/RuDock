//! Host-side dispatch for page → host WebMessages, and the worker→UI
//! postback queue (WebView2 calls must happen on the UI thread, so worker
//! threads enqueue JSON and poke the host window with WM_WB_POST).

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindowVisible, KillTimer, PostMessageW, SetForegroundWindow, SetTimer, ShowWindow,
    SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WM_APP,
};

pub const WM_WB_POST: u32 = WM_APP + 42;
/// Sent by wb-hook (bare Win key) to show/hide the panel.
pub const WM_WB_TOGGLE: u32 = WM_APP + 41;
/// Sent by wb-daemon（panel.show / panel.hide）：显式显隐，供 CLI/Agent 调用。
pub const WM_WB_SHOW: u32 = WM_APP + 43;
pub const WM_WB_HIDE: u32 = WM_APP + 44;
/// Sent by wb-daemon after the pinned desktop widget selection changes.
pub const WM_WB_DESKTOP_REFRESH: u32 = WM_APP + 45;
/// Fallback timer id: force-hide if the page never answers "hide.done".
pub const HIDE_TIMER_ID: usize = 7;
/// Fallback timer id: reveal even if the page never answers "show.ready".
pub const SHOW_TIMER_ID: usize = 8;
/// Desktop-only low-frequency z-order/visibility repair.
pub const DESKTOP_REPAIR_TIMER_ID: usize = 10;

static PENDING_HIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PENDING_SHOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static AUTOHIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static INTERACTION_LOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DESKTOP_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SETTINGS_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 失焦自动隐藏开关（--no-autohide 测试用）
pub fn set_autohide(on: bool) {
    AUTOHIDE.store(on, std::sync::atomic::Ordering::SeqCst);
}
pub fn autohide() -> bool {
    AUTOHIDE.load(std::sync::atomic::Ordering::SeqCst)
}
pub fn interaction_locked() -> bool {
    INTERACTION_LOCK.load(std::sync::atomic::Ordering::SeqCst)
}
pub fn set_desktop_mode(on: bool) {
    DESKTOP_MODE.store(on, std::sync::atomic::Ordering::SeqCst);
}
pub fn desktop_mode() -> bool {
    DESKTOP_MODE.load(std::sync::atomic::Ordering::SeqCst)
}
pub fn set_settings_mode(on: bool) { SETTINGS_MODE.store(on, std::sync::atomic::Ordering::SeqCst); }
pub fn settings_mode() -> bool { SETTINGS_MODE.load(std::sync::atomic::Ordering::SeqCst) }

static PENDING: Mutex<Vec<serde_json::Value>> = Mutex::new(Vec::new());
static HOST_HWND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_host_hwnd(hwnd: HWND) {
    HOST_HWND.store(hwnd.0 as usize, std::sync::atomic::Ordering::SeqCst);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Called from the WebMessageReceived handler (UI thread).
pub fn on_web_message(text: &str) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    let kind = msg.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    match kind {
        "query" => {
            let id = msg.get("id").cloned().unwrap_or(serde_json::json!(0));
            let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
            // IPC can block (daemon cold start) — never inside the WV2 callback.
            std::thread::spawn(move || {
                let reply = match crate::ipc::Client::connect()
                    .and_then(|mut c| c.call("search", serde_json::json!({"query": text, "limit": 20})))
                {
                    Ok(result) => serde_json::json!({"kind":"results","id":id,"results":result}),
                    Err(e) => serde_json::json!({"kind":"results","id":id,"results":[],"error":e}),
                };
                enqueue_post(reply);
            });
        }
        "rpc" => {
            // Generic daemon passthrough for widgets (todo.list, note.add, ...).
            let id = msg.get("id").cloned().unwrap_or(serde_json::json!(0));
            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
            let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));
            std::thread::spawn(move || {
                let reply = match crate::ipc::Client::connect().and_then(|mut c| c.call(&method, params)) {
                    Ok(result) => serde_json::json!({"kind":"rpc","id":id,"result":result}),
                    Err(e) => serde_json::json!({"kind":"rpc","id":id,"error":e}),
                };
                enqueue_post(reply);
            });
        }
        "hide" => request_hide(),
        "hide.done" => hide_now(),
        "show.ready" => reveal_now(),
        "interaction" => {
            let active = msg.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
            INTERACTION_LOCK.store(active, std::sync::atomic::Ordering::SeqCst);
        }
        "action" => handle_action(&msg),
        "sysinfo" => {
            let id = msg.get("id").cloned().unwrap_or(serde_json::json!(0));
            std::thread::spawn(move || enqueue_post(sysinfo_json(id)));
        }
        "icon" => {
            let path = msg.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
            if !path.is_empty() {
                std::thread::spawn(move || {
                    let data = crate::icons::icon_data_url(&path).ok();
                    if let Some(data_url) = data {
                        enqueue_post(serde_json::json!({"kind":"icon","path":path,"dataUrl":data_url}));
                    }
                });
            }
        }
        "media" => {
            std::thread::spawn(move || {
                let reply = match crate::media::current() {
                    Ok(Some(info)) => serde_json::json!({
                        "kind":"media", "active":true, "playing":info.playing,
                        "title":info.title, "artist":info.artist, "art":info.art_data_url,
                    }),
                    Ok(None) => serde_json::json!({"kind":"media","active":false}),
                    Err(e) => serde_json::json!({"kind":"media","active":false,"error":e}),
                };
                enqueue_post(reply);
            });
        }
        "media.cmd" => {
            let cmd = msg.get("cmd").and_then(|c| c.as_str()).unwrap_or("").to_string();
            std::thread::spawn(move || {
                let _ = crate::media::command(&cmd);
                // Push a fresh snapshot right after the command.
                std::thread::sleep(std::time::Duration::from_millis(400));
                let reply = match crate::media::current() {
                    Ok(Some(info)) => serde_json::json!({
                        "kind":"media", "active":true, "playing":info.playing,
                        "title":info.title, "artist":info.artist, "art":info.art_data_url,
                    }),
                    _ => serde_json::json!({"kind":"media","active":false}),
                };
                enqueue_post(reply);
            });
        }
        "weather" => {
            std::thread::spawn(move || {
                let reply = match crate::weather::current() {
                    Ok(data) => serde_json::json!({"kind":"weather","ok":true,"data":data}),
                    Err(e) => serde_json::json!({"kind":"weather","ok":false,"error":e}),
                };
                enqueue_post(reply);
            });
        }
        "ai.ask" => {
            // 随手问 AI：? 前缀触发，gpt-5.6-luna，SSE 流式回推 ai.delta/ai.done/ai.error
            let id = msg.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
            let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("").trim().to_string();
            if !text.is_empty() {
                std::thread::spawn(move || crate::ai::ask(id, text));
            }
        }
        "ready" => {
            // Page booted and painted: capture the backdrop, then reveal.
            show_panel();
        }
        "cardrects" => {
            // 真透明：页面把卡片矩形（DIP）报上来，宿主换算物理像素后对
            // 这些区域开 DWM 实时模糊；缝隙不模糊 → 透出活桌面。
            let hwnd = HWND(HOST_HWND.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void);
            if !hwnd.0.is_null() {
                let mut dips: Vec<(f64, f64, f64, f64)> = Vec::new();
                if let Some(arr) = msg.get("rects").and_then(|r| r.as_array()) {
                    for r in arr {
                        if let Some(rc) = r.as_array() {
                            if rc.len() == 4 {
                                let v: Vec<f64> = rc.iter().filter_map(|n| n.as_f64()).collect();
                                if v.len() == 4 {
                                    dips.push((v[0], v[1], v[2], v[3]));
                                }
                            }
                        }
                    }
                }
                let scale = unsafe { GetDpiForWindow(hwnd) } as f64 / 96.0;
                let phys: Vec<(i32, i32, i32, i32)> = dips
                    .iter()
                    .map(|&(x, y, w, h)| {
                        ((x * scale).round() as i32, (y * scale).round() as i32,
                         (w * scale).round() as i32, (h * scale).round() as i32)
                    })
                    .collect();
                if settings_mode() {
                    // Settings is a normal opaque, resizable window. It must
                    // never inherit the panel's card-shaped native region.
                } else if desktop_mode() {
                    // Desktop cards already render their own CSS glass. Keep
                    // the native window region stable while WebView2 is
                    // restoring; transient empty reports must not collapse
                    // the whole host to a 1x1 black/flickering region.
                    if !phys.is_empty() {
                        crate::dwm::set_desktop_regions(hwnd, &phys);
                        crate::dwm::pin_to_desktop(hwnd);
                    }
                } else {
                    crate::dwm::set_card_regions(hwnd, &phys, (20.0 * scale).round() as i32);
                }
            }
        }
        "selftest" => {
            // Self-test page reporting back; surface it on stdout for CI.
            println!("{}", serde_json::json!({"event":"selftest","detail":msg.get("detail").cloned().unwrap_or(serde_json::Value::Null),"ok":msg.get("ok").cloned().unwrap_or(serde_json::Value::Null)}));
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        _ => {}
    }
}

fn handle_action(msg: &serde_json::Value) {
    match msg.get("action").and_then(|a| a.as_str()).unwrap_or("") {
        "open" => {
            if let Some(path) = msg.get("path").and_then(|p| p.as_str()) {
                unsafe {
                    let p = wide(path);
                    let r = ShellExecuteW(None, PCWSTR::null(), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                    // ShellExecuteW returns HINSTANCE; >32 means success.
                    if (r.0 as usize) <= 32 {
                        enqueue_post(serde_json::json!({"kind":"toast","text":format!("无法打开: {path}")}));
                        return;
                    }
                }
                request_hide();
            }
        }
        "copy" => {
            if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                if set_clipboard_text(content).is_ok() {
                    request_hide();
                } else {
                    enqueue_post(serde_json::json!({"kind":"toast","text":"写入剪贴板失败"}));
                }
            }
        }
        "reveal" => {
            if let Some(path) = msg.get("path").and_then(|p| p.as_str()) {
                match reveal_in_explorer(path) {
                    Ok(()) => request_hide(),
                    Err(error) => enqueue_post(
                        serde_json::json!({"kind":"toast","text":error}),
                    ),
                }
            }
        }
        "hide" => request_hide(),
        _ => {}
    }
}

fn reveal_in_explorer(path: &str) -> Result<(), String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let target = std::path::Path::new(path);
    if !target.exists() {
        return Err("文件已经移动或删除".into());
    }
    let explorer = std::env::var_os("SYSTEMROOT")
        .map(std::path::PathBuf::from)
        .map(|root| root.join("explorer.exe"))
        .unwrap_or_else(|| "explorer.exe".into());
    let mut command = std::process::Command::new(explorer);
    if target.is_dir() {
        command.arg(target);
    } else {
        command.arg(format!("/select,{}", target.to_string_lossy()));
    }
    command
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|_| "无法在资源管理器中定位文件".into())
}

fn set_clipboard_text(text: &str) -> Result<(), ()> {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let w = wide(text);
    unsafe {
        OpenClipboard(None).map_err(|_| ())?;
        let ok = (|| {
            EmptyClipboard().map_err(|_| ())?;
            let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2).map_err(|_| ())?;
            let p = GlobalLock(h);
            if p.is_null() {
                return Err(());
            }
            std::ptr::copy_nonoverlapping(w.as_ptr(), p as *mut u16, w.len());
            let _ = GlobalUnlock(h);
            SetClipboardData(CF_UNICODETEXT, windows::Win32::Foundation::HANDLE(h.0)).map_err(|_| ())?;
            Ok(())
        })();
        let _ = CloseClipboard();
        ok
    }
}

/// RAM + CPU snapshot for the sysinfo widget. CPU% needs two samples;
/// the first call reports 0 and seeds the baseline.
fn sysinfo_json(id: serde_json::Value) -> serde_json::Value {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows::Win32::System::Threading::GetSystemTimes;
    let (mut mem_total, mut mem_used_pct) = (0u64, 0u64);
    unsafe {
        let mut ms = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if GlobalMemoryStatusEx(&mut ms).is_ok() {
            mem_total = ms.ullTotalPhys;
            mem_used_pct = ms.dwMemoryLoad as u64;
        }
    }
    static PREV: Mutex<Option<(u64, u64, u64)>> = Mutex::new(None);
    let mut cpu_pct = 0u64;
    unsafe {
        let mut idle = std::mem::zeroed();
        let mut kernel = std::mem::zeroed();
        let mut user = std::mem::zeroed();
        if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_ok() {
            let ft = |f: windows::Win32::Foundation::FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
            let (i, k, u) = (ft(idle), ft(kernel), ft(user));
            let mut prev = PREV.lock().unwrap();
            if let Some((pi, pk, pu)) = *prev {
                let di = i.saturating_sub(pi);
                let dk = k.saturating_sub(pk);
                let du = u.saturating_sub(pu);
                let total = dk + du;
                if total > 0 {
                    cpu_pct = (total.saturating_sub(di)) * 100 / total;
                }
            }
            *prev = Some((i, k, u));
        }
    }
    // Battery / AC (desktop without battery reports 255 → null)
    let mut battery_pct = serde_json::Value::Null;
    let mut ac_online = serde_json::Value::Null;
    unsafe {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut sps = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut sps).is_ok() {
            if sps.BatteryLifePercent <= 100 {
                battery_pct = serde_json::json!(sps.BatteryLifePercent);
            }
            if sps.ACLineStatus <= 1 {
                ac_online = serde_json::json!(sps.ACLineStatus == 1);
            }
        }
    }
    serde_json::json!({
        "kind": "sysinfo", "id": id,
        "cpu_pct": cpu_pct,
        "mem_used_pct": mem_used_pct,
        "mem_total_gb": (mem_total as f64 / 1073741824.0 * 10.0).round() / 10.0,
        "battery_pct": battery_pct,
        "ac_online": ac_online,
    })
}

/// Show + foreground the panel. 时序修复（不再"卡一下→静帧→动画"）：
/// 1) 只取缓存背景（隐藏期间已后台预截；首次启动缓存为空才同步截一次）
/// 2) 发 "show" 给页面：页面把卡片摆到动画第 0 帧并冻结，回 "show.ready"
/// 3) 收到 ready（或 200ms 兜底超时）→ ShowWindow + 发 "go" 解冻动画
///    → 亮窗帧就是动画第 0 帧，绝无静帧闪现。
pub fn show_panel() {
    let hwnd = HWND(HOST_HWND.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void);
    if hwnd.0.is_null() {
        return;
    }
    if desktop_mode() {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        crate::dwm::pin_to_desktop(hwnd);
        return;
    }
    if settings_mode() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }
    cancel_hide();
    unsafe {
        if IsWindowVisible(hwnd).as_bool() {
            return;
        }
        if PENDING_SHOW.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return; // 已经在上画流程中
        }
        if crate::webview2::is_ready() {
            if crate::backdrop::fakebg_enabled() {
                let bg = crate::backdrop::cached_data_url()
                    .or_else(|| crate::backdrop::capture_data_url()); // 仅首次启动
                if let Some(url) = bg {
                    enqueue_post(serde_json::json!({"kind":"bg","dataUrl":url}));
                }
            }
            let _ = SetTimer(hwnd, SHOW_TIMER_ID, 200, None);
            enqueue_post(serde_json::json!({"kind":"show"}));
        } else {
            // 页面还没起：直接亮（ready 会再走一遍完整流程）
            PENDING_SHOW.store(false, std::sync::atomic::Ordering::SeqCst);
            if crate::dwm::card_frost_mode() {
                crate::dwm::frost_show(true);
            } else {
                crate::dwm::veil_show(hwnd, true);
            }
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// 页面第 0 帧就绪（或兜底超时）：真正亮窗并解冻动画。
pub fn reveal_now() {
    if !PENDING_SHOW.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let hwnd = HWND(HOST_HWND.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void);
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = KillTimer(hwnd, SHOW_TIMER_ID);
        // 磨砂先亮（不抢焦点），面板随后盖在上面
        if crate::dwm::card_frost_mode() {
            crate::dwm::frost_show(true);
        } else {
            crate::dwm::veil_show(hwnd, true);
        }
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    crate::webview2::post_json(&serde_json::json!({"kind":"go"}));
}

/// Ask the page to play its exit animation; the window is really hidden when
/// the page posts "hide.done" (or the fallback timer fires).
pub fn request_hide() {
    let hwnd = HWND(HOST_HWND.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void);
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return;
        }
        if settings_mode() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        } else if crate::webview2::is_ready() {
            PENDING_HIDE.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = SetTimer(hwnd, HIDE_TIMER_ID, 380, None);
            if !crate::dwm::card_frost_mode() {
                crate::dwm::veil_show(hwnd, false); // veil 与卡片消失动画同步淡出
            }
            crate::webview2::post_json(&serde_json::json!({"kind":"hide"}));
        } else {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Cancel an in-flight animated hide (user re-summoned the panel mid-exit).
pub fn cancel_hide() {
    PENDING_HIDE.store(false, std::sync::atomic::Ordering::SeqCst);
    let hwnd = HWND(HOST_HWND.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void);
    if !hwnd.0.is_null() {
        unsafe {
            let _ = KillTimer(hwnd, HIDE_TIMER_ID);
        }
    }
}

/// Request an animated hide from any thread (AI tool `panel.hide` 用).
/// Real work happens in the host window's wnd_proc on WM_WB_HIDE.
pub fn post_hide_message() {
    let hwnd = HWND(HOST_HWND.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void);
    if !hwnd.0.is_null() {
        unsafe {
            let _ = PostMessageW(hwnd, WM_WB_HIDE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Actually hide the host window (called on "hide.done" or the fallback timer).
/// Idempotent: a stale timer or double hide.done is harmless.
pub fn hide_now() {
    PENDING_HIDE.store(false, std::sync::atomic::Ordering::SeqCst);
    INTERACTION_LOCK.store(false, std::sync::atomic::Ordering::SeqCst);
    let hwnd = HWND(HOST_HWND.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void);
    if !hwnd.0.is_null() {
        unsafe {
            let _ = KillTimer(hwnd, HIDE_TIMER_ID);
            crate::dwm::frost_show(false);
            if IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
    // 真透明模式无需截图；--fakebg 时隐藏期间后台预截图，下一次呼出零等待。
    if crate::backdrop::fakebg_enabled() {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(120)); // 等 DWM 落定
            let _ = crate::backdrop::capture_data_url();
        });
    }
}

/// Public wrapper: queue a JSON payload for the page (thread-safe).
pub fn post_to_page(v: serde_json::Value) {
    enqueue_post(v);
}

fn enqueue_post(v: serde_json::Value) {
    PENDING.lock().unwrap().push(v);
    let hwnd = HWND(HOST_HWND.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void);
    if !hwnd.0.is_null() {
        unsafe {
            let _ = PostMessageW(hwnd, WM_WB_POST, WPARAM(0), LPARAM(0));
        }
    }
}

/// Drain pending posts — call from the host window's wnd_proc on WM_WB_POST.
pub fn flush_posts() {
    let items: Vec<serde_json::Value> = std::mem::take(&mut *PENDING.lock().unwrap());
    for item in items {
        crate::webview2::post_json(&item);
    }
}

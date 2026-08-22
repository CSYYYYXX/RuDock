//! PoC 2a: DWM Mica/Acrylic host window + show/hide latency benchmark.
//!
//! Modes:
//!   wb-panel --bench <n>   warm window, n× hide→show, print latency JSON, exit
//!   wb-panel               interactive: Mica window, Esc hides, Ctrl+W quits
//!
//! PoC 2b (WebView2 embed) lives in webview2.rs behind --wv2.

mod ai;
mod backdrop;
mod dwm;
mod host;
mod icons;
mod ipc;
mod media;
mod weather;
mod webview2;

use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, UpdateWindow, PAINTSTRUCT};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::*;

pub(crate) const WINDOW_W: i32 = 520; // right-docked panel width; height = work area
pub(crate) const WINDOW_H: i32 = 560; // bench/fallback only; real height comes from work area

struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Returns `None` for a secondary process after it has asked the existing
/// panel to show. The named mutex also closes the startup race before HWND exists.
fn acquire_single_instance(desktop: bool) -> Result<Option<SingleInstance>, String> {
    let mutex_name: Vec<u16> = if desktop {
        "Local\\WBDesktopWidgetsSingleInstance\0"
    } else {
        "Local\\WBPanelSingleInstance\0"
    }
    .encode_utf16()
    .collect();
    let mutex = unsafe { CreateMutexW(None, true, PCWSTR(mutex_name.as_ptr())) }
        .map_err(|e| format!("single-instance mutex failed: {e}"))?;
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Ok(Some(SingleInstance(mutex)));
    }

    let class_name: Vec<u16> = if desktop { "WBDesktopWidgets\0" } else { "WBPanelPoc\0" }
        .encode_utf16()
        .collect();
    let hwnd = unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), None) }.unwrap_or_default();
    let awakened = if hwnd.0.is_null() {
        false
    } else {
        unsafe { PostMessageW(hwnd, host::WM_WB_SHOW, WPARAM(0), LPARAM(0)) }.is_ok()
    };
    unsafe {
        let _ = CloseHandle(mutex);
    }
    println!(
        "{}",
        serde_json::json!({"event":"already_running","awakened":awakened})
    );
    Ok(None)
}

fn main() {
    // PerMonitorV2 before any window exists — launcher must get physical pixels.
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let args: Vec<String> = std::env::args().collect();
    let desktop = args.iter().any(|a| a == "--desktop");

    // Headless icon-extraction test: wb-panel --icon-test <lnk-or-exe> <out.png>
    if let Some(pos) = args.iter().position(|a| a == "--icon-test") {
        let (Some(src), Some(out)) = (args.get(pos + 1), args.get(pos + 2)) else {
            eprintln!("usage: --icon-test <path> <out.png>");
            std::process::exit(2);
        };
        match icons::icon_data_url(src) {
            Ok(url) => {
                let b64 = url.trim_start_matches("data:image/png;base64,");
                match icons::b64_decode(b64) {
                    Ok(bytes) => match std::fs::write(out, &bytes) {
                        Ok(()) => {
                            println!("{}", serde_json::json!({"event":"icon_test","ok":true,"bytes":bytes.len(),"out":out}));
                            std::process::exit(0);
                        }
                        Err(e) => { eprintln!("write: {e}"); std::process::exit(1); }
                    },
                    Err(e) => { eprintln!("b64: {e}"); std::process::exit(1); }
                }
            }
            Err(e) => { eprintln!("icon: {e}"); std::process::exit(1); }
        }
    }

    let diagnostic_instance = args
        .iter()
        .any(|a| a == "--allow-multiple" || a == "--bench");
    let _single_instance = if diagnostic_instance {
        None
    } else {
        match acquire_single_instance(desktop) {
            Ok(Some(instance)) => Some(instance),
            Ok(None) => return,
            Err(e) => {
                eprintln!("fatal: {e}");
                std::process::exit(1);
            }
        }
    };

    let wv2 = args.iter().any(|a| a == "--wv2");
    // 测试用：失焦不自动隐藏（截图验证 AI 流式等慢速链路时，用户的窗口会抢焦点）
    host::set_desktop_mode(desktop);
    host::set_autohide(!desktop && !args.iter().any(|a| a == "--no-autohide"));
    let duration: Option<u64> = args
        .windows(2)
        .find(|w| w[0] == "--duration")
        .and_then(|w| w[1].parse().ok());
    let bench: Option<u32> = args
        .windows(2)
        .find(|w| w[0] == "--bench")
        .and_then(|w| w[1].parse().ok());

    // 磨砂池必须先创建：后创建的面板在 Z 序上盖过它们。
    if !desktop {
        dwm::create_frost_pool();
    }
    let hwnd = match dwm::create_panel_window(desktop) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("fatal: {e}");
            std::process::exit(1);
        }
    };
    let plain = args.iter().any(|a| a == "--plain-win");
    let material = if plain { "none(ab-test)" } else { dwm::apply_material(hwnd) };
    host::set_host_hwnd(hwnd);
    if desktop {
        dwm::set_desktop_regions(hwnd, &[]);
    }
    println!("{}", serde_json::json!({"event": "window_created", "material": material, "w": WINDOW_W, "h": WINDOW_H}));

    if wv2 {
        match webview2::embed(hwnd) {
            Ok(()) => println!("{}", serde_json::json!({"event": "webview2_ready"})),
            Err(e) => {
                println!("{}", serde_json::json!({"event": "webview2_failed", "detail": e}));
                std::process::exit(1);
            }
        }
    }

    if let Some(n) = bench {
        let mut stats = bench_show_hide(hwnd, n);
        let obj = stats.as_object_mut().unwrap();
        obj.insert("event".into(), "bench".into());
        obj.insert("rounds".into(), n.into());
        println!("{stats}");
        return;
    }

    // Interactive message loop (PoC: plain loop; production moves to wb-panel proper).
    unsafe {
        if wv2 {
            // Start HIDDEN: the page posts "ready" after boot; only then do we
            // capture the backdrop, show and foreground — no dark/white flash.
            let url = webview2::resolve_url().map(|mut url| {
                if desktop {
                    url.push_str(if url.contains('?') { "&mode=desktop" } else { "?mode=desktop" });
                }
                url
            });
            match url.and_then(|u| webview2::navigate(&u)) {
                Ok(()) => {}
                Err(e) => println!("{}", serde_json::json!({"event":"navigate_failed","detail": e})),
            }
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
        }
        let mut msg = MSG::default();
        if let Some(secs) = duration {
            // Timed run for automation: pump N seconds then clean exit.
            if wv2 {
                std::thread::sleep(std::time::Duration::from_millis(800));
                webview2::diag(hwnd);
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            while std::time::Instant::now() < deadline {
                while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            println!("{}", serde_json::json!({"event": "timed_run_done", "secs": secs}));
            return;
        }
        while GetMessageW(&mut msg, HWND::default(), 0, 0).into() {
            if msg.message == WM_KEYDOWN && msg.wParam.0 == 0x1B {
                // Esc: hide (panel semantics: close on Esc)
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Hide→show latency: the number DeskBox struggled with on Win10 (real-window moves).
fn bench_show_hide(hwnd: HWND, rounds: u32) -> serde_json::Value {
    let mut samples = Vec::with_capacity(rounds as usize);
    // warm
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    for _ in 0..rounds {
        let t0 = Instant::now();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = UpdateWindow(hwnd);
        }
        let show_us = t0.elapsed().as_micros();
        samples.push(show_us);
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    samples.sort_unstable();
    let p = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    serde_json::json!({
        "p50_us": p(0.50), "p95_us": p(0.95), "max_us": samples.last(),
        "p95_ms": p(0.95) as f64 / 1000.0,
        "target_p95_ms": 100,
        "pass": (p(0.95) as f64 / 1000.0) < 100.0,
    })
}

pub(crate) unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        m if m == host::WM_WB_POST => {
            host::flush_posts();
            LRESULT(0)
        }
        m if m == host::WM_WB_TOGGLE => {
            // Bare Win from wb-hook: animated show or hide.
            if IsWindowVisible(hwnd).as_bool() {
                host::request_hide();
            } else {
                host::show_panel();
            }
            LRESULT(0)
        }
        m if m == host::WM_WB_SHOW => {
            // daemon panel.show（CLI/Agent）：显式显示，幂等
            if !IsWindowVisible(hwnd).as_bool() {
                host::show_panel();
            }
            LRESULT(0)
        }
        m if m == host::WM_WB_HIDE => {
            // daemon panel.hide（CLI/Agent / AI 工具）：显式隐藏，幂等
            if IsWindowVisible(hwnd).as_bool() {
                host::request_hide();
            }
            LRESULT(0)
        }
        m if m == host::WM_WB_DESKTOP_REFRESH => {
            if host::desktop_mode() {
                host::post_to_page(serde_json::json!({"kind":"desktop.refresh"}));
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // let DWM backdrop show through
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if host::desktop_mode() {
                dwm::pin_to_desktop(hwnd);
                return LRESULT(0);
            }
            // Spotlight semantics: losing focus dismisses the panel.
            if host::autohide()
                && !host::interaction_locked()
                && wparam.0 as u32 & 0xffff == WA_INACTIVE
            {
                host::request_hide();
            }
            LRESULT(0)
        }
        WM_TIMER => {
            // Fallbacks: page never answered hide.done / show.ready.
            if wparam.0 == host::HIDE_TIMER_ID {
                host::hide_now();
            } else if wparam.0 == host::SHOW_TIMER_ID {
                host::reveal_now();
            } else if wparam.0 == dwm::FROST_FADE_TIMER_ID {
                dwm::frost_fade_tick(hwnd);
            }
            LRESULT(0)
        }
        // Docked panel: no dragging (default HTCLIENT behavior).
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

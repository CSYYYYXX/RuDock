//! DWM material: Mica on Win11, acrylic/solid fallback table (技术方案 §8.1).
//! Whole client area extended into the frame so DWM draws the backdrop.

use std::sync::Mutex;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND};
use windows::Win32::Graphics::Dwm::{DwmEnableBlurBehindWindow, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_WINDOW_CORNER_PREFERENCE, DWM_BB_ENABLE, DWM_BB_BLURREGION, DWM_BLURBEHIND};
use windows::Win32::Graphics::Gdi::{CombineRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, SetWindowRgn, RGN_OR};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::wnd_proc;

const DWMWCP_ROUND: u32 = 2;
const DWMSBT_NONE: u32 = 1;
const DWMSBT_MAINWINDOW: u32 = 2; // Mica
const DWMSBT_TRANSIENTWINDOW: u32 = 3; // Acrylic
const DWMSBT_TABBEDWINDOW: u32 = 4; // Mica Alt

pub fn create_panel_window(desktop: bool) -> windows::core::Result<HWND> {
    unsafe {
        let hinst = HINSTANCE::from(GetModuleHandleW(None)?);
        let brush = CreateSolidBrush(COLORREF(0x0020_2020)); // fallback solid bg
        let class_name: Vec<u16> = if desktop { "WBDesktopWidgets\0" } else { "WBPanelPoc\0" }
            .encode_utf16()
            .collect();
        let title: Vec<u16> = if desktop { "WB Desktop Widgets\0" } else { "WB Panel\0" }
            .encode_utf16()
            .collect();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
            hbrBackground: brush,
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let ex = if desktop { WS_EX_TOOLWINDOW } else { WS_EX_TOPMOST | WS_EX_TOOLWINDOW };
        let style = WS_POPUP;
        // Full work-area overlay (Spotlight-style): the page paints a captured
        // wallpaper backdrop; the widget board docks on the right half and the
        // launcher box floats centered in the free half. 代替 Win 键搜索。
        let mut rc = std::mem::zeroed::<windows::Win32::Foundation::RECT>();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut rc as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))?;
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        let x = rc.left;
        let y = rc.top;
        CreateWindowExW(
            ex,
            windows::core::PCWSTR(class_name.as_ptr()),
            windows::core::PCWSTR(title.as_ptr()),
            style,
            x,
            y,
            w,
            h,
            None,
            None,
            hinst,
            None,
        )
    }
}

pub fn set_desktop_regions(hwnd: HWND, rects: &[(i32, i32, i32, i32)]) {
    unsafe {
        let region = if let Some(&(x, y, width, height)) = rects.first() {
            let acc = CreateRoundRectRgn(x, y, x + width, y + height, 20, 20);
            for &(x, y, width, height) in &rects[1..] {
                let next = CreateRoundRectRgn(x, y, x + width, y + height, 20, 20);
                if !next.is_invalid() {
                    let _ = CombineRgn(acc, acc, next, RGN_OR);
                    let _ = DeleteObject(next);
                }
            }
            acc
        } else {
            CreateRoundRectRgn(0, 0, 1, 1, 1, 1)
        };
        if !region.is_invalid() {
            if SetWindowRgn(hwnd, region, true) == 0 {
                let _ = DeleteObject(region);
            }
        }
    }
}

pub fn pin_to_desktop(hwnd: HWND) {
    unsafe {
        let progman = FindWindowW(w!("Progman"), None).unwrap_or_default();
        if progman.0.is_null() {
            return;
        }
        let insert_after = GetWindow(progman, GW_HWNDPREV).unwrap_or(HWND_TOP);
        let _ = SetWindowPos(
            hwnd,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

/// Returns the material actually applied (fallback table in action).
///
/// Current design: floating glass cards directly on the wallpaper — so the
/// HOST window gets NO backdrop (DWMSBT_NONE); each widget card does its own
/// CSS backdrop-filter glass. Mica/acrylic paths kept for --plain-win A/B.
pub fn apply_material(hwnd: HWND) -> &'static str {
    unsafe {
        // Dark immersive frame: Mica/backdrop follows the dark palette.
        let dark = 1i32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(20), // DWMWA_USE_IMMERSIVE_DARK_MODE
            &dark as *const _ as *const _,
            4,
        );

        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const _,
            4,
        );

        // Explicitly NO system backdrop: gaps between cards must show the
        // real wallpaper, and cards do their own glass via CSS.
        let none = DWMSBT_NONE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &none as *const _ as *const _, 4);

        // 把整个客户区扩展进"玻璃框"：DWM 会把客户区表面当作带 alpha 的玻璃处理，
        // 未涂色处透出后方实时桌面；模糊由 DwmEnableBlurBehindWindow 的区域控制。
        // 没有这一步，未涂色表面是不透明黑，blur-behind 无从生效。
        let glass_all = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &glass_all);
        "glass-none"
    }
}

/// 真透明核心：只在卡片矩形区域开 DWM 实时模糊（blur-behind + HRGN），
/// 卡片缝隙不做任何处理 → 透出的是**活的桌面像素**（后面的窗口动，这里实时跟着动）。
/// rects 为物理像素 (x, y, w, h)；空切片 = 关闭模糊（动画期间/隐藏时用）。
/// DwmEnableBlurBehindWindow 会复制 region，调用后即可 DeleteObject。
pub fn set_blur_regions(hwnd: HWND, rects: &[(i32, i32, i32, i32)], radius: i32) {
    unsafe {
        if rects.is_empty() {
            let bb = DWM_BLURBEHIND {
                dwFlags: DWM_BB_ENABLE,
                fEnable: false.into(),
                hRgnBlur: windows::Win32::Graphics::Gdi::HRGN::default(),
                fTransitionOnMaximized: false.into(),
            };
            let _ = DwmEnableBlurBehindWindow(hwnd, &bb);
            return;
        }
        let (x, y, w, h) = rects[0];
        let acc = CreateRoundRectRgn(x, y, x + w, y + h, radius, radius);
        if acc.is_invalid() {
            return;
        }
        for &(x, y, w, h) in &rects[1..] {
            let rgn = CreateRoundRectRgn(x, y, x + w, y + h, radius, radius);
            if rgn.is_invalid() {
                continue;
            }
            let _ = CombineRgn(acc, acc, rgn, RGN_OR);
            let _ = DeleteObject(rgn);
        }
        let bb = DWM_BLURBEHIND {
            dwFlags: if std::env::var_os("WB_BLUR_FULL").is_some() {
                DWM_BB_ENABLE // A/B：整窗模糊（不限区域）
            } else {
                DWM_BB_ENABLE | DWM_BB_BLURREGION
            },
            fEnable: true.into(),
            hRgnBlur: acc,
            fTransitionOnMaximized: false.into(),
        };
        let _ = DwmEnableBlurBehindWindow(hwnd, &bb);
        let _ = DeleteObject(acc);
    }
}


// ================= 亚克力磨砂池 =================
// 经典 blur-behind 太弱（用户实测"只是半透明"）；整窗亚克力 + SetWindowRgn
// 又被证实accent 渲染忽略区域裁剪（磨砂溢出到缝隙）。最终方案：
// **磨砂池**——一组独立的无激活小窗口，各自开真亚克力，按卡片矩形逐一对位
// （内缩 inset 让方角藏进卡片圆角内），缝隙完全没有窗口 → 透出活桌面。
// accent API 不可用时回退到主窗口的经典区域 blur-behind。

const FROST_POOL_SIZE: usize = 24;
/// 磨砂窗相对卡片矩形内缩的物理像素：让方角藏进卡片 16px 圆角内（150% 下 R=24px，
/// 对角线安全边界 ≈ 7px，取 10 留余量）。
const FROST_INSET: i32 = 10;

static FROST_HWNDS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static FROST_ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    /// ABGR：A<<24 | B<<16 | G<<8 | R
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WcaData {
    attrib: u32,
    pv_data: *mut std::ffi::c_void,
    cb_data: usize,
}

type SetWindowCompositionAttributeFn = unsafe extern "system" fn(HWND, *mut WcaData) -> i32;

fn accent_fn() -> Option<SetWindowCompositionAttributeFn> {
    unsafe {
        let user32 = GetModuleHandleW(w!("user32.dll")).ok()?;
        let p = windows::Win32::System::LibraryLoader::GetProcAddress(
            user32,
            windows::core::s!("SetWindowCompositionAttribute"),
        )?;
        Some(std::mem::transmute(p))
    }
}

fn enable_acrylic(hwnd: HWND) -> bool {
    let Some(f) = accent_fn() else { return false };
    // 深色亚克力：alpha 0x66 + RGB(15,17,26) → ABGR 0x66_1A_11_0F
    let mut policy = AccentPolicy {
        accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        accent_flags: 0,
        gradient_color: 0x661A_110F,
        animation_id: 0,
    };
    let mut data = WcaData {
        attrib: WCA_ACCENT_POLICY,
        pv_data: &mut policy as *mut _ as *mut std::ffi::c_void,
        cb_data: std::mem::size_of::<AccentPolicy>(),
    };
    unsafe { f(hwnd, &mut data) != 0 }
}

unsafe extern "system" fn frost_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    if msg == WM_ERASEBKGND {
        return windows::Win32::Foundation::LRESULT(1);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 启动时创建磨砂池（必须在面板窗口之前，保证面板在 Z 序上方）。
///
/// 池的两种用法（见 veil_show / set_card_regions 文档）：
/// - v11 默认 veil 模式：只用第 1 张窗拉满全屏当磨砂底板，无逐帧操作；
/// - WB_CARD_FROST=1 逐卡模式：24 张窗逐一对位到卡片矩形并逐帧跟随。
/// 亚克力不可用则池为空，veil 回退官方系统 backdrop，逐卡回退经典区域 blur-behind。
pub fn create_frost_pool() {
    unsafe {
        let Some(_) = accent_fn() else { return };
        let hinst = HINSTANCE::from(GetModuleHandleW(None).unwrap_or_default());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(frost_wnd_proc),
            hInstance: hinst,
            lpszClassName: w!("WBPanelFrost"),
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mut pool = FROST_HWNDS.lock().unwrap();
        for _ in 0..FROST_POOL_SIZE {
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("WBPanelFrost"),
                w!("WB Frost"),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                hinst,
                None,
            )
            .unwrap_or_default();
            if hwnd.0.is_null() || !enable_acrylic(hwnd) {
                continue;
            }
            // 注意：不能以 WS_EX_LAYERED 创建——LWA 路径会永久削弱亚克力模糊。
            // 窗初始即隐藏；淡入前先加 layered 皮调 alpha 0，淡出完成后摘掉还原全强度。
            // 圆角（Win11 官方机制，accent 会服从它，与 SetWindowRgn 不同）
            let corner = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner as *const _ as *const _,
                4,
            );
            pool.push(hwnd.0 as usize);
        }
        println!("{}", serde_json::json!({"event":"frost_pool","n":pool.len()}));
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

pub fn frost_pool_len() -> usize {
    FROST_HWNDS.lock().unwrap().len()
}

fn hwnd_of(raw: usize) -> HWND {
    HWND(raw as *mut std::ffi::c_void)
}

/// 与面板同显隐（NOACTIVATE 不抢焦点）。
/// hide：立即全隐藏并重置淡入淡出状态，下次亮窗从 alpha 0 重新淡入。
pub fn frost_show(show: bool) {
    let pool = FROST_HWNDS.lock().unwrap();
    unsafe {
        if !show {
            FROST_ALPHA.store(0, std::sync::atomic::Ordering::SeqCst);
            FROST_TARGET.store(0, std::sync::atomic::Ordering::SeqCst);
            for &w in pool.iter() {
                let w = hwnd_of(w);
                set_layered(w, true); // 重置为"可淡入"状态，下次亮窗从 0 淡入
                let _ = SetLayeredWindowAttributes(w, COLORREF(0), 0, LWA_ALPHA);
                let _ = ShowWindow(w, SW_HIDE);
            }
            return;
        }
        let n = FROST_ACTIVE.load(std::sync::atomic::Ordering::SeqCst);
        let alpha = FROST_ALPHA.load(std::sync::atomic::Ordering::SeqCst);
        if alpha == 0 {
            return; // 全透明时不亮，等矩形流进来走淡入
        }
        for (i, w) in pool.iter().enumerate() {
            let w = hwnd_of(*w);
            if i < n {
                let _ = ShowWindow(w, SW_SHOWNA);
            } else {
                let _ = ShowWindow(w, SW_HIDE);
            }
        }
    }
}

/// 磨砂窗淡入淡出定时器（宿主 UI 线程 WM_TIMER 驱动）。
pub const FROST_FADE_TIMER_ID: usize = 9;
static FROST_ALPHA: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static FROST_TARGET: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn fade_to(panel: HWND, target: u32) {
    use std::sync::atomic::Ordering::SeqCst;
    let prev = FROST_TARGET.swap(target, SeqCst);
    if prev == target || FROST_ALPHA.load(SeqCst) == target {
        return;
    }
    if target == 0 {
        // 落定态的窗是"非 layered 全强度亚克力"，淡出前要先加回 layered 皮
        // （已 layered 的保持当前 alpha，避免亮度跳变）。
        let pool: Vec<HWND> = FROST_HWNDS.lock().unwrap().iter().map(|&r| hwnd_of(r)).collect();
        let n = FROST_ACTIVE.load(SeqCst);
        unsafe {
            for (i, w) in pool.iter().enumerate() {
                if i >= n {
                    break;
                }
                let ex = GetWindowLongPtrW(*w, GWL_EXSTYLE);
                if ex & (WS_EX_LAYERED.0 as isize) == 0 {
                    set_layered(*w, true);
                    let _ = SetLayeredWindowAttributes(*w, COLORREF(0), 255, LWA_ALPHA);
                }
            }
        }
    }
    unsafe {
        let _ = SetTimer(panel, FROST_FADE_TIMER_ID, 16, None);
    }
}

/// 每 16ms 走一步：alpha 以 32 步进逼近目标（≈130ms 完成，与卡片动画同量级）。
/// 淡入完成后摘掉 WS_EX_LAYERED：LWA 路径会削弱亚克力模糊强度，
/// 只有过渡期间才用 layered 淡入淡出，落定后恢复全强度亚克力。
pub fn frost_fade_tick(panel: HWND) {
    use std::sync::atomic::Ordering::SeqCst;
    let alpha = FROST_ALPHA.load(SeqCst);
    let target = FROST_TARGET.load(SeqCst);
    if alpha == target {
        unsafe {
            let _ = KillTimer(panel, FROST_FADE_TIMER_ID);
        }
        return;
    }
    let next = if alpha < target {
        (alpha + 32).min(target)
    } else {
        alpha.saturating_sub(32).max(target)
    };
    FROST_ALPHA.store(next, SeqCst);
    let pool: Vec<HWND> = FROST_HWNDS.lock().unwrap().iter().map(|&r| hwnd_of(r)).collect();
    let n = FROST_ACTIVE.load(SeqCst);
    unsafe {
        for (i, w) in pool.iter().enumerate() {
            if i < n && next > 0 {
                let _ = SetLayeredWindowAttributes(*w, COLORREF(0), next as u8, LWA_ALPHA);
            } else {
                let _ = ShowWindow(*w, SW_HIDE);
            }
        }
        if next == target {
            let _ = KillTimer(panel, FROST_FADE_TIMER_ID);
            if next == 255 {
                for (i, w) in pool.iter().enumerate() {
                    if i < n {
                        set_layered(*w, false); // 落定：恢复全强度亚克力
                    }
                }
            }
        }
    }
}

/// 加/摘 WS_EX_LAYERED（摘 style 需要 FRAMECHANGED 才立即生效）。
fn set_layered(hwnd: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = if on {
            ex | WS_EX_LAYERED.0 as isize
        } else {
            ex & !(WS_EX_LAYERED.0 as isize)
        };
        if want == ex {
            return;
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// v11：整屏磨砂 veil（默认模式）。
/// 用户实测逐卡跟踪在高刷屏上"太卡"——165Hz × 24 个亚克力窗逐帧 SetWindowPos，
/// 每帧都强制亚克力重采样，DWM 合成队列被打满。veil 模式只把池里第一张窗
/// 拉满整个工作区、钉在面板正下方：显示/隐藏各做一次定位 + 一次淡入淡出，
/// 动画期间**零窗口操作**，DWM 合成的始终是同一张静态表面 → 绝对流畅。
/// 缝隙不再透活桌面（用户拍板："底层背景就是覆盖整个桌面的模糊玻璃"）。
/// 设 WB_CARD_FROST=1 切回逐卡跟踪模式做 A/B。
pub fn card_frost_mode() -> bool {
    std::env::var_os("WB_CARD_FROST").is_some_and(|v| v == "1")
}

/// veil 显隐。show：拉满面板矩形、方角、从当前 alpha 淡入；hide：原位淡出
/// （真正的隐藏在 hide_now → frost_show(false) 统一收尾）。
pub fn veil_show(panel: HWND, show: bool) {
    use std::sync::atomic::Ordering::SeqCst;
    let pool: Vec<HWND> = FROST_HWNDS.lock().unwrap().iter().map(|&r| hwnd_of(r)).collect();
    let Some(&veil) = pool.first() else {
        if show {
            // 亚克力不可用：官方系统 backdrop 兜底（整窗亚克力，同样无逐帧操作）
            unsafe {
                let t = DWMSBT_TRANSIENTWINDOW;
                let _ = DwmSetWindowAttribute(
                    panel,
                    DWMWA_SYSTEMBACKDROP_TYPE,
                    &t as *const _ as *const _,
                    4,
                );
            }
        }
        return;
    };
    if show {
        unsafe {
            let flat = 1u32; // DWMWCP_DONOTROUND：全屏 veil 不要圆角
            let _ = DwmSetWindowAttribute(
                veil,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &flat as *const _ as *const _,
                4,
            );
            let mut rc = std::mem::zeroed::<windows::Win32::Foundation::RECT>();
            let _ = GetWindowRect(panel, &mut rc);
            // 先披 layered 皮（对齐当前淡入进度，通常 0）再亮窗，杜绝全强度闪现
            set_layered(veil, true);
            let _ = SetLayeredWindowAttributes(veil, COLORREF(0), FROST_ALPHA.load(SeqCst) as u8, LWA_ALPHA);
            let _ = SetWindowPos(
                veil,
                panel, // 恰好位于面板之下
                rc.left,
                rc.top,
                rc.right - rc.left,
                rc.bottom - rc.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        FROST_ACTIVE.store(1, SeqCst);
        fade_to(panel, 255);
    } else {
        fade_to(panel, 0);
    }
}

/// 把磨砂窗逐一对位到卡片矩形（物理像素，相对工作区原点）。
/// 每个磨砂窗用 SetWindowPos 插到面板正下方，避免盖住 WebView2。
/// 【仅 WB_CARD_FROST=1 逐卡模式使用】veil 模式下页面矩形上报直接忽略。
/// 矩形从有到无 / 从无到有只拨淡入淡出目标，位置由页面的逐帧上报流驱动，
/// 与卡片同帧运动——不再是"第二层背景"，而是组件自己的磨砂底。
pub fn set_card_regions(panel: HWND, rects: &[(i32, i32, i32, i32)], _radius: i32) {
    use std::sync::atomic::Ordering::SeqCst;
    if !card_frost_mode() {
        return; // veil 模式：磨砂与卡片矩形无关，零逐帧操作
    }
    let pool: Vec<HWND> = FROST_HWNDS.lock().unwrap().iter().map(|&r| hwnd_of(r)).collect();
    if pool.is_empty() {
        set_blur_regions(panel, rects, _radius); // 回退：弱模糊（Win11 22H2+ 上约等于无）
        return;
    }
    let n = rects.len().min(pool.len());
    let alpha = FROST_ALPHA.load(SeqCst) as u8;
    for (i, w) in pool.iter().enumerate() {
        unsafe {
            if i < n {
                let (x, y, rw, rh) = rects[i];
                let x = x + FROST_INSET;
                let y = y + FROST_INSET;
                let rw = (rw - FROST_INSET * 2).max(24);
                let rh = (rh - FROST_INSET * 2).max(24);
                // 先定透明状态再亮窗：落定态直接全强度（摘 layered）；
                // 过渡态先披 layered 皮并把 alpha 对齐到当前淡入进度（默认 0，不跳变）。
                if alpha == 255 && FROST_TARGET.load(SeqCst) == 255 {
                    set_layered(*w, false);
                } else {
                    set_layered(*w, true);
                    let _ = SetLayeredWindowAttributes(*w, COLORREF(0), alpha, LWA_ALPHA);
                }
                let _ = SetWindowPos(
                    *w,
                    panel, // 恰好位于面板之下
                    x,
                    y,
                    rw,
                    rh,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            } else if n > 0 {
                let _ = ShowWindow(*w, SW_HIDE);
            }
            // n == 0 时不立刻隐藏：保持原位淡出，和卡片消失动画一起收场
        }
    }
    FROST_ACTIVE.store(n, SeqCst);
    fade_to(panel, if n > 0 { 255 } else { 0 });
}

//! 面板跨进程控制：FindWindow + PostMessage（与 wb-hook 同协议）。
//! 面板没在跑时 show/toggle 会把 wb-panel.exe 拉起来（Agent 可完全脱离人工）。

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsWindowVisible, PostMessageW, WM_CLOSE,
};

/// 与 wb-panel host.rs 保持一致
pub const WM_WB_TOGGLE: u32 = 0x8000 + 41;
pub const WM_WB_SHOW: u32 = 0x8000 + 43;
pub const WM_WB_HIDE: u32 = 0x8000 + 44;
pub const WM_WB_DESKTOP_REFRESH: u32 = 0x8000 + 45;

const PANEL_CLASS: &str = "WBPanelPoc";
const DESKTOP_CLASS: &str = "WBDesktopWidgets";

fn find_window(class: &str) -> Option<HWND> {
    unsafe {
        let class: Vec<u16> = format!("{class}\0").encode_utf16().collect();
        FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null())
            .ok()
            .filter(|h| !h.0.is_null())
    }
}

fn find_panel() -> Option<HWND> {
    find_window(PANEL_CLASS)
}

fn post(hwnd: HWND, msg: u32) {
    unsafe {
        let _ = PostMessageW(hwnd, msg, None, None);
    }
}

fn panel_visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

fn spawn_panel(desktop: bool) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wb-panel.exe")));
    if let Some(exe) = exe {
        if exe.exists() {
            // 必须带 --wv2，否则只建透明空窗（血泪教训）
            let _ = std::process::Command::new(exe)
                .arg("--wv2")
                .args(desktop.then_some("--desktop"))
                .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}

pub fn show() -> serde_json::Value {
    match find_panel() {
        Some(h) => {
            post(h, WM_WB_SHOW);
            serde_json::json!({"panel": "shown"})
        }
        None => {
            spawn_panel(false);
            serde_json::json!({"panel": "started"})
        }
    }
}

pub fn hide() -> serde_json::Value {
    match find_panel() {
        Some(h) if panel_visible(h) => {
            post(h, WM_WB_HIDE);
            serde_json::json!({"panel": "hidden"})
        }
        _ => serde_json::json!({"panel": "already hidden"}),
    }
}

pub fn toggle() -> serde_json::Value {
    match find_panel() {
        Some(h) => {
            post(h, WM_WB_TOGGLE);
            serde_json::json!({"panel": "toggled"})
        }
        None => {
            spawn_panel(false);
            serde_json::json!({"panel": "started"})
        }
    }
}

pub fn close() -> serde_json::Value {
    match find_panel() {
        Some(h) => {
            post(h, WM_CLOSE);
            serde_json::json!({"panel": "closing"})
        }
        None => serde_json::json!({"panel": "not running"}),
    }
}

pub fn desktop_running() -> bool {
    find_window(DESKTOP_CLASS).is_some()
}

pub fn sync_desktop(enabled: bool) -> serde_json::Value {
    match (enabled, find_window(DESKTOP_CLASS)) {
        (true, Some(hwnd)) => {
            post(hwnd, WM_WB_DESKTOP_REFRESH);
            serde_json::json!({"desktop": "refreshed"})
        }
        (true, None) => {
            spawn_panel(true);
            serde_json::json!({"desktop": "started"})
        }
        (false, Some(hwnd)) => {
            post(hwnd, WM_CLOSE);
            serde_json::json!({"desktop": "closing"})
        }
        (false, None) => serde_json::json!({"desktop": "stopped"}),
    }
}

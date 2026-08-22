use interprocess::local_socket::{prelude::*, GenericNamespaced};
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_WB_TRAY: u32 = WM_APP + 90;
const TRAY_ID: u32 = 1;
const CMD_OPEN: usize = 1001;
const CMD_EXIT: usize = 1002;
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

pub fn start() {
    std::thread::spawn(|| {
        if let Err(e) = run() {
            eprintln!("wb-daemon: tray unavailable: {e}");
        }
    });
}

fn run() -> Result<(), String> {
    unsafe {
        let hinst = HINSTANCE::from(GetModuleHandleW(None).map_err(|e| e.to_string())?);
        let class_name = w!("WBTrayWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("WB"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            hinst,
            None,
        )
        .map_err(|e| e.to_string())?;

        let mut icon = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_WB_TRAY,
            hIcon: LoadIconW(HINSTANCE::default(), IDI_APPLICATION)
                .map_err(|e| e.to_string())?,
            ..Default::default()
        };
        set_wide(&mut icon.szTip, "WB - Agent-Native Desktop");
        if !Shell_NotifyIconW(NIM_ADD, &icon).as_bool() {
            return Err("Shell_NotifyIconW(NIM_ADD) failed".into());
        }
        TRAY_HWND.store(hwnd.0 as isize, Ordering::Release);
        eprintln!("wb-daemon: tray ready");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon);
        TRAY_HWND.store(0, Ordering::Release);
    }
    Ok(())
}

pub fn remove() {
    let raw = TRAY_HWND.swap(0, Ordering::AcqRel);
    if raw == 0 {
        return;
    }
    let icon = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(raw as *mut _),
        uID: TRAY_ID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon);
    }
}

fn set_wide<const N: usize>(target: &mut [u16; N], value: &str) {
    for (dst, src) in target
        .iter_mut()
        .zip(value.encode_utf16().chain(std::iter::once(0)))
    {
        *dst = src;
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_WB_TRAY => {
            match lparam.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    super::panelctl::show();
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xffff {
                CMD_OPEN => {
                    super::panelctl::show();
                }
                CMD_EXIT => {
                    std::thread::spawn(request_daemon_stop);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("打开 WB"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("退出 WB"));
    let mut point = POINT::default();
    let _ = GetCursorPos(&mut point);
    let _ = SetForegroundWindow(hwnd);
    let selected = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        0,
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);
    if selected.0 != 0 {
        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(selected.0 as usize), LPARAM(0));
    }
}

fn request_daemon_stop() {
    let Ok(name) = wb_core::paths::pipe_name().to_ns_name::<GenericNamespaced>() else {
        return;
    };
    let Ok(mut stream) = interprocess::local_socket::Stream::connect(name) else {
        return;
    };
    let request = serde_json::json!({
        "jsonrpc":"2.0",
        "id":"tray-exit",
        "method":"daemon.stop",
        "params":{}
    });
    let _ = writeln!(stream, "{request}");
    let _ = stream.flush();
    let mut response = String::new();
    let _ = BufReader::new(stream).read_line(&mut response);
}

//! Clipboard live capture: hidden message-only window + WM_CLIPBOARDUPDATE.
//! Text/files/image entries land in the clips table; deduped by content.

use std::sync::Arc;
use wb_core::models::{new_id, ClipEntry, ClipKind};
use wb_core::storage::Storage;
use windows::core::w;
use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, GetClipboardData, IsClipboardFormatAvailable,
    OpenClipboard,
};

const CF_UNICODETEXT: u32 = 13;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::*;

const MAX_CHARS: usize = 100_000;

struct Ctx {
    storage: Arc<Storage>,
    last: std::sync::Mutex<String>,
}

pub fn start(storage: Arc<Storage>) {
    std::thread::spawn(move || unsafe {
        let hinst = windows::Win32::Foundation::HINSTANCE::from(GetModuleHandleW(None).unwrap_or_default());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: w!("WBClipListener"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("WBClipListener"),
            w!("wb-clip"),
            WINDOW_STYLE::default(),
            0, 0, 0, 0,
            HWND_MESSAGE,
            HMENU::default(),
            hinst,
            None,
        )
        .expect("clip listener window");
        // ctx pointer via GWLP_USERDATA
        let ctx = Box::leak(Box::new(Ctx { storage, last: std::sync::Mutex::new(String::new()) }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as *const Ctx as isize);
        let _ = AddClipboardFormatListener(hwnd);
        eprintln!("wb-daemon: clipboard listener up");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        let ctx_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Ctx;
        if let Some(ctx) = ctx_ptr.as_ref() {
            capture(ctx);
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe fn capture(ctx: &Ctx) {
    if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
        return; // images/files land here in M1.5+
    }
    if OpenClipboard(None).is_err() {
        return;
    }
    let text = (|| {
        let h = GetClipboardData(CF_UNICODETEXT).ok()?;
        let hglobal = HGLOBAL(h.0);
        let size = GlobalSize(hglobal);
        if size == 0 {
            return None;
        }
        let p = GlobalLock(hglobal);
        if p.is_null() {
            return None;
        }
        let wide = std::slice::from_raw_parts(p as *const u16, size / 2);
        let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        let s = String::from_utf16_lossy(&wide[..end]);
        let _ = GlobalUnlock(hglobal);
        Some(s)
    })();
    let _ = CloseClipboard();

    let Some(text) = text else { return };
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_CHARS {
        return;
    }
    {
        let mut last = ctx.last.lock().unwrap();
        if *last == text {
            return; // dedupe repeat copies
        }
        *last = text.clone();
    }
    let entry = ClipEntry {
        id: new_id(),
        kind: ClipKind::Text,
        content: text,
        created_at: chrono::Utc::now(),
    };
    let _ = ctx.storage.clip_add(&entry);
}

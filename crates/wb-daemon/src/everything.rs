//! Everything 1.4/1.5 IPC provider using the official Unicode v1 query.

use std::cell::RefCell;
use std::time::{Duration, Instant};
use wb_core::models::{ResultKind, SearchResult};
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const EVERYTHING_CLASS: windows::core::PCWSTR = w!("EVERYTHING_TASKBAR_NOTIFICATION");
const REPLY_CLASS: windows::core::PCWSTR = w!("WBEverythingReplyWindow");
const EVERYTHING_WM_IPC: u32 = WM_USER;
const EVERYTHING_IPC_IS_DB_LOADED: usize = 401;
const EVERYTHING_IPC_COPYDATAQUERYW: usize = 2;
const EVERYTHING_IPC_MATCHPATH: u32 = 0x0000_0004;
const REPLY_COPYDATA_ID: usize = 0x5742_4556;
const QUERY_HEADER_BYTES: usize = 20;
const LIST_HEADER_BYTES: usize = 28;
const ITEM_BYTES: usize = 12;
const MAX_RESULTS: usize = 200;
const MAX_QUERY_UNITS: usize = 4096;
const MAX_REPLY_BYTES: usize = 32 * 1024 * 1024;
const IPC_TIMEOUT: Duration = Duration::from_millis(1500);

thread_local! {
    static REPLY: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

pub fn available() -> bool {
    everything_window().is_ok()
}

pub fn database_loaded() -> bool {
    let Ok(everything) = everything_window() else {
        return false;
    };
    let mut loaded = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            everything,
            EVERYTHING_WM_IPC,
            WPARAM(EVERYTHING_IPC_IS_DB_LOADED),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            IPC_TIMEOUT.as_millis() as u32,
            Some(&mut loaded),
        )
    };
    sent.0 != 0 && loaded != 0
}

pub fn search(query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    if query.encode_utf16().count() > MAX_QUERY_UNITS || query.contains('\0') {
        return Err("Everything query is invalid or too long".into());
    }
    let limit = limit.clamp(1, MAX_RESULTS);
    let everything = everything_window()?;
    if !database_loaded() {
        return Err("Everything database is not loaded".into());
    }
    let reply = create_reply_window()?;
    REPLY.with(|slot| *slot.borrow_mut() = None);

    let query_data = encode_query(reply, query, limit);
    let copydata = COPYDATASTRUCT {
        dwData: EVERYTHING_IPC_COPYDATAQUERYW,
        cbData: query_data.len() as u32,
        lpData: query_data.as_ptr() as *mut _,
    };
    let mut message_result = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            everything,
            WM_COPYDATA,
            WPARAM(reply.0 as usize),
            LPARAM(&copydata as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            IPC_TIMEOUT.as_millis() as u32,
            Some(&mut message_result),
        )
    };
    if sent.0 == 0 || message_result == 0 {
        unsafe {
            let _ = DestroyWindow(reply);
        }
        return Err("Everything rejected or timed out sending the IPC query".into());
    }

    let deadline = Instant::now() + IPC_TIMEOUT;
    let bytes = loop {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some(bytes) = REPLY.with(|slot| slot.borrow_mut().take()) {
            break bytes;
        }
        if Instant::now() >= deadline {
            unsafe {
                let _ = DestroyWindow(reply);
            }
            return Err("Everything IPC reply timed out".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    unsafe {
        let _ = DestroyWindow(reply);
    }
    parse_reply(&bytes, query, limit)
}

fn everything_window() -> Result<HWND, String> {
    unsafe { FindWindowW(EVERYTHING_CLASS, None) }
        .ok()
        .filter(|hwnd| !hwnd.0.is_null())
        .ok_or_else(|| "Everything is not running".to_string())
}

fn create_reply_window() -> Result<HWND, String> {
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| unsafe {
            let hinstance = HINSTANCE::from(GetModuleHandleW(None).map_err(|e| e.to_string())?);
            let class = WNDCLASSW {
                lpfnWndProc: Some(reply_wnd_proc),
                hInstance: hinstance,
                lpszClassName: REPLY_CLASS,
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err("RegisterClassW(WBEverythingReplyWindow) failed".into());
            }
            Ok(())
        })
        .clone()?;
    unsafe {
        let hinstance = HINSTANCE::from(GetModuleHandleW(None).map_err(|e| e.to_string())?);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            REPLY_CLASS,
            w!("wb-everything-reply"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            hinstance,
            None,
        )
        .map_err(|e| e.to_string())?;
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
        Ok(hwnd)
    }
}

unsafe extern "system" fn reply_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_COPYDATA && lparam.0 != 0 {
        let data = &*(lparam.0 as *const COPYDATASTRUCT);
        let len = data.cbData as usize;
        if data.dwData == REPLY_COPYDATA_ID
            && !data.lpData.is_null()
            && (LIST_HEADER_BYTES..=MAX_REPLY_BYTES).contains(&len)
        {
            let bytes = std::slice::from_raw_parts(data.lpData as *const u8, len).to_vec();
            REPLY.with(|slot| *slot.borrow_mut() = Some(bytes));
            return LRESULT(1);
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn encode_query(reply: HWND, query: &str, limit: usize) -> Vec<u8> {
    let wide: Vec<u16> = query.encode_utf16().chain(std::iter::once(0)).collect();
    let mut out = Vec::with_capacity(QUERY_HEADER_BYTES + wide.len() * 2);
    for value in [
        reply.0 as usize as u32,
        REPLY_COPYDATA_ID as u32,
        EVERYTHING_IPC_MATCHPATH,
        0,
        limit.clamp(1, MAX_RESULTS) as u32,
    ] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for unit in wide {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn parse_reply(bytes: &[u8], query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
    if bytes.len() < LIST_HEADER_BYTES {
        return Err("Everything reply is shorter than the list header".into());
    }
    let count = read_u32(bytes, 20)? as usize;
    let item_end = LIST_HEADER_BYTES
        .checked_add(
            count
                .checked_mul(ITEM_BYTES)
                .ok_or("Everything item count overflow")?,
        )
        .ok_or("Everything item table overflow")?;
    if count > MAX_RESULTS || item_end > bytes.len() {
        return Err("Everything reply has an invalid item table".into());
    }
    let q = query.to_lowercase();
    let mut results = Vec::with_capacity(count.min(limit));
    for index in 0..count {
        let item = LIST_HEADER_BYTES + index * ITEM_BYTES;
        let filename_offset = read_u32(bytes, item + 4)? as usize;
        let path_offset = read_u32(bytes, item + 8)? as usize;
        if filename_offset < item_end || path_offset < item_end {
            return Err("Everything reply string overlaps its item table".into());
        }
        let filename = read_utf16z(bytes, filename_offset)?;
        let parent = read_utf16z(bytes, path_offset)?;
        if filename.is_empty() {
            continue;
        }
        let full_path = if parent.is_empty() {
            filename.clone()
        } else if parent.ends_with(['\\', '/']) {
            format!("{parent}{filename}")
        } else {
            format!("{parent}\\{filename}")
        };
        let title = filename.to_lowercase();
        results.push(SearchResult {
            kind: ResultKind::File,
            title: filename,
            subtitle: (!parent.is_empty()).then_some(parent),
            preview: None,
            path: Some(full_path),
            score: if title.starts_with(&q) { 0.84 } else { 0.72 },
            source: "everything".into(),
        });
        if results.len() >= limit {
            break;
        }
    }
    Ok(results)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "Everything reply contains a truncated integer".to_string())?;
    Ok(u32::from_le_bytes(raw.try_into().unwrap()))
}

fn read_utf16z(bytes: &[u8], offset: usize) -> Result<String, String> {
    if !offset.is_multiple_of(2) || offset >= bytes.len() {
        return Err("Everything reply contains an invalid string offset".into());
    }
    let mut units = Vec::new();
    let mut cursor = offset;
    loop {
        let raw = bytes
            .get(cursor..cursor + 2)
            .ok_or_else(|| "Everything reply contains an unterminated string".to_string())?;
        let unit = u16::from_le_bytes(raw.try_into().unwrap());
        if unit == 0 {
            break;
        }
        units.push(unit);
        cursor += 2;
    }
    String::from_utf16(&units).map_err(|_| "Everything reply contains invalid UTF-16".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_utf16z(bytes: &mut Vec<u8>, text: &str) -> u32 {
        let offset = bytes.len() as u32;
        for unit in text.encode_utf16().chain(std::iter::once(0)) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        offset
    }

    #[test]
    fn encodes_official_unicode_v1_query_layout() {
        let hwnd = HWND(0x1234usize as *mut _);
        let data = encode_query(hwnd, "周报", 25);
        assert_eq!(read_u32(&data, 0).unwrap(), 0x1234);
        assert_eq!(read_u32(&data, 4).unwrap(), REPLY_COPYDATA_ID as u32);
        assert_eq!(read_u32(&data, 8).unwrap(), EVERYTHING_IPC_MATCHPATH);
        assert_eq!(read_u32(&data, 16).unwrap(), 25);
        assert_eq!(read_utf16z(&data, QUERY_HEADER_BYTES).unwrap(), "周报");
    }

    #[test]
    fn parses_bounded_unicode_results() {
        let mut bytes = vec![0u8; LIST_HEADER_BYTES + ITEM_BYTES * 2];
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&2u32.to_le_bytes());
        let first_name = push_utf16z(&mut bytes, "quarterly-report.txt");
        let first_path = push_utf16z(&mut bytes, r"E:\docs");
        let second_name = push_utf16z(&mut bytes, "周报.md");
        let second_path = push_utf16z(&mut bytes, "C:");
        for (index, name, path) in [(0, first_name, first_path), (1, second_name, second_path)] {
            let base = LIST_HEADER_BYTES + index * ITEM_BYTES;
            bytes[base + 4..base + 8].copy_from_slice(&name.to_le_bytes());
            bytes[base + 8..base + 12].copy_from_slice(&path.to_le_bytes());
        }
        let results = parse_reply(&bytes, "周报", 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].path.as_deref(),
            Some(r"E:\docs\quarterly-report.txt")
        );
        assert_eq!(results[1].path.as_deref(), Some(r"C:\周报.md"));
        assert_eq!(results[1].source, "everything");
        assert_eq!(results[1].score, 0.84);
    }

    #[test]
    fn rejects_invalid_reply_offsets_and_counts() {
        let mut bytes = vec![0u8; LIST_HEADER_BYTES + ITEM_BYTES];
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[LIST_HEADER_BYTES + 4..LIST_HEADER_BYTES + 8]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_reply(&bytes, "x", 10).is_err());
        bytes[20..24].copy_from_slice(&201u32.to_le_bytes());
        assert!(parse_reply(&bytes, "x", 10).is_err());
    }
}

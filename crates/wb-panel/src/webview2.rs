//! PoC 2b: embed WebView2 with a fully transparent background on the DWM
//! backdrop window. Manual minimal COM bindings — vtable layouts and IIDs
//! verified against WebView2.h (SDK 1.0.3179.45). Loader DLL is loaded
//! dynamically so no import libs / MSVC toolchain is needed.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use windows::core::{GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, MSG, PM_REMOVE};

use crate::{WINDOW_H, WINDOW_W};

// ---- IIDs (from WebView2.h) ----
const IID_ENV: GUID = GUID::from_u128(0xb96d755e_0319_4e92_a296_2343_6f46a1fc);
const IID_CONTROLLER2: GUID = GUID::from_u128(0xc979903e_d4ca_4228_92eb_47ee_3fa96eab);
const IID_ENV_COMPLETED: GUID = GUID::from_u128(0x4e8a3389_c9d8_4bd2_b6b5_124f_ee6c_c14d);
const IID_CONTROLLER_COMPLETED: GUID = GUID::from_u128(0x6c4819f3_c9b7_4260_8127_c9f5_bde7_f68c);
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_0000_0000_0046);

type HResultFn = HRESULT;

// ---- callback object plumbing ----

#[repr(C)]
struct Handler {
    vtable: *const HandlerVtbl,
    iid: GUID,
    refs: u32,
    slot: usize, // 0 = env slot, 1 = controller slot
}

type InvokeFn = unsafe extern "system" fn(*mut Handler, HRESULT, *mut c_void) -> HRESULT;

#[repr(C)]
struct HandlerVtbl {
    query_interface: unsafe extern "system" fn(*mut Handler, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut Handler) -> u32,
    release: unsafe extern "system" fn(*mut Handler) -> u32,
    invoke: InvokeFn,
}

static ENV_PTR: AtomicUsize = AtomicUsize::new(0);
static CTRL_PTR: AtomicUsize = AtomicUsize::new(0);
static ENV_TID: AtomicUsize = AtomicUsize::new(0);
static HWND_SLOT: AtomicUsize = AtomicUsize::new(0);
static CREATE_CTRL_HR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static ENV_READY: AtomicBool = AtomicBool::new(false);
static CTRL_READY: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn h_query_interface(this: *mut Handler, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    let iid = &*riid;
    if *iid == IID_IUNKNOWN || *iid == (*this).iid {
        *out = this as *mut c_void;
        ((*(*this).vtable).add_ref)(this);
        HRESULT(0)
    } else {
        *out = std::ptr::null_mut();
        HRESULT(0x80004002u32 as i32) // E_NOINTERFACE
    }
}
unsafe extern "system" fn h_add_ref(this: *mut Handler) -> u32 {
    (*this).refs += 1;
    (*this).refs
}
unsafe extern "system" fn h_release(this: *mut Handler) -> u32 {
    (*this).refs = (*this).refs.saturating_sub(1);
    (*this).refs
}
unsafe extern "system" fn h_invoke(this: *mut Handler, hr: HRESULT, ptr: *mut c_void) -> HRESULT {
    if hr.is_err() || ptr.is_null() {
        ENV_READY.store(true, Ordering::SeqCst); // unblock pump; caller checks ptr
        CTRL_READY.store(true, Ordering::SeqCst);
        return HRESULT(0);
    }
    if (*this).slot == 0 {
        // Canonical pattern: create the controller inside the env-completed
        // callback — the exact context the runtime considers "its" thread.
        ENV_PTR.store(ptr as usize, Ordering::SeqCst);
        ENV_TID.store(GetCurrentThreadId() as usize, Ordering::SeqCst);
        let hwnd = HWND(HWND_SLOT.load(Ordering::SeqCst) as *mut c_void);
        let ctrl_handler = make_handler(IID_CONTROLLER_COMPLETED, 1);
        let vt = *(ptr as *const *const EnvVtbl);
        let c_hr = ((*vt).create_controller)(ptr, hwnd, ctrl_handler);
        CREATE_CTRL_HR.store(c_hr.0 as u32, Ordering::SeqCst);
        ENV_READY.store(true, Ordering::SeqCst);
    } else {
        // Take our own reference immediately — after Invoke returns the
        // runtime drops its ref and a raw stored pointer would dangle.
        let vt = *(ptr as *const *const IUnknownOnlyVtbl);
        ((*vt).add_ref)(ptr);
        CTRL_PTR.store(ptr as usize, Ordering::SeqCst);
        CTRL_READY.store(true, Ordering::SeqCst);
    }
    HRESULT(0)
}

fn make_handler(iid: GUID, slot: usize) -> *mut Handler {
    let vt: &'static HandlerVtbl = Box::leak(Box::new(HandlerVtbl {
        query_interface: h_query_interface,
        add_ref: h_add_ref,
        release: h_release,
        invoke: h_invoke,
    }));
    Box::into_raw(Box::new(Handler { vtable: vt, iid, refs: 1, slot }))
}

// ---- NavigationCompleted handler (dedicated: different Invoke signature) ----

#[repr(C)]
struct NavHandler {
    vtable: *const NavHandlerVtbl,
    refs: u32,
}

#[repr(C)]
struct NavHandlerVtbl {
    query_interface: unsafe extern "system" fn(*mut NavHandler, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut NavHandler) -> u32,
    release: unsafe extern "system" fn(*mut NavHandler) -> u32,
    invoke: unsafe extern "system" fn(*mut NavHandler, *mut c_void, *mut c_void) -> HRESULT,
}

// IID_ICoreWebView2NavigationCompletedEventHandler (WebView2.h 1.0.3179.45)
const IID_NAV_COMPLETED: GUID = GUID::from_u128(0xd33a35bf_1c49_4f98_93ab_006e_0533_fe1c);

#[repr(C)]
struct NavCompletedArgsVtbl {
    qi: QiFn,
    add_ref: AddRefFn,
    release: ReleaseFn,
    get_is_success: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    get_web_error_status: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
}

unsafe extern "system" fn n_qi(this: *mut NavHandler, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if *riid == IID_IUNKNOWN || *riid == nav_handler_iid() {
        *out = this as *mut c_void;
        ((*(*this).vtable).add_ref)(this);
        HRESULT(0)
    } else {
        *out = std::ptr::null_mut();
        HRESULT(0x80004002u32 as i32)
    }
}
unsafe extern "system" fn n_add_ref(this: *mut NavHandler) -> u32 {
    (*this).refs += 1;
    (*this).refs
}
unsafe extern "system" fn n_release(this: *mut NavHandler) -> u32 {
    (*this).refs = (*this).refs.saturating_sub(1);
    (*this).refs
}
unsafe extern "system" fn n_invoke(_this: *mut NavHandler, _sender: *mut c_void, args: *mut c_void) -> HRESULT {
    let mut ok: i32 = -1;
    let mut status: i32 = -1;
    if !args.is_null() {
        let vt = *(args as *const *const NavCompletedArgsVtbl);
        let _ = ((*vt).get_is_success)(args, &mut ok);
        let _ = ((*vt).get_web_error_status)(args, &mut status);
    }
    println!("{}", serde_json::json!({"event":"navigation_completed","is_success": ok, "web_error_status": status}));
    let _ = std::io::Write::flush(&mut std::io::stdout());
    HRESULT(0)
}

fn nav_handler_iid() -> GUID {
    IID_NAV_COMPLETED
}

fn make_nav_handler() -> *mut NavHandler {
    let vt: &'static NavHandlerVtbl = Box::leak(Box::new(NavHandlerVtbl {
        query_interface: n_qi,
        add_ref: n_add_ref,
        release: n_release,
        invoke: n_invoke,
    }));
    Box::into_raw(Box::new(NavHandler { vtable: vt, refs: 1 }))
}

// ---- WebMessageReceived handler: page → host channel ----

const IID_WM_HANDLER: GUID = GUID::from_u128(0x57213f19_00e6_49fa_8e07_898e_a01e_cbd2);

#[repr(C)]
pub struct WmHandler {
    vtable: *const WmHandlerVtbl,
    refs: u32,
}

#[repr(C)]
struct WmHandlerVtbl {
    query_interface: unsafe extern "system" fn(*mut WmHandler, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut WmHandler) -> u32,
    release: unsafe extern "system" fn(*mut WmHandler) -> u32,
    invoke: unsafe extern "system" fn(*mut WmHandler, *mut c_void, *mut c_void) -> HRESULT,
}

// ICoreWebView2WebMessageReceivedEventArgs: get_Source(0), get_WebMessageAsJson(1), TryGetWebMessageAsString(2)
#[repr(C)]
struct WmArgsVtbl {
    qi: QiFn,
    add_ref: AddRefFn,
    release: ReleaseFn,
    get_source: OpaqueFn,
    get_web_message_as_json: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HRESULT,
}

unsafe extern "system" fn wm_qi(this: *mut WmHandler, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if *riid == IID_IUNKNOWN || *riid == IID_WM_HANDLER {
        *out = this as *mut c_void;
        ((*(*this).vtable).add_ref)(this);
        HRESULT(0)
    } else {
        *out = std::ptr::null_mut();
        HRESULT(0x80004002u32 as i32)
    }
}
unsafe extern "system" fn wm_add_ref(this: *mut WmHandler) -> u32 {
    (*this).refs += 1;
    (*this).refs
}
unsafe extern "system" fn wm_release(this: *mut WmHandler) -> u32 {
    (*this).refs = (*this).refs.saturating_sub(1);
    (*this).refs
}

unsafe extern "system" fn wm_invoke(_this: *mut WmHandler, _sender: *mut c_void, args: *mut c_void) -> HRESULT {
    if args.is_null() {
        return HRESULT(0);
    }
    let vt = *(args as *const *const WmArgsVtbl);
    let mut p: *mut u16 = std::ptr::null_mut();
    if (((*vt).get_web_message_as_json)(args, &mut p)).is_err() || p.is_null() {
        return HRESULT(0);
    }
    let len = (0..).take_while(|&i| *p.add(i) != 0).count();
    let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
    windows::Win32::System::Com::CoTaskMemFree(Some(p as *const c_void));
    crate::host::on_web_message(&text);
    HRESULT(0)
}

fn make_wm_handler() -> *mut WmHandler {
    let vt: &'static WmHandlerVtbl = Box::leak(Box::new(WmHandlerVtbl {
        query_interface: wm_qi,
        add_ref: wm_add_ref,
        release: wm_release,
        invoke: wm_invoke,
    }));
    Box::into_raw(Box::new(WmHandler { vtable: vt, refs: 1 }))
}

/// True once the WebView2 control exists and post_json can reach the page.
pub fn is_ready() -> bool {
    CTRL_READY.load(Ordering::SeqCst) && !(WV_PTR.load(Ordering::SeqCst) as *mut c_void).is_null()
}

/// Push a JSON message to the page. MUST be called on the WebView2 (UI) thread.
pub fn post_json(msg: &serde_json::Value) {
    let wv = WV_PTR.load(Ordering::SeqCst) as *mut c_void;
    if wv.is_null() {
        return;
    }
    unsafe {
        let wv_vt = *(wv as *const *const WebView2Vtbl);
        let s = wide(&msg.to_string());
        let _ = ((*wv_vt).post_web_message_as_json)(wv, PCWSTR(s.as_ptr()));
    }
}



type QiFn = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
type AddRefFn = unsafe extern "system" fn(*mut c_void) -> u32;
type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;

#[repr(C)]
struct IUnknownOnlyVtbl {
    qi: QiFn,
    add_ref: AddRefFn,
    release: ReleaseFn,
}

#[repr(C)]
struct EnvVtbl {
    qi: QiFn,
    add_ref: AddRefFn,
    release: ReleaseFn,
    create_controller: unsafe extern "system" fn(*mut c_void, HWND, *mut Handler) -> HRESULT,
}

#[repr(C)]
struct WebView2Vtbl {
    qi: QiFn,
    add_ref: AddRefFn,
    release: ReleaseFn,
    get_settings: OpaqueFn,  // [propget] — was missed by regex extraction, caused off-by-2
    get_source: OpaqueFn,    // [propget]
    navigate: unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT,
    navigate_to_string: unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT,
    add_navigation_starting: OpaqueFn,
    remove_navigation_starting: OpaqueFn,
    add_content_loading: OpaqueFn,
    remove_content_loading: OpaqueFn,
    add_source_changed: OpaqueFn,
    remove_source_changed: OpaqueFn,
    add_history_changed: OpaqueFn,
    remove_history_changed: OpaqueFn,
    add_navigation_completed: unsafe extern "system" fn(*mut c_void, *mut NavHandler, *mut u64) -> HRESULT,
    remove_navigation_completed: OpaqueFn,      // 13
    add_frame_nav_starting: OpaqueFn,           // 14
    remove_frame_nav_starting: OpaqueFn,        // 15
    add_frame_nav_completed: OpaqueFn,          // 16
    remove_frame_nav_completed: OpaqueFn,       // 17
    add_script_dialog: OpaqueFn,                // 18
    remove_script_dialog: OpaqueFn,             // 19
    add_permission_req: OpaqueFn,               // 20
    remove_permission_req: OpaqueFn,            // 21
    add_process_failed: OpaqueFn,               // 22
    remove_process_failed: OpaqueFn,            // 23
    add_script_on_doc_created: OpaqueFn,        // 24
    remove_script_on_doc_created: OpaqueFn,     // 25
    execute_script: OpaqueFn,                   // 26
    capture_preview: OpaqueFn,                  // 27
    reload: OpaqueFn,                           // 28
    post_web_message_as_json: unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT, // 29
    post_web_message_as_string: OpaqueFn,       // 30
    add_web_message_received: unsafe extern "system" fn(*mut c_void, *mut WmHandler, *mut u64) -> HRESULT, // 31
}

// ICoreWebView2Controller: 23 methods after IUnknown (order per WebView2.h).
type OpaqueFn = usize;

// ---- EnvironmentOptions implementation (we own this object) ----

#[repr(C)]
struct EnvOptions {
    vtable: *const EnvOptionsVtbl,
    refs: u32,
    args: Vec<u16>, // UTF-16, null-terminated
}

#[repr(C)]
struct EnvOptionsVtbl {
    query_interface: unsafe extern "system" fn(*mut EnvOptions, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut EnvOptions) -> u32,
    release: unsafe extern "system" fn(*mut EnvOptions) -> u32,
    get_args: unsafe extern "system" fn(*mut EnvOptions, *mut *mut u16) -> HRESULT,
    put_args: OpaqueFn,
    get_language: unsafe extern "system" fn(*mut EnvOptions, *mut *mut u16) -> HRESULT,
    put_language: OpaqueFn,
    get_target_version: unsafe extern "system" fn(*mut EnvOptions, *mut *mut u16) -> HRESULT,
    put_target_version: OpaqueFn,
    get_sso: unsafe extern "system" fn(*mut EnvOptions, *mut i32) -> HRESULT,
    put_sso: OpaqueFn,
}

const IID_ENV_OPTIONS: GUID = GUID::from_u128(0x2fde08a8_1e9a_4766_8c05_95a9_ceb9_d1c5);

unsafe extern "system" fn o_qi(this: *mut EnvOptions, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if *riid == IID_IUNKNOWN || *riid == IID_ENV_OPTIONS {
        *out = this as *mut c_void;
        ((*(*this).vtable).add_ref)(this);
        HRESULT(0)
    } else {
        *out = std::ptr::null_mut();
        HRESULT(0x80004002u32 as i32)
    }
}
unsafe extern "system" fn o_add_ref(this: *mut EnvOptions) -> u32 {
    (*this).refs += 1;
    (*this).refs
}
unsafe extern "system" fn o_release(this: *mut EnvOptions) -> u32 {
    (*this).refs = (*this).refs.saturating_sub(1);
    (*this).refs
}

unsafe fn copy_cotaskmem(s: &[u16]) -> *mut u16 {
    let bytes = s.len() * 2;
    let p = windows::Win32::System::Com::CoTaskMemAlloc(bytes) as *mut u16;
    if !p.is_null() {
        std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
    }
    p
}

unsafe extern "system" fn o_get_args(this: *mut EnvOptions, out: *mut *mut u16) -> HRESULT {
    *out = copy_cotaskmem(&(*this).args);
    HRESULT(0)
}
unsafe extern "system" fn o_get_language(_this: *mut EnvOptions, out: *mut *mut u16) -> HRESULT {
    *out = copy_cotaskmem(&[0]);
    HRESULT(0)
}
unsafe extern "system" fn o_get_target_version(_this: *mut EnvOptions, out: *mut *mut u16) -> HRESULT {
    *out = copy_cotaskmem(&[0]);
    HRESULT(0)
}
unsafe extern "system" fn o_get_sso(_this: *mut EnvOptions, out: *mut i32) -> HRESULT {
    *out = 0;
    HRESULT(0)
}

fn make_env_options(args: &str) -> *mut EnvOptions {
    let vt: &'static EnvOptionsVtbl = Box::leak(Box::new(EnvOptionsVtbl {
        query_interface: o_qi,
        add_ref: o_add_ref,
        release: o_release,
        get_args: o_get_args,
        put_args: 0,
        get_language: o_get_language,
        put_language: 0,
        get_target_version: o_get_target_version,
        put_target_version: 0,
        get_sso: o_get_sso,
        put_sso: 0,
    }));
    Box::into_raw(Box::new(EnvOptions { vtable: vt, refs: 1, args: wide(args) }))
}
#[repr(C)]
struct Controller2Vtbl {
    qi: QiFn,
    add_ref: AddRefFn,
    release: ReleaseFn,
    get_is_visible: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT, // 0
    put_is_visible: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT, // 1
    get_bounds: unsafe extern "system" fn(*mut c_void, *mut RECT) -> HRESULT, // 2
    put_bounds: unsafe extern "system" fn(*mut c_void, RECT) -> HRESULT,    // 3
    get_zoom_factor: OpaqueFn,           // 4
    put_zoom_factor: OpaqueFn,           // 5
    add_zoom_changed: OpaqueFn,          // 6
    remove_zoom_changed: OpaqueFn,       // 7
    set_bounds_and_zoom: OpaqueFn,       // 8
    move_focus: OpaqueFn,                // 9
    add_move_focus_req: OpaqueFn,        // 10
    remove_move_focus_req: OpaqueFn,     // 11
    add_got_focus: OpaqueFn,             // 12
    remove_got_focus: OpaqueFn,          // 13
    add_lost_focus: OpaqueFn,            // 14
    remove_lost_focus: OpaqueFn,         // 15
    add_accel_key: OpaqueFn,             // 16
    remove_accel_key: OpaqueFn,          // 17
    get_parent_window: OpaqueFn,         // 18
    put_parent_window: OpaqueFn,         // 19
    notify_parent_pos: OpaqueFn,         // 20
    close: OpaqueFn,                     // 21
    get_core_webview2: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT, // 22
    // ICoreWebView2Controller2:
    get_bg_color: OpaqueFn,              // 23
    put_bg_color: unsafe extern "system" fn(*mut c_void, CoreWebView2Color) -> HRESULT, // 24
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CoreWebView2Color {
    a: u8,
    r: u8,
    g: u8,
    b: u8,
}

fn pump_until(flag: &AtomicBool, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    unsafe {
        let mut msg = MSG::default();
        while !flag.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    flag.load(Ordering::SeqCst)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Post-show diagnostics: dump the child window tree (visibility must be
/// evaluated after the parent is shown).
pub fn diag(hwnd: HWND) {
    unsafe extern "system" fn enum_cb(child: HWND, out: LPARAM) -> windows::Win32::Foundation::BOOL {
        let mut cls = [0u16; 64];
        let n = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(child, &mut cls);
        let name = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
        let mut r = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(child, &mut r);
        let visible = windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(child).as_bool();
        let v = &mut *(out.0 as *mut Vec<serde_json::Value>);
        v.push(serde_json::json!({"class": name, "visible": visible, "rect": [r.left, r.top, r.right, r.bottom]}));
        windows::Win32::Foundation::BOOL(1)
    }
    let mut children: Vec<serde_json::Value> = Vec::new();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumChildWindows(
            hwnd,
            Some(enum_cb),
            LPARAM(&mut children as *mut _ as isize),
        );
    }
    println!("{}", serde_json::json!({"event":"diag_post_show","children": children}));
    let _ = std::io::Write::flush(&mut std::io::stdout());
}


fn loader_path() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let beside = exe.with_file_name("WebView2Loader.dll");
        if beside.exists() {
            return Some(beside);
        }
        // repo layout: crates/wb-panel/target/debug/../../.wv2-sdk/...
        if let Some(root) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            let sdk = root.join(r".wv2-sdk\runtimes\win-x64\native\WebView2Loader.dll");
            if sdk.exists() {
                return Some(sdk);
            }
        }
    }
    None
}

const POC_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;height:100%;background:transparent;font-family:'Segoe UI Variable','Microsoft YaHei',sans-serif}
.panel{box-sizing:border-box;height:100%;padding:14px;display:flex;flex-direction:column;gap:10px}
.card{flex:1;background:rgba(18,18,28,.62);backdrop-filter:blur(2px);border:1px solid rgba(255,255,255,.14);border-radius:14px;padding:16px;color:#f5f5f5;display:flex;flex-direction:column;gap:10px}
input{width:100%;box-sizing:border-box;padding:12px 16px;font-size:15px;border-radius:10px;border:1px solid rgba(255,255,255,.18);background:rgba(255,255,255,.10);color:#fff;outline:none}
input::placeholder{color:rgba(255,255,255,.55)}
.row{background:rgba(255,255,255,.08);border:1px solid rgba(255,255,255,.10);border-radius:10px;padding:10px 14px;font-size:13.5px}
.dim{opacity:.6;font-size:12px}
</style></head><body><div class="panel"><div class="card">
<input placeholder="WB PoC — 搜索应用 / 文件 / 剪贴板 / 问 AI…" autofocus>
<div class="row">✅ WebView2 透明背景 + Mica 底材验证页</div>
<div class="row">卡片外圈透出的磨砂感来自 DWM backdrop，不是 WebView2 画的。</div>
<div class="dim">Esc 隐藏 · 这是 PoC 2b 内嵌页面</div>
</div></div></body></html>"#;

pub fn embed(hwnd: HWND) -> Result<(), String> {
    unsafe {
        let coinit = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        println!("{}", serde_json::json!({"event":"debug","coinit": format!("0x{:08x}", coinit.0), "tid_embed": unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }}));

        let dll = loader_path().ok_or("WebView2Loader.dll not found (run cargo build from repo root)")?;
        let hmod = LoadLibraryW(PCWSTR(wide(&dll.to_string_lossy()).as_ptr()))
            .map_err(|e| format!("LoadLibrary WebView2Loader: {e}"))?;
        let proc = GetProcAddress(hmod, windows::core::s!("CreateCoreWebView2EnvironmentWithOptions"))
            .ok_or("GetProcAddress CreateCoreWebView2EnvironmentWithOptions")?;
        let create_env: unsafe extern "system" fn(PCWSTR, PCWSTR, *mut c_void, *mut Handler) -> HRESULT =
            std::mem::transmute(proc);

        let udf = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ".".into())
            .join("WB")
            .join("wv2-poc");
        let _ = std::fs::create_dir_all(&udf);
        let udf_w = wide(&udf.to_string_lossy());

        HWND_SLOT.store(hwnd.0 as usize, Ordering::SeqCst);
        let disable_gpu = std::env::args().any(|a| a == "--disable-gpu");
        let with_opts = disable_gpu || std::env::args().any(|a| a == "--with-opts");
        let opts: *mut c_void = if with_opts {
            let args = if disable_gpu { "--disable-gpu" } else { "" };
            println!("{}", serde_json::json!({"event":"debug","browser_args": args}));
            make_env_options(args) as *mut c_void
        } else {
            std::ptr::null_mut()
        };
        let env_handler = make_handler(IID_ENV_COMPLETED, 0);
        let hr = create_env(PCWSTR::null(), PCWSTR(udf_w.as_ptr()), opts, env_handler);
        if hr.is_err() {
            return Err(format!("CreateCoreWebView2EnvironmentWithOptions: 0x{:08x}", hr.0));
        }
        if !pump_until(&ENV_READY, 10000) || ENV_PTR.load(Ordering::SeqCst) == 0 {
            return Err("environment creation timed out / failed".into());
        }
        let create_hr = CREATE_CTRL_HR.load(Ordering::SeqCst) as i32;
        if HRESULT(create_hr).is_err() {
            return Err(format!("CreateCoreWebView2Controller (in-callback): 0x{create_hr:08x}"));
        }
        if !pump_until(&CTRL_READY, 10000) || CTRL_PTR.load(Ordering::SeqCst) == 0 {
            return Err("controller creation timed out / failed".into());
        }
        let ctrl = CTRL_PTR.load(Ordering::SeqCst) as *mut c_void;

        // QI for ICoreWebView2Controller2 → transparent background.
        let mut ctrl2: *mut c_void = std::ptr::null_mut();
        let ctrl_vt = *(ctrl as *const *const Controller2Vtbl);
        let hr = ((*ctrl_vt).qi)(ctrl, &IID_CONTROLLER2, &mut ctrl2);
        if hr.is_err() || ctrl2.is_null() {
            return Err(format!("QI ICoreWebView2Controller2: 0x{:08x}", hr.0));
        }
        let ctrl2_vt = *(ctrl2 as *const *const Controller2Vtbl);
        let opaque = std::env::args().any(|a| a == "--opaque");
        if !opaque {
            let hr = ((*ctrl2_vt).put_bg_color)(ctrl2, CoreWebView2Color { a: 0, r: 0, g: 0, b: 0 });
            if hr.is_err() {
                return Err(format!("put_DefaultBackgroundColor: 0x{:08x}", hr.0));
            }
        } else {
            println!("{}", serde_json::json!({"event":"debug","bg":"opaque(ab-test)"}));
        }

        let mut bounds = RECT { left: 0, top: 0, right: WINDOW_W, bottom: WINDOW_H };
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut bounds);
        let _ = ((*ctrl2_vt).put_bounds)(ctrl2, bounds);
        let _ = ((*ctrl2_vt).put_is_visible)(ctrl2, 1);

        let mut wv: *mut c_void = std::ptr::null_mut();
        let hr = ((*ctrl2_vt).get_core_webview2)(ctrl2, &mut wv);
        if hr.is_err() || wv.is_null() {
            return Err(format!("get_CoreWebView2: 0x{:08x}", hr.0));
        }
        let wv_vt = *(wv as *const *const WebView2Vtbl);
        let nav_handler = make_nav_handler();
        let mut token: u64 = 0;
        let hr = ((*wv_vt).add_navigation_completed)(wv, nav_handler, &mut token);
        if hr.is_err() {
            return Err(format!("add_NavigationCompleted: 0x{:08x}", hr.0));
        }
        let wm_handler = make_wm_handler();
        let hr = ((*wv_vt).add_web_message_received)(wv, wm_handler, &mut token);
        if hr.is_err() {
            return Err(format!("add_WebMessageReceived: 0x{:08x}", hr.0));
        }
        WV_PTR.store(wv as usize, Ordering::SeqCst);
        Ok(())
    }
}

static WV_PTR: AtomicUsize = AtomicUsize::new(0);

/// URL override: --url <url> (default: local PoC page via file://)
pub fn resolve_url() -> Result<String, String> {
    let cli_url: Option<String> = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--url")
        .map(|w| w[1].clone());
    match cli_url {
        Some(u) => Ok(u),
        None => {
            let html_path = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().and_then(|d| d.parent()).and_then(|d| d.parent()).map(|r| r.to_path_buf()))
                .map(|r| r.join("assets").join("panel-ui").join("index.html"))
                .ok_or("locate index.html")?;
            Ok(format!("file:///{}", html_path.to_string_lossy().replace('\\', "/")))
        }
    }
}

/// Navigate AFTER the host window is visible (Chrome defers real work while hidden).
pub fn navigate(url: &str) -> Result<(), String> {
    let wv = WV_PTR.load(Ordering::SeqCst) as *mut c_void;
    if wv.is_null() {
        return Err("webview not ready".into());
    }
    unsafe {
        let wv_vt = *(wv as *const *const WebView2Vtbl);
        println!("{}", serde_json::json!({"event":"debug","navigate": url}));
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let url_w = wide(url);
        let hr = ((*wv_vt).navigate)(wv, PCWSTR(url_w.as_ptr()));
        if hr.is_err() {
            return Err(format!("Navigate: 0x{:08x}", hr.0));
        }
    }
    Ok(())
}

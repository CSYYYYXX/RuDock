//! 截图换底：呼出面板前截取面板后方的桌面像素，缩半 + PNG 压缩成 data URL
//! 发给页面做整屏背景。效果 = 窗口"完全透明"（空隙是锐利壁纸），而卡片的
//! backdrop-filter 直接模糊这张图 → 真正的磨砂玻璃（WebView2 的
//! backdrop-filter 本身够不到桌面像素）。
//!
//! 结果带 30s 缓存：连续呼出/隐藏零开销，壁纸变化半分钟内自动跟上。

use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};

struct Cache {
    at: Instant,
    url: String,
}
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// `--fakebg`：旧的"截图换底"兼容路径。默认走 DWM 区域模糊真透明
/// （`dwm::set_blur_regions`），show 路径完全不截图、零开销。
pub fn fakebg_enabled() -> bool {
    std::env::args().any(|a| a == "--fakebg")
}

/// 只看缓存、绝不阻塞：show 路径专用。首次启动缓存为空时调用方才同步截。
pub fn cached_data_url() -> Option<String> {
    CACHE.lock().unwrap().as_ref().map(|c| c.url.clone())
}

/// Data URL of a half-scale PNG of the work area. `None` if capture fails
/// (页面会保持上一次的背景或纯色兜底).
pub fn capture_data_url() -> Option<String> {    if let Some(c) = &*CACHE.lock().unwrap() {
        if c.at.elapsed() < Duration::from_secs(30) {
            return Some(c.url.clone());
        }
    }
    match capture_fresh() {
        Ok(url) => {
            println!("{}", serde_json::json!({"event":"bg_capture","ok":true,"bytes":url.len()}));
            let _ = std::io::Write::flush(&mut std::io::stdout());
            *CACHE.lock().unwrap() = Some(Cache { at: Instant::now(), url: url.clone() });
            Some(url)
        }
        Err(e) => {
            println!("{}", serde_json::json!({"event":"bg_capture","ok":false,"err":e}));
            let _ = std::io::Write::flush(&mut std::io::stdout());
            None
        }
    }
}

fn capture_fresh() -> Result<String, String> {
    unsafe {
        let mut rc = RECT::default();
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rc as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .map_err(|e| format!("workarea: {e}"))?;
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if w <= 0 || h <= 0 {
            return Err("empty work area".into());
        }
        let (dw, dh) = (w, h); // 全分辨率：空隙处的"假透明"必须锐利

        let hdc_screen = GetDC(HWND::default()); // 整个虚拟屏 DC（物理像素）
        if hdc_screen.0.is_null() {
            return Err("GetDC failed".into());
        }
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        let hbm = CreateCompatibleBitmap(hdc_screen, dw, dh);
        let old = SelectObject(hdc_mem, HGDIOBJ::from(hbm));
        SetStretchBltMode(hdc_mem, STRETCH_HALFTONE);
        let ok = StretchBlt(hdc_mem, 0, 0, dw, dh, hdc_screen, rc.left, rc.top, w, h, SRCCOPY);

        // 读出 BGRA 像素（top-down）
        let mut raw = vec![0u8; (dw * dh * 4) as usize];
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = dw;
        bmi.bmiHeader.biHeight = -dh; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let rows = GetDIBits(
            hdc_mem,
            hbm,
            0,
            dh as u32,
            Some(raw.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let _ = SelectObject(hdc_mem, old);
        let _ = DeleteObject(HGDIOBJ::from(hbm));
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(HWND::default(), hdc_screen);

        if ok.as_bool() == false || rows == 0 {
            return Err("stretchblt/getdibits failed".into());
        }
        let mut rgba = crate::icons::bgra_to_rgba(&raw);
        // GetDIBits 的 32bpp 不带有效 alpha（全 0 → 全透明黑图）。屏幕截图强制不透明。
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        // PNG scanlines: 每行前置 filter byte 0，再整体 zlib 压缩
        let stride = (dw * 4) as usize;
        let mut scan = Vec::with_capacity((stride + 1) * dh as usize);
        for y in 0..dh as usize {
            scan.push(0);
            scan.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&scan, 4);
        let png = crate::icons::png_encode_rgba_z(dw as u32, dh as u32, &z);
        Ok(format!("data:image/png;base64,{}", crate::icons::b64_pub_encode(&png)))
    }
}

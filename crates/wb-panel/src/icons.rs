//! App icon extraction: SHGetFileInfo → HICON → 32×32 BGRA DIB → PNG (stored
//! deflate, zero-dependency encoder) → base64 data URL for the WebView page.

use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Extract the file/shortcut icon as a PNG data URL (SIZE×SIZE).
pub fn icon_data_url(path: &str) -> Result<String, String> {
    let rgba = extract_rgba(path)?;
    let png = png_encode_rgba(SIZE, SIZE, &rgba);
    Ok(format!("data:image/png;base64,{}", b64_encode(&png)))
}

const SIZE: u32 = 64; // extracted at 64px: crisp on HiDPI (was 32 — blurry)

fn extract_rgba(path: &str) -> Result<Vec<u8>, String> {
    // Preferred: IShellItemImageFactory — real 64px render, resolves .lnk targets.
    if let Ok(rgba) = extract_via_image_factory(path) {
        return Ok(rgba);
    }
    extract_via_shgetfileinfo(path)
}

fn extract_via_image_factory(path: &str) -> Result<Vec<u8>, String> {
    use windows::Win32::UI::Shell::{SHCreateItemFromParsingName, IShellItemImageFactory, SIIGBF_ICONONLY, SIIGBF_BIGGERSIZEOK};
    unsafe {
        let p = wide(path);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(p.as_ptr()), None)
            .map_err(|e| format!("SHCreateItemFromParsingName: {e}"))?;
        let hbm = factory
            .GetImage(
                windows::Win32::Foundation::SIZE { cx: SIZE as i32, cy: SIZE as i32 },
                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
            )
            .map_err(|e| format!("GetImage: {e}"))?;
        let rgba = bitmap_to_rgba(hbm, SIZE, SIZE);
        let _ = DeleteObject(hbm);
        rgba
    }
}

fn extract_via_shgetfileinfo(path: &str) -> Result<Vec<u8>, String> {
    unsafe {
        let p = wide(path);
        let mut info = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            PCWSTR(p.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if r == 0 || info.hIcon.is_invalid() {
            return Err("SHGetFileInfoW: no icon".into());
        }
        let hicon = info.hIcon;
        let hdc_screen = GetDC(None);
        let hdc = CreateCompatibleDC(hdc_screen);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = SIZE as i32;
        bmi.bmiHeader.biHeight = -(SIZE as i32); // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbm = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| format!("CreateDIBSection: {e}"))?;
        let old = SelectObject(hdc, hbm);
        // 32px source drawn onto 64 canvas — fallback only, mild upscale.
        let _ = DrawIconEx(hdc, 16, 16, hicon, 32, 32, 0, None, DI_NORMAL);
        let raw = std::slice::from_raw_parts(bits as *const u8, (SIZE * SIZE * 4) as usize);
        let rgba = bgra_to_rgba(raw);
        SelectObject(hdc, old);
        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc);
        ReleaseDC(None, hdc_screen);
        let _ = DestroyIcon(hicon);
        Ok(rgba)
    }
}

/// Read a (premultiplied BGRA, top-down) HBITMAP back to straight RGBA.
fn bitmap_to_rgba(hbm: HBITMAP, w: u32, h: u32) -> Result<Vec<u8>, String> {
    unsafe {
        let mut bmp = BITMAP::default();
        if GetObjectW(hbm, std::mem::size_of::<BITMAP>() as i32, Some(&mut bmp as *mut _ as *mut c_void)) == 0 {
            return Err("GetObjectW".into());
        }
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut raw = vec![0u8; (w * h * 4) as usize];
        let hdc = GetDC(None);
        let n = GetDIBits(hdc, hbm, 0, h, Some(raw.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);
        ReleaseDC(None, hdc);
        if n == 0 {
            return Err("GetDIBits".into());
        }
        // Shell image factory returns premultiplied BGRA; un-premultiply for PNG.
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for px in raw.chunks_exact(4) {
            let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
            if a > 0 && a < 255 {
                let un = |c: u8| ((c as u32 * 255 / a as u32).min(255)) as u8;
                out.extend_from_slice(&[un(r), un(g), un(b), a]);
            } else {
                out.extend_from_slice(&[r, g, b, a]);
            }
        }
        Ok(out)
    }
}

pub(crate) fn bgra_to_rgba(raw: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(raw.len());
    for px in raw.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    rgba
}

// ---- minimal PNG encoder (RGBA8, zlib "stored" blocks) ----

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let mut c = Vec::with_capacity(4 + data.len());
    c.extend_from_slice(tag);
    c.extend_from_slice(data);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
    out
}

pub(crate) fn png_encode_rgba(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    // raw scanlines: filter byte 0 per row
    let stride = (w * 4) as usize;
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }
    // zlib stream of stored deflate blocks (≤65535 each)
    let mut z = vec![0x78, 0x01];
    let mut i = 0;
    while i < raw.len() {
        let n = (raw.len() - i).min(65535);
        let last = i + n == raw.len();
        z.push(if last { 1 } else { 0 });
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[i..i + n]);
        i += n;
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    png_encode_rgba_z(w, h, &z)
}

/// Wrap an already-built zlib stream (compressed or stored) into a PNG.
pub(crate) fn png_encode_rgba_z(w: u32, h: u32, z: &[u8]) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    png.extend(chunk(b"IHDR", &ihdr));
    png.extend(chunk(b"IDAT", z));
    png.extend(chunk(b"IEND", &[]));
    png
}

/// Public base64 (media album art uses it too).
pub fn b64_pub_encode(data: &[u8]) -> String {
    b64_encode(data)
}

fn b64_encode(data: &[u8]) -> String {    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let b0 = c[0] as u32;
        let b1 = *c.get(1).unwrap_or(&0) as u32;
        let b2 = *c.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Decode helper for the headless --icon-test path.
pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u32, String> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'+' => Ok(62),
            b'/' => Ok(63),
            b'=' => Ok(0),
            _ => Err(format!("bad b64 char: {}", c as char)),
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for c in bytes.chunks(4) {
        if c.len() < 4 {
            break;
        }
        let n = (val(c[0])? << 18) | (val(c[1])? << 12) | (val(c[2])? << 6) | val(c[3])?;
        out.push((n >> 16) as u8);
        if c[2] != b'=' {
            out.push((n >> 8) as u8);
        }
        if c[3] != b'=' {
            out.push(n as u8);
        }
    }
    Ok(out)
}

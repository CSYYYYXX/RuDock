//! GSMTC media integration: current track info + transport controls via
//! GlobalSystemMediaTransportControlsSessionManager (the same system API
//! Windows widgets use). Runs on worker threads (MTA).

use std::ffi::c_void;

pub struct MediaInfo {
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub art_data_url: Option<String>,
}

fn init_mta() {
    unsafe {
        // Ignore RPC_E_CHANGED_MODE (already STA) — GetCurrentSession still works.
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
    }
}

/// Current media snapshot. Ok(None) = no active session (nothing playing).
pub fn current() -> Result<Option<MediaInfo>, String> {
    use windows::Media::Control::*;
    init_mta();
    unsafe {
        let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map_err(|e| format!("RequestAsync: {e}"))?
            .get()
            .map_err(|e| format!("RequestAsync.get: {e}"))?;
        let session = match mgr.GetCurrentSession() {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let props = session
            .TryGetMediaPropertiesAsync()
            .and_then(|a| a.get())
            .map_err(|e| format!("TryGetMediaProperties: {e}"))?;
        let title = props.Title().map(|h| h.to_string_lossy()).unwrap_or_default();
        let artist = props.Artist().map(|h| h.to_string_lossy()).unwrap_or_default();
        if title.is_empty() {
            return Ok(None);
        }
        let status = session
            .GetPlaybackInfo()
            .and_then(|i| Ok(i.PlaybackStatus()?))
            .unwrap_or(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Stopped);
        let playing = status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;

        // Album art: thumbnail stream → PNG/JPEG bytes → data URL.
        let art = (|| -> Option<String> {
            let thumb = props.Thumbnail().ok()?;
            let stream = thumb.OpenReadAsync().ok()?.get().ok()?;
            let size = stream.Size().ok()? as u32;
            if size == 0 || size > 3_000_000 {
                return None;
            }
            let ctype = stream.ContentType().ok()?.to_string_lossy();
            let reader = windows::Storage::Streams::DataReader::CreateDataReader(&stream).ok()?;
            reader.LoadAsync(size).ok()?.get().ok()?;
            let mut buf = vec![0u8; size as usize];
            reader.ReadBytes(&mut buf).ok()?;
            Some(format!("data:{ctype};base64,{}", crate::icons::b64_pub_encode(&buf)))
        })();

        Ok(Some(MediaInfo { playing, title, artist, art_data_url: art }))
    }
}

/// Transport control: "toggle" | "next" | "prev".
pub fn command(cmd: &str) -> Result<(), String> {
    use windows::Media::Control::*;
    init_mta();
    unsafe {
        let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map_err(|e| format!("RequestAsync: {e}"))?
            .get()
            .map_err(|e| format!("RequestAsync.get: {e}"))?;
        let session = mgr.GetCurrentSession().map_err(|e| format!("no session: {e}"))?;
        let r = match cmd {
            "toggle" => session.TryTogglePlayPauseAsync(),
            "next" => session.TrySkipNextAsync(),
            "prev" => session.TrySkipPreviousAsync(),
            _ => return Err(format!("unknown media cmd: {cmd}")),
        };
        r.map_err(|e| format!("media cmd: {e}"))?
            .get()
            .map_err(|e| format!("media cmd get: {e}"))?;
        Ok(())
    }
}

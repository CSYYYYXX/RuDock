//! Weather via open-meteo (no API key). Location: ip-api.com coarse geo,
//! fallback Wuhan. Fetched through system curl.exe (zero extra deps),
//! cached 10 minutes. All on worker threads.

use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Cache {
    at: Option<Instant>,
    data: Option<serde_json::Value>,
}
static CACHE: Mutex<Cache> = Mutex::new(Cache { at: None, data: None });

fn curl_json(url: &str) -> Result<serde_json::Value, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new(r"C:\Windows\System32\curl.exe")
        .args(["-s", "--max-time", "8", url])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exit {:?}", out.status.code()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("json: {e}"))
}

fn locate() -> (f64, f64, String) {
    if let Ok(v) = curl_json("http://ip-api.com/json/?fields=lat,lon,city&lang=zh-CN") {
        let lat = v.get("lat").and_then(|x| x.as_f64());
        let lon = v.get("lon").and_then(|x| x.as_f64());
        let city = v.get("city").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if let (Some(lat), Some(lon)) = (lat, lon) {
            return (lat, lon, city);
        }
    }
    (30.59, 114.30, "武汉".into()) // fallback
}

/// WMO weather code → (icon, 中文)
fn wmo(code: i64) -> (&'static str, &'static str) {
    match code {
        0 => ("☀", "晴"),
        1 => ("🌤", "大部晴"),
        2 => ("⛅", "多云"),
        3 => ("☁", "阴"),
        45 | 48 => ("🌫", "雾"),
        51 | 53 | 55 | 56 | 57 => ("🌦", "毛毛雨"),
        61 | 63 | 65 | 66 | 67 | 80 | 81 | 82 => ("🌧", "雨"),
        71 | 73 | 75 | 77 | 85 | 86 => ("🌨", "雪"),
        95 | 96 | 99 => ("⛈", "雷暴"),
        _ => ("·", "未知"),
    }
}

pub fn current() -> Result<serde_json::Value, String> {
    {
        let c = CACHE.lock().unwrap();
        if let (Some(at), Some(data)) = (c.at, &c.data) {
            if at.elapsed() < Duration::from_secs(600) {
                return Ok(data.clone());
            }
        }
    }
    let (lat, lon, city) = locate();
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code,relative_humidity_2m,wind_speed_10m&hourly=temperature_2m,weather_code&forecast_hours=6&timezone=auto"
    );
    let v = curl_json(&url)?;
    let cur = v.get("current").ok_or("no current")?;
    let temp = cur.get("temperature_2m").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let code = cur.get("weather_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let hum = cur.get("relative_humidity_2m").and_then(|x| x.as_i64()).unwrap_or(0);
    let wind = cur.get("wind_speed_10m").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let (icon, text) = wmo(code);

    let mut hourly = Vec::new();
    if let Some(h) = v.get("hourly") {
        let times: Vec<&str> = h.get("time").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|t| t.as_str()).collect()).unwrap_or_default();
        let temps: Vec<f64> = h.get("temperature_2m").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|t| t.as_f64()).collect()).unwrap_or_default();
        let codes: Vec<i64> = h.get("weather_code").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|t| t.as_i64()).collect()).unwrap_or_default();
        for i in 0..times.len().min(6) {
            let hh = times[i].rsplit('T').next().unwrap_or("").to_string();
            let (hi, _) = wmo(*codes.get(i).unwrap_or(&0));
            hourly.push(serde_json::json!({"time": hh, "temp": temps.get(i).copied().unwrap_or(0.0).round(), "icon": hi}));
        }
    }

    let data = serde_json::json!({
        "city": city, "temp": temp.round(), "icon": icon, "text": text, "code": code,
        "humidity": hum, "wind": (wind * 10.0).round() / 10.0,
        "hourly": hourly,
    });
    let mut c = CACHE.lock().unwrap();
    c.at = Some(Instant::now());
    c.data = Some(data.clone());
    Ok(data)
}

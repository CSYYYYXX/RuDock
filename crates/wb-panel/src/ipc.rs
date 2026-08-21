//! Minimal daemon JSON-RPC client for the panel host (same wire protocol
//! as wb-cli: NDJSON over the `wb-daemon` named pipe, auto-spawn on miss).

use std::io::{BufRead, BufReader, Write};
use interprocess::local_socket::{prelude::*, GenericNamespaced};
use interprocess::TryClone;

type LsStream = interprocess::local_socket::Stream;

pub struct Client {
    reader: BufReader<LsStream>,
    writer: LsStream,
}

impl Client {
    pub fn connect() -> Result<Self, String> {
        let name = wb_core::paths::pipe_name()
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| format!("pipe name: {e}"))?;
        match LsStream::connect(name.clone()) {
            Ok(s) => Ok(Self {
                reader: BufReader::new(s.try_clone().map_err(|e| e.to_string())?),
                writer: s,
            }),
            Err(_) => {
                spawn_daemon();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    match LsStream::connect(name.clone()) {
                        Ok(s) => {
                            return Ok(Self {
                                reader: BufReader::new(s.try_clone().map_err(|e| e.to_string())?),
                                writer: s,
                            })
                        }
                        Err(e) => {
                            if std::time::Instant::now() > deadline {
                                return Err(format!("cannot reach daemon: {e}"));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(80));
                        }
                    }
                }
            }
        }
    }

    pub fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let mut line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;
        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("daemon closed connection".into());
        }
        let resp: serde_json::Value = serde_json::from_str(buf.trim()).map_err(|e| e.to_string())?;
        if let Some(err) = resp.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("daemon error");
            return Err(msg.to_string());
        }
        Ok(resp.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }
}

#[cfg(windows)]
fn spawn_daemon() {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wb-daemon.exe")));
    if let Some(exe) = exe {
        if exe.exists() {
            let _ = std::process::Command::new(exe)
                .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}

#[cfg(not(windows))]
fn spawn_daemon() {}

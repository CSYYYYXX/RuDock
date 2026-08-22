//! WB MCP adapter: stdio JSON-RPC server for Claude/Cursor and other Agents.
//!
//! The daemon remains the single source of truth. MCP only translates:
//! - tools/list  <- daemon cmd.tools
//! - tools/call  -> daemon cmd.run / skill.list / skill.get
//! - resources   <- plugin Skills exposed by daemon

use interprocess::local_socket::{prelude::*, GenericNamespaced};
use interprocess::TryClone;
use std::io::{BufRead, BufReader, Write};
use wb_core::protocol::{Request, Response};

struct DaemonClient {
    reader: BufReader<interprocess::local_socket::Stream>,
    writer: interprocess::local_socket::Stream,
    next_id: u64,
}

impl DaemonClient {
    fn connect() -> Result<Self, String> {
        let name = wb_core::paths::pipe_name().to_ns_name::<GenericNamespaced>().map_err(|e| format!("pipe name: {e}"))?;
        let stream = match interprocess::local_socket::Stream::connect(name.clone()) {
            Ok(stream) => stream,
            Err(_) => {
                spawn_daemon();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    match interprocess::local_socket::Stream::connect(name.clone()) {
                        Ok(stream) => break stream,
                        Err(e) if std::time::Instant::now() >= deadline => return Err(format!("daemon unavailable: {e}")),
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(80)),
                    }
                }
            }
        };
        let reader = BufReader::new(stream.try_clone().map_err(|e| format!("pipe clone: {e}"))?);
        Ok(Self { reader, writer: stream, next_id: 1 })
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request { jsonrpc: "2.0".into(), id: serde_json::json!(id), method: method.into(), params };
        let mut line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;
        let mut buf = String::new();
        if self.reader.read_line(&mut buf).map_err(|e| e.to_string())? == 0 { return Err("daemon closed connection".into()); }
        let resp: Response = serde_json::from_str(buf.trim()).map_err(|e| e.to_string())?;
        if let Some(error) = resp.error { return Err(error.get("message").and_then(|v| v.as_str()).unwrap_or("daemon error").into()); }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}

#[cfg(windows)]
fn spawn_daemon() {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("wb-daemon.exe")));
    if let Some(exe) = exe.filter(|p| p.is_file()) {
        let _ = std::process::Command::new(exe).creation_flags(0x0800_0000 | 0x0000_0008)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    }
}

#[cfg(not(windows))]
fn spawn_daemon() {}

fn rpc_result(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn rpc_error(id: &serde_json::Value, code: i64, message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.into()}})
}

fn text_content(value: &serde_json::Value) -> serde_json::Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    serde_json::json!({"content":[{"type":"text","text":text}]})
}

fn mcp_tools(daemon_tools: serde_json::Value) -> serde_json::Value {
    let mut out = Vec::new();
    if let Some(items) = daemon_tools.as_array() {
        for tool in items {
            let Some(name) = tool.get("name").and_then(|v| v.as_str()) else { continue };
            out.push(serde_json::json!({
                "name": name,
                "description": tool.get("description").cloned().unwrap_or(serde_json::json!("")),
                "inputSchema": tool.get("parameters").cloned().unwrap_or(serde_json::json!({"type":"object","properties":{}})),
            }));
        }
    }
    out.push(serde_json::json!({"name":"skill_list","description":"列出社区插件提供的 Agent Skills。","inputSchema":{"type":"object","properties":{},"required":[],"additionalProperties":false}}));
    out.push(serde_json::json!({"name":"skill_get","description":"读取一个插件 Skill 的完整 Markdown 说明。","inputSchema":{"type":"object","properties":{"plugin":{"type":"string"},"id":{"type":"string"}},"required":["plugin","id"],"additionalProperties":false}}));
    serde_json::json!({"tools":out})
}

fn call_tool(client: &mut DaemonClient, name: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let result = match name {
        "skill_list" => client.call("skill.list", args)?,
        "skill_get" => client.call("skill.get", args)?,
        other => client.call("cmd.run", serde_json::json!({"id": other.replace('_', "."), "args": args}))?,
    };
    Ok(text_content(&result))
}

fn resources_list(client: &mut DaemonClient) -> Result<serde_json::Value, String> {
    let skills = client.call("skill.list", serde_json::json!({}))?;
    let empty = Vec::new();
    let resources: Vec<serde_json::Value> = skills.as_array().unwrap_or(&empty).iter().filter_map(|s| {
        let plugin = s.get("plugin")?.as_str()?;
        let id = s.get("id")?.as_str()?;
        Some(serde_json::json!({"uri":format!("wb://skill/{plugin}/{id}"),"name":s.get("name").cloned().unwrap_or(serde_json::json!(id)),"description":s.get("description").cloned().unwrap_or(serde_json::json!("")),"mimeType":"text/markdown"}))
    }).collect();
    Ok(serde_json::json!({"resources":resources}))
}

fn resource_read(client: &mut DaemonClient, uri: &str) -> Result<serde_json::Value, String> {
    let rest = uri.strip_prefix("wb://skill/").ok_or("unsupported resource URI")?;
    let (plugin, id) = rest.split_once('/').ok_or("invalid Skill resource URI")?;
    let value = client.call("skill.get", serde_json::json!({"plugin":plugin,"id":id}))?;
    let text = value.get("content").and_then(|v| v.as_str()).unwrap_or("");
    Ok(serde_json::json!({"contents":[{"uri":uri,"mimeType":"text/markdown","text":text}]}))
}

fn handle(client: &mut DaemonClient, request: serde_json::Value) -> Option<serde_json::Value> {
    let id = request.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
    if request.get("id").is_none() { return None; }
    let result = match method {
        "initialize" => Ok(serde_json::json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{},"resources":{"subscribe":false,"listChanged":false}},"serverInfo":{"name":"wb-mcp","version":env!("CARGO_PKG_VERSION")}})),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => client.call("cmd.tools", serde_json::json!({})).map(mcp_tools),
        "tools/call" => {
            let name = request.pointer("/params/name").and_then(|v| v.as_str()).ok_or_else(|| "missing tool name".to_string());
            let args = request.pointer("/params/arguments").cloned().unwrap_or(serde_json::json!({}));
            name.and_then(|n| call_tool(client, n, args))
        }
        "resources/list" => resources_list(client),
        "resources/read" => {
            let uri = request.pointer("/params/uri").and_then(|v| v.as_str()).ok_or_else(|| "missing resource uri".to_string());
            uri.and_then(|u| resource_read(client, u))
        }
        _ => Err(format!("method not found: {method}")),
    };
    Some(match result { Ok(value) => rpc_result(&id, value), Err(error) => rpc_error(&id, -32602, error) })
}

fn main() {
    let stdin = std::io::stdin();
    let mut client = match DaemonClient::connect() { Ok(c) => c, Err(e) => { eprintln!("wb-mcp: {e}"); std::process::exit(5); } };
    let mut stdout = std::io::BufWriter::new(std::io::stdout());
    for line in BufReader::new(stdin.lock()).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() { continue; }
        let request = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => v,
            Err(e) => { let out = rpc_error(&serde_json::Value::Null, -32700, e.to_string()); let _ = writeln!(stdout, "{}", out); let _ = stdout.flush(); continue; }
        };
        if let Some(response) = handle(&mut client, request) { let _ = writeln!(stdout, "{}", response); let _ = stdout.flush(); }
    }
}

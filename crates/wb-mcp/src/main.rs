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
    serde_json::json!({
        "content":[{"type":"text","text":text}],
        "structuredContent": value,
        "isError": false,
    })
}

fn tool_error(message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "content":[{"type":"text","text":message.into()}],
        "isError": true,
    })
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
                "annotations": tool.get("annotations").cloned().unwrap_or_else(|| serde_json::json!({
                    "readOnlyHint": false,
                    "destructiveHint": true,
                    "idempotentHint": false,
                    "openWorldHint": true,
                })),
            }));
        }
    }
    out.push(serde_json::json!({"name":"skill_list","description":"列出社区插件提供的 Agent Skills。","inputSchema":{"type":"object","properties":{},"required":[],"additionalProperties":false},"annotations":{"title":"列出 WB Skills","readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}));
    out.push(serde_json::json!({"name":"skill_get","description":"读取一个插件 Skill 的完整 Markdown 说明。","inputSchema":{"type":"object","properties":{"plugin":{"type":"string"},"id":{"type":"string"}},"required":["plugin","id"],"additionalProperties":false},"annotations":{"title":"读取 WB Skill","readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}));
    out.push(serde_json::json!({"name":"events_tail","description":"按游标读取 WB daemon 的脱敏审计事件；可选长轮询等待新事件。","inputSchema":{"type":"object","properties":{"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200},"wait_ms":{"type":"integer","minimum":0,"maximum":30000}},"required":[],"additionalProperties":false},"annotations":{"title":"读取 WB 事件","readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}));
    serde_json::json!({"tools":out})
}

fn call_tool(client: &mut DaemonClient, name: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let result = match name {
        "skill_list" => client.call("skill.list", args)?,
        "skill_get" => client.call("skill.get", args)?,
        "events_tail" => client.call("events.tail", args)?,
        other => client.call("cmd.tool.run", serde_json::json!({"name": other, "args": args, "origin": "mcp"}))?,
    };
    Ok(text_content(&result))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WritePolicy { Client, Ask, ReadOnly }

fn write_policy(client: &mut DaemonClient) -> Result<WritePolicy, String> {
    let settings = client.call("settings.get", serde_json::json!({}))?;
    Ok(write_policy_from_settings(&settings))
}

fn write_policy_from_settings(settings: &serde_json::Value) -> WritePolicy {
    match settings.get("mcp_write_policy").and_then(|value| value.as_str()) {
        Some("ask") => WritePolicy::Ask,
        Some("read-only") => WritePolicy::ReadOnly,
        _ => WritePolicy::Client,
    }
}

fn tool_risk(client: &mut DaemonClient, name: &str) -> Result<(bool, String), String> {
    match name {
        "skill_list" => return Ok((true, "列出 WB Skills".into())),
        "skill_get" => return Ok((true, "读取 WB Skill".into())),
        "events_tail" => return Ok((true, "读取 WB 事件".into())),
        _ => {}
    }
    let tools = client.call("cmd.tools", serde_json::json!({"include_annotations":true}))?;
    let tool = tools.as_array()
        .and_then(|items| items.iter().find(|tool| tool.get("name").and_then(|value| value.as_str()) == Some(name)))
        .ok_or_else(|| format!("unknown tool: {name}"))?;
    let read_only = tool.pointer("/annotations/readOnlyHint").and_then(|value| value.as_bool()).unwrap_or(false);
    let title = tool.pointer("/annotations/title").and_then(|value| value.as_str()).unwrap_or(name).to_string();
    Ok((read_only, title))
}

struct SessionState {
    protocol_version: String,
    elicitation: bool,
    next_server_id: u64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self { protocol_version: "2024-11-05".into(), elicitation: false, next_server_id: 1 }
    }
}

fn confirm_tool<R: BufRead, W: Write>(state: &mut SessionState, name: &str, title: &str, args: &serde_json::Value, reader: &mut R, writer: &mut W) -> Result<bool, String> {
    let request_id = format!("wb-elicitation-{}", state.next_server_id);
    state.next_server_id += 1;
    let args = serde_json::to_string(args).unwrap_or_else(|_| "{}".into());
    let args: String = args.chars().take(800).collect();
    let request = serde_json::json!({
        "jsonrpc":"2.0", "id":request_id, "method":"elicitation/create",
        "params":{"message":format!("WB 请求执行工具「{title}」({name})。参数：{args}"),
        "requestedSchema":{"type":"object","properties":{"confirm":{"type":"boolean","title":"允许执行","description":"仅确认本次调用","default":false}},"required":["confirm"]}}
    });
    writeln!(writer, "{request}").map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())?;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|error| error.to_string())? == 0 {
            return Err("client closed while awaiting MCP elicitation".into());
        }
        let message: serde_json::Value = serde_json::from_str(line.trim()).map_err(|error| error.to_string())?;
        if message.get("id") == Some(&serde_json::json!(request_id)) && message.get("method").is_none() {
            let action = message.pointer("/result/action").and_then(|value| value.as_str());
            let confirmed = message.pointer("/result/content/confirm").and_then(|value| value.as_bool()).unwrap_or(false);
            return Ok(action == Some("accept") && confirmed);
        }
        if message.get("method").and_then(|value| value.as_str()) == Some("notifications/cancelled")
            && message.pointer("/params/requestId") == Some(&serde_json::json!(request_id)) {
            return Ok(false);
        }
        if let Some(id) = message.get("id") {
            let response = rpc_error(id, -32000, "WB is awaiting tool confirmation");
            writeln!(writer, "{response}").map_err(|error| error.to_string())?;
            writer.flush().map_err(|error| error.to_string())?;
        }
    }
}

fn call_tool_with_policy<R: BufRead, W: Write>(client: &mut DaemonClient, state: &mut SessionState, name: &str, args: serde_json::Value, reader: &mut R, writer: &mut W) -> Result<serde_json::Value, String> {
    let (read_only, title) = tool_risk(client, name)?;
    if !read_only {
        match write_policy(client)? {
            WritePolicy::Client => {}
            WritePolicy::ReadOnly => return Ok(tool_error(format!("WB 已按 read-only 策略阻止写工具：{title} ({name})"))),
            WritePolicy::Ask => {
                if !state.elicitation {
                    return Ok(tool_error("WB 的 ask 策略要求 MCP 2025-06-18 elicitation；当前客户端未声明支持"));
                }
                if !confirm_tool(state, name, &title, &args, reader, writer)? {
                    return Ok(tool_error(format!("用户未批准工具调用：{title} ({name})")));
                }
            }
        }
    }
    Ok(call_tool(client, name, args).unwrap_or_else(tool_error))
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

fn handle<R: BufRead, W: Write>(client: &mut DaemonClient, state: &mut SessionState, request: serde_json::Value, reader: &mut R, writer: &mut W) -> Option<serde_json::Value> {
    let id = request.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
    if request.get("id").is_none() { return None; }
    let result = match method {
        "initialize" => {
            let requested = request.pointer("/params/protocolVersion").and_then(|value| value.as_str()).unwrap_or("2024-11-05");
            let modern = requested >= "2025-06-18";
            state.protocol_version = if modern { "2025-06-18" } else { "2024-11-05" }.into();
            state.elicitation = modern && request.pointer("/params/capabilities/elicitation").is_some();
            Ok(serde_json::json!({"protocolVersion":state.protocol_version,"capabilities":{"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false}},"serverInfo":{"name":"wb-mcp","version":env!("CARGO_PKG_VERSION")}}))
        }
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => client.call("cmd.tools", serde_json::json!({"include_annotations":true})).map(mcp_tools),
        "tools/call" => {
            let name = request.pointer("/params/name").and_then(|v| v.as_str()).ok_or_else(|| "missing tool name".to_string());
            let args = request.pointer("/params/arguments").cloned().unwrap_or(serde_json::json!({}));
            name.and_then(|name| call_tool_with_policy(client, state, name, args, reader, writer))
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
    let mut state = SessionState::default();
    let mut stdout = std::io::BufWriter::new(std::io::stdout());
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        let Ok(read) = reader.read_line(&mut line) else { break };
        if read == 0 { break; }
        if line.trim().is_empty() { continue; }
        let request = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => v,
            Err(e) => { let out = rpc_error(&serde_json::Value::Null, -32700, e.to_string()); let _ = writeln!(stdout, "{}", out); let _ = stdout.flush(); continue; }
        };
        if let Some(response) = handle(&mut client, &mut state, request, &mut reader, &mut stdout) { let _ = writeln!(stdout, "{}", response); let _ = stdout.flush(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_daemon_annotations_to_mcp_tools() {
        let tools = mcp_tools(serde_json::json!([{
            "type": "function",
            "name": "search",
            "description": "search",
            "parameters": {"type":"object","properties":{}},
            "annotations": {
                "title": "全局搜索",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        }]));
        let search = &tools["tools"][0];
        assert_eq!(search["annotations"]["readOnlyHint"], true);
        assert_eq!(search["annotations"]["title"], "全局搜索");
        assert!(search.get("inputSchema").is_some());
    }

    #[test]
    fn unknown_tool_risk_defaults_conservative() {
        let tools = mcp_tools(serde_json::json!([{
            "name": "legacy_plugin",
            "parameters": {"type":"object","properties":{}}
        }]));
        let annotations = &tools["tools"][0]["annotations"];
        assert_eq!(annotations["readOnlyHint"], false);
        assert_eq!(annotations["destructiveHint"], true);
        assert_eq!(annotations["openWorldHint"], true);

        let skill = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "skill_get")
            .unwrap();
        assert_eq!(skill["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn maps_events_tool_and_write_policies() {
        let tools = mcp_tools(serde_json::json!([]));
        let events = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "events_tail")
            .unwrap();
        assert_eq!(events["annotations"]["readOnlyHint"], true);
        assert_eq!(
            events["inputSchema"]["properties"]["wait_ms"]["maximum"],
            30_000
        );

        assert_eq!(
            write_policy_from_settings(&serde_json::json!({"mcp_write_policy":"ask"})),
            WritePolicy::Ask
        );
        assert_eq!(
            write_policy_from_settings(&serde_json::json!({"mcp_write_policy":"read-only"})),
            WritePolicy::ReadOnly
        );
        assert_eq!(
            write_policy_from_settings(&serde_json::json!({})),
            WritePolicy::Client
        );
    }

    #[test]
    fn elicitation_requires_explicit_accept_and_confirmation() {
        let accepted = b"{\"jsonrpc\":\"2.0\",\"id\":\"wb-elicitation-1\",\"result\":{\"action\":\"accept\",\"content\":{\"confirm\":true}}}\n";
        let mut reader = BufReader::new(&accepted[..]);
        let mut writer = Vec::new();
        let mut state = SessionState::default();
        assert!(confirm_tool(
            &mut state,
            "todo_add",
            "添加待办",
            &serde_json::json!({"title":"ship"}),
            &mut reader,
            &mut writer,
        )
        .unwrap());
        let request: serde_json::Value =
            serde_json::from_slice(writer.strip_suffix(b"\n").unwrap()).unwrap();
        assert_eq!(request["method"], "elicitation/create");
        assert_eq!(
            request["params"]["requestedSchema"]["required"][0],
            "confirm"
        );

        for response in [
            b"{\"jsonrpc\":\"2.0\",\"id\":\"wb-elicitation-1\",\"result\":{\"action\":\"decline\"}}\n".as_slice(),
            b"{\"jsonrpc\":\"2.0\",\"id\":\"wb-elicitation-1\",\"result\":{\"action\":\"accept\",\"content\":{\"confirm\":false}}}\n".as_slice(),
        ] {
            let mut reader = BufReader::new(response);
            let mut writer = Vec::new();
            let mut state = SessionState::default();
            assert!(!confirm_tool(
                &mut state,
                "todo_add",
                "添加待办",
                &serde_json::json!({}),
                &mut reader,
                &mut writer,
            )
            .unwrap());
        }
    }
}

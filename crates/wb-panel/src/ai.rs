//! 随手问 AI（M3）+ AI 工具调用（M3.5）：
//! 中转站 OpenAI Responses API + function calling，SSE 流式转发给页面。
//! 模型可调用 wb 命令注册表里的工具（todo_add / note_add / search / clip_get / panel_hide），
//! 宿主执行（经 daemon JSON-RPC）后把结果回喂，模型再给自然语言总结（最多 3 轮）。
//! HTTP 复用零依赖路子：系统 curl.exe（-N 无缓冲）按行读 SSE。
//! 网络类错误自动回退本地代理 127.0.0.1:7890。配置：WB_AI_URL / WB_AI_KEY / WB_AI_MODEL。

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;

const INSTRUCTIONS: &str = "你是 Windows 启动面板 WB 里的 AI 助手。用用户的语言回答，简洁直接，先说结论，纯文本（可用短列表，不要表格，除非用户明确要否则不要代码块）。\
    你可以调用工具操作用户的面板：todo_add 加待办、note_add 记笔记、search 全局搜索、clip_get 查看最近剪贴板、panel_hide 收起面板。\
    社区插件还可以携带 Agent Skill 文档；遇到特定工作流或插件能力不熟悉时，先用 skill_list 找到相关 Skill，再用 skill_get 读取说明，然后调用对应插件命令。\
    用户意图是【做事】（提醒、记录、查找、收起等）就调用工具，做完用一句话确认；纯问答直接回答，不要硬调工具。";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_TOOL_ROUNDS: u32 = 3;

struct FnCall {
    call_id: String,
    name: String,
    arguments: String,
}

enum RoundEnd {
    Text,
    Calls(Vec<FnCall>),
}

pub fn ask(id: u64, text: String) {
    if let Err(e) = ask_inner(id, &text) {
        crate::host::post_to_page(serde_json::json!({
            "kind": "ai.error", "id": id, "error": e,
        }));
    }
}

fn ask_inner(id: u64, text: &str) -> Result<(), String> {
    let mut proxy = false;
    let mut input = serde_json::json!(text);

    for round in 0..=MAX_TOOL_ROUNDS {
        match run_round(id, &input, proxy, round == MAX_TOOL_ROUNDS) {
            Ok(RoundEnd::Text) => return Ok(()),
            Ok(RoundEnd::Calls(calls)) => {
                // 执行工具并回喂
                let mut followup: Vec<serde_json::Value> = vec![
                    serde_json::json!({"role":"user","content":[{"type":"input_text","text": text}]}),
                ];
                let mut outputs: Vec<serde_json::Value> = Vec::new();
                for c in &calls {
                    crate::host::post_to_page(serde_json::json!({
                        "kind": "ai.delta", "id": id,
                        "delta": format!("\n⏳ 执行：{} …\n", c.name),
                    }));
                    let output = exec_tool(&c.name, &c.arguments);
                    followup.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": c.call_id, "name": c.name, "arguments": c.arguments,
                    }));
                    outputs.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": c.call_id, "output": output,
                    }));
                }
                followup.extend(outputs);
                input = serde_json::Value::Array(followup);
            }
            Err(e) => {
                if !proxy && is_net_err(&e) {
                    proxy = true; // 网络错误：本地代理重试同一回合
                    continue;
                }
                return Err(e);
            }
        }
    }
    Ok(())
}

fn is_net_err(e: &str) -> bool {
    e.contains("curl exit") || e.contains("spawn") || e.contains("stream ended")
}

/// 执行一个 AI 工具：面板控制走窗口消息，其余统一走 daemon `cmd.run`
/// （cmd.run 内部分辨内建注册表 / 插件命令，面板无需感知）。
/// 工具名 → 命令 id：下划线还原点（todo_add → todo.add），与 cmd.tools 的生成规则互逆。
/// 返回喂回给模型的 output 字符串（紧凑 JSON 或错误描述）。
fn exec_tool(name: &str, arguments: &str) -> String {
    let args: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}));

    if name == "panel_hide" {
        crate::host::post_hide_message();
        return "{\"hidden\": true}".into();
    }
    if name == "skill_list" || name == "skill_get" {
        let method = if name == "skill_list" { "skill.list" } else { "skill.get" };
        return match crate::ipc::Client::connect().and_then(|mut c| c.call(method, args)) {
            Ok(v) => {
                let slim = if name == "skill_get" { slim_skill(&v) } else { v };
                serde_json::to_string(&slim).unwrap_or_else(|_| "{}".into())
            }
            Err(e) => format!("{{\"error\": {}}}", serde_json::to_string(&e).unwrap_or_default()),
        };
    }
    let cmd_id = wb_plugin_sdk::Manifest::cmd_id(name);

    match crate::ipc::Client::connect().and_then(|mut c| {
        c.call("cmd.run", serde_json::json!({"id": cmd_id, "args": args}))
    }) {
        Ok(v) => {
            let slim = match cmd_id.as_str() {
                "search" => slim_search(&v),
                "clip.get" => slim_clips(&v),
                _ => v,
            };
            serde_json::to_string(&slim).unwrap_or_else(|_| "{}".into())
        }
        Err(e) => format!("{{\"error\": {}}}", serde_json::to_string(&e).unwrap_or_default()),
    }
}

fn slim_search(v: &serde_json::Value) -> serde_json::Value {
    v.as_array()
        .map(|arr| {
            serde_json::Value::Array(
                arr.iter()
                    .take(8)
                    .map(|r| {
                        serde_json::json!({
                            "kind": r.get("kind"), "title": r.get("title"),
                            "subtitle": r.get("subtitle"), "path": r.get("path"),
                        })
                    })
                    .collect(),
            )
        })
        .unwrap_or_else(|| v.clone())
}

fn slim_clips(v: &serde_json::Value) -> serde_json::Value {
    v.as_array()
        .map(|arr| {
            serde_json::Value::Array(
                arr.iter()
                    .take(5)
                    .map(|c| {
                        serde_json::json!({
                            "kind": c.get("kind"),
                            "content": c.get("content").and_then(|s| s.as_str()).map(|s| s.chars().take(200).collect::<String>()),
                        })
                    })
                    .collect(),
            )
        })
        .unwrap_or_else(|| v.clone())
}

fn slim_skill(v: &serde_json::Value) -> serde_json::Value {
    let mut out = v.clone();
    if let Some(content) = out.get_mut("content").and_then(|x| x.as_str()).map(String::from) {
        if let Some(obj) = out.as_object_mut() {
            obj.insert("content".into(), serde_json::Value::String(content.chars().take(12_000).collect()));
        }
    }
    out
}

fn skill_tools() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "name": "skill_list",
            "description": "列出社区插件提供的 Agent Skills，帮助选择合适的工作流。",
            "parameters": {"type": "object", "properties": {}, "required": [], "additionalProperties": false}
        },
        {
            "type": "function",
            "name": "skill_get",
            "description": "读取一个插件 Skill 的完整说明。使用 skill_list 得到 plugin 和 id。",
            "parameters": {
                "type": "object",
                "properties": {
                    "plugin": {"type": "string", "description": "插件 id"},
                    "id": {"type": "string", "description": "Skill id"}
                },
                "required": ["plugin", "id"],
                "additionalProperties": false
            }
        }
    ])
}

/// AI 工具清单：daemon `cmd.tools`（内建注册表 + 插件 ai 命令的并集），
/// daemon 不在就退回本地内建注册表（插件工具自然缺席）。
fn fetch_tools() -> serde_json::Value {
    let mut tools = crate::ipc::Client::connect()
        .and_then(|mut c| c.call("cmd.tools", serde_json::json!({})))
        .unwrap_or_else(|_| wb_core::commands::tools_json());
    if let (Some(dst), Some(extra)) = (tools.as_array_mut(), skill_tools().as_array()) {
        dst.extend(extra.iter().cloned());
    }
    tools
}

/// 跑一个 Responses 回合：SSE 流式推页面；返回 Text（纯回答）或 Calls（要调工具）。
/// `no_tools`：最后一轮强制纯文本收尾。
fn run_round(id: u64, input: &serde_json::Value, proxy: bool, no_tools: bool) -> Result<RoundEnd, String> {
    let mut body = serde_json::json!({
        "model": wb_core::ai::model_name(),
        "instructions": INSTRUCTIONS,
        "input": input,
        "stream": true,
    });
    if !no_tools {
        body.as_object_mut().map(|o| o.insert("tools".into(), fetch_tools()));
    }
    let body = body.to_string();

    let mut args: Vec<String> = vec![
        "-sN".into(), "--max-time".into(), "120".into(),
        "-X".into(), "POST".into(), wb_core::ai::api_url(),
        "-H".into(), "Content-Type: application/json".into(),
        "-H".into(), format!("Authorization: Bearer {}", wb_core::ai::api_key()),
        "--data-binary".into(), "@-".into(),
    ];
    if proxy {
        args.push("-x".into());
        args.push("http://127.0.0.1:7890".into());
    }

    let mut child = std::process::Command::new(r"C:\Windows\System32\curl.exe")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("spawn curl: {e}"))?;
    child
        .stdin
        .as_mut()
        .ok_or("no stdin")?
        .write_all(body.as_bytes())
        .map_err(|e| format!("write stdin: {e}"))?;
    drop(child.stdin.take());

    let stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = BufReader::new(stdout);
    let mut got_delta = false;
    let mut calls: Vec<FnCall> = Vec::new();
    let mut plain = String::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("read: {e}"))?;
        let line = line.trim_end();
        if line.is_empty() || line.starts_with("event:") || line.starts_with(':') {
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            plain.push_str(line);
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "response.output_text.delta" => {
                if let Some(d) = ev.get("delta").and_then(|d| d.as_str()) {
                    got_delta = true;
                    crate::host::post_to_page(serde_json::json!({
                        "kind": "ai.delta", "id": id, "delta": d,
                    }));
                }
            }
            "response.output_item.done" => {
                let item = ev.get("item").cloned().unwrap_or(serde_json::Value::Null);
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    calls.push(FnCall {
                        call_id: item.get("call_id").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                        name: item.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                        arguments: item.get("arguments").and_then(|s| s.as_str()).unwrap_or("{}").to_string(),
                    });
                }
            }
            "response.completed" => {
                let _ = child.wait();
                if calls.is_empty() {
                    crate::host::post_to_page(serde_json::json!({"kind":"ai.done","id":id}));
                    return Ok(RoundEnd::Text);
                }
                return Ok(RoundEnd::Calls(calls));
            }
            "response.failed" | "error" => {
                let msg = ev
                    .pointer("/response/error/message")
                    .or_else(|| ev.pointer("/error/message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("未知错误")
                    .to_string();
                let _ = child.kill();
                return Err(msg);
            }
            _ => {}
        }
    }
    let _ = child.wait();
    if !calls.is_empty() {
        return Ok(RoundEnd::Calls(calls));
    }
    if got_delta {
        crate::host::post_to_page(serde_json::json!({"kind":"ai.done","id":id}));
        return Ok(RoundEnd::Text);
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&plain) {
        let msg = v
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("请求失败")
            .to_string();
        return Err(msg);
    }
    Err("stream ended without data".into())
}

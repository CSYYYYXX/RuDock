//! 共享 AI 客户端配置 + 同步问答（daemon `agent.ask` 用，CLI/Agent 场景）。
//! 面板内的流式版本见 wb-panel/src/ai.rs（SSE 逐 token 推页面）。
//! 同样走系统 curl.exe，零新增依赖；网络失败回退本地代理 7890。

use std::io::Write;
use std::os::windows::process::CommandExt;

const DEFAULT_URL: &str = "http://52.9.107.166:18680/responses";
const DEFAULT_KEY: &str = "sk-524934e16de9d6d670d6f8fd1a7dc0554853e25ec319363d0c17960d68a85afd";
const DEFAULT_MODEL: &str = "gpt-5.6-luna";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn api_url() -> String {
    std::env::var("WB_AI_URL").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_URL.into())
}
pub fn api_key() -> String {
    std::env::var("WB_AI_KEY").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_KEY.into())
}
pub fn model_name() -> String {
    std::env::var("WB_AI_MODEL").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_MODEL.into())
}

/// 同步问答：非流式一次拿全文本。供 daemon `agent.ask`（wb CLI / 外部 Agent 用）。
pub fn ask_sync(prompt: &str) -> Result<String, String> {
    match ask_sync_inner(prompt, false) {
        Ok(t) => Ok(t),
        Err(e) => {
            // 网络类错误回退本地代理重试一次
            if e.contains("curl") || e.contains("spawn") {
                ask_sync_inner(prompt, true)
            } else {
                Err(e)
            }
        }
    }
}

fn ask_sync_inner(prompt: &str, proxy: bool) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model_name(),
        "input": prompt,
        "stream": false,
    })
    .to_string();

    let mut args: Vec<String> = vec![
        "-s".into(), "--max-time".into(), "120".into(),
        "-X".into(), "POST".into(), api_url(),
        "-H".into(), "Content-Type: application/json".into(),
        "-H".into(), format!("Authorization: Bearer {}", api_key()),
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
    let out = child.wait_with_output().map_err(|e| format!("curl wait: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exit {:?}", out.status.code()));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("json: {e}"))?;

    if let Some(msg) = v.pointer("/error/message").and_then(|m| m.as_str()) {
        return Err(msg.to_string());
    }
    // Responses API：output[] 里找 type=message 的 content[].text
    if let Some(items) = v.get("output").and_then(|o| o.as_array()) {
        for item in items {
            if item.get("type").and_then(|t| t.as_str()) != Some("message") {
                continue;
            }
            if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                        return Ok(t.to_string());
                    }
                }
            }
        }
    }
    Err("response has no text output".into())
}

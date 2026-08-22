//! JSON-RPC 2.0 over NDJSON (one request per line, one response per line).
//! The single API consumed by panel, CLI and (M3) MCP — API-first dogma.

use crate::error::CoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }

    pub fn err(id: Value, e: &CoreError) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: None, error: Some(e.to_envelope()["error"].clone()) }
    }
}

/// Command surface self-description for `wb schema` — agents explore
/// without reading docs.
pub fn schema() -> Value {
    serde_json::json!({
        "version": 1,
        "methods": [
            {"name": "daemon.ping", "params": {}, "returns": {"type": "object"}},
            {"name": "daemon.stop", "params": {}, "returns": {"type": "object"}},
            {"name": "settings.get", "params": {}, "returns": {"type": "object"}, "status": "M5"},
            {"name": "settings.set", "params": {"takeover_win": "boolean?", "autostart": "boolean?"}, "returns": {"type": "object"}, "status": "M5"},
            {"name": "hook.status", "params": {}, "returns": {"type": "object"}, "status": "M5"},
            {"name": "search", "params": {"query": "string", "limit": "number?", "type": "file|app|clip|note|todo|plugin?"}, "returns": {"type": "array"}},
            {"name": "note.add", "params": {"content": "string", "tags": "string[]?"}, "returns": {"type": "object"}},
            {"name": "note.list", "params": {"limit": "number?"}, "returns": {"type": "array"}},
            {"name": "note.get", "params": {"id": "string"}, "returns": {"type": "object"}},
            {"name": "note.rm", "params": {"id": "string"}, "returns": {"type": "object"}},
            {"name": "todo.add", "params": {"title": "string", "due": "string?", "repeat": "string?", "tags": "string[]?"}, "returns": {"type": "object"}},
            {"name": "todo.list", "params": {"all": "boolean?"}, "returns": {"type": "array"}},
            {"name": "todo.done", "params": {"id": "string"}, "returns": {"type": "object"}},
            {"name": "todo.rm", "params": {"id": "string"}, "returns": {"type": "object"}},
            {"name": "clip.get", "params": {"last": "number?"}, "returns": {"type": "array"}},
            {"name": "clip.add", "params": {"kind": "text|image|files", "content": "string"}, "returns": {"type": "object"}},
            {"name": "clip.clear", "params": {}, "returns": {"type": "object"}},
            {"name": "panel.show", "params": {"query": "string?"}, "returns": {"type": "object"}, "status": "M2"},
            {"name": "agent.ask", "params": {"prompt": "string", "provider": "string?"}, "returns": {"type": "object"}, "status": "M3"},
            {"name": "cmd.list", "params": {}, "returns": {"type": "array"}, "status": "M3.5"},
            {"name": "cmd.run", "params": {"id": "string", "args": "object?"}, "returns": {"type": "any"}, "status": "M3.5"},
            {"name": "cmd.tool.run", "params": {"name": "string", "args": "object?"}, "returns": {"type": "any"}, "status": "M5"},
            {"name": "cmd.tools", "params": {}, "returns": {"type": "array"}, "status": "M4"},
            {"name": "plugin.list", "params": {}, "returns": {"type": "array"}, "status": "M4"},
            {"name": "plugin.reload", "params": {}, "returns": {"type": "object"}, "status": "M4"},
            {"name": "plugin.install", "params": {"source": "string"}, "returns": {"type": "object"}, "status": "M5"},
            {"name": "plugin.remove", "params": {"id": "string"}, "returns": {"type": "object"}, "status": "M5"},
            {"name": "plugin.approve", "params": {"id": "string"}, "returns": {"type": "object"}, "status": "M5"},
            {"name": "plugin.revoke", "params": {"id": "string"}, "returns": {"type": "object"}, "status": "M5"},
            {"name": "plugin.run", "params": {"name": "string", "command": "string?", "args": "object?"}, "returns": {"type": "any"}, "status": "M4"},
            {"name": "plugin.widget", "params": {"id": "string"}, "returns": {"type": "object"}, "status": "M4"},
            {"name": "plugin.rpc", "params": {"plugin": "string", "method": "string", "params": "object?"}, "returns": {"type": "any"}, "status": "M5"},
            {"name": "skill.list", "params": {}, "returns": {"type": "array"}, "status": "M5"},
            {"name": "skill.get", "params": {"plugin": "string", "id": "string"}, "returns": {"type": "object"}, "status": "M5"},
            {"name": "audit.tail", "params": {"limit": "number?"}, "returns": {"type": "array"}, "status": "M3.5"},
            {"name": "schema", "params": {}, "returns": {"type": "object"}},
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_exposes_daemon_stop() {
        let schema = schema();
        let methods = schema["methods"].as_array().unwrap();
        assert!(methods.iter().any(|method| method["name"] == "daemon.stop"));
    }
}

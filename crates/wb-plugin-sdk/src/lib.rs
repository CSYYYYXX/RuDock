//! wb-plugin-sdk: WB 插件格式 v1 —— manifest 类型与校验。
//!
//! 一个插件 = 一个文件夹 + `plugin.json`：
//! ```json
//! {
//!   "id": "hello", "name": "Hello 示例", "version": "0.1.0",
//!   "description": "…", "author": "…",
//!   "handler": "main.ps1",
//!   "commands": [{
//!     "id": "util.hello", "title": "打招呼", "hint": "…",
//!     "arg": { "name": "name", "prompt": "跟谁打招呼？" },
//!     "ai": { "description": "…", "properties": {"name": {"type":"string"}}, "required": ["name"] }
//!   }],
//!   "widget": { "file": "widget.html", "title": "示例组件", "span": 2 }
//! }
//! ```
//! - `commands` 自动出现在三处：面板 `>` 命令模式 / AI function calling / `wb cmd run`。
//! - `handler` 契约：子进程 stdin 收 `{"command": "<id>", "args": {...}}`，stdout 吐一个 JSON 值。
//! - `widget` 是单文件 HTML（内联 style/script），以 sandboxed iframe 装进面板，
//!   内置 `wbRpc(method, params)` 桥可调 daemon 能力。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// 插件 id：小写字母/数字/中划线（文件夹名建议一致）
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// 命令处理器（相对插件目录）。有 commands 时必填。
    #[serde(default)]
    pub handler: Option<String>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub widget: Option<WidgetSpec>,
    /// Agent 可读取的 Skill 文档（相对插件目录）。
    #[serde(default)]
    pub skills: Vec<SkillSpec>,
    /// 安装后需由用户批准的能力。版本或权限集合变化会使授权失效。
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    /// 命令 id，点分层级，如 "util.hello"。注册进 cmd.list / cmd.run / AI 工具。
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub hint: String,
    /// 主参数（`>` 模式与 CLI --arg 用）；None = 无参命令
    #[serde(default)]
    pub arg: Option<ArgSpec>,
    /// 暴露给 AI 的工具描述；None = 不给模型用（破坏性操作别暴露）
    #[serde(default)]
    pub ai: Option<AiSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSpec {
    pub name: String,
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSpec {
    pub description: String,
    /// JSON Schema properties 对象
    #[serde(default)]
    pub properties: serde_json::Value,
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetSpec {
    /// 单文件 HTML（相对插件目录）
    pub file: String,
    pub title: String,
    /// 卡片宽度格数 1-4，默认 2
    #[serde(default)]
    pub span: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSpec {
    /// 插件内唯一 id，建议使用小写短名，如 "triage"。
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Markdown/纯文本文件，相对插件目录。
    pub file: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Manifest {
    pub const PERMISSIONS: &'static [&'static str] = &[
        "clipboard.read",
        "clipboard.write",
        "data.read",
        "data.write",
        "panel.control",
        "network",
        "filesystem",
        "process",
        "system",
    ];

    pub fn validate(&self) -> Result<(), String> {
        let id_ok = !self.id.is_empty()
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !id_ok {
            return Err(format!("bad plugin id: {:?}（仅限小写字母/数字/中划线）", self.id));
        }
        if self.version.is_empty() {
            return Err("version 为空".into());
        }
        if self.name.is_empty() {
            return Err("name 为空".into());
        }
        let no_escape = |p: &str| !p.contains("..") && !p.starts_with(['/', '\\']) && !p.contains(':');
        if let Some(h) = &self.handler {
            if !no_escape(h) {
                return Err(format!("handler 路径非法: {h:?}"));
            }
        }
        if let Some(w) = &self.widget {
            if !no_escape(&w.file) {
                return Err(format!("widget.file 路径非法: {:?}", w.file));
            }
            if let Some(s) = w.span {
                if !(1..=4).contains(&s) {
                    return Err(format!("widget.span 越界: {s}（1-4）"));
                }
            }
        }
        let cmd_id_ok = |id: &str| {
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
        };
        let mut skill_ids = std::collections::HashSet::new();
        for s in &self.skills {
            if !cmd_id_ok(&s.id) {
                return Err(format!("bad skill id: {:?}", s.id));
            }
            if !skill_ids.insert(&s.id) {
                return Err(format!("重复 skill id: {:?}", s.id));
            }
            if s.name.is_empty() {
                return Err(format!("skill {} 缺 name", s.id));
            }
            if !no_escape(&s.file) {
                return Err(format!("skill.file 路径非法: {:?}", s.file));
            }
        }
        let mut command_ids = std::collections::HashSet::new();
        let mut tool_names = std::collections::HashSet::new();
        for c in &self.commands {
            if !cmd_id_ok(&c.id) {
                return Err(format!("bad command id: {:?}", c.id));
            }
            if !command_ids.insert(&c.id) {
                return Err(format!("重复 command id: {:?}", c.id));
            }
            if c.ai.is_some() && !tool_names.insert(Self::tool_name(&c.id)) {
                return Err(format!("AI 工具名冲突: {:?}", Self::tool_name(&c.id)));
            }
            if c.title.is_empty() {
                return Err(format!("命令 {} 缺 title", c.id));
            }
        }
        let mut permissions = std::collections::HashSet::new();
        for permission in &self.permissions {
            if !Self::PERMISSIONS.contains(&permission.as_str()) {
                return Err(format!("未知权限: {permission:?}"));
            }
            if !permissions.insert(permission) {
                return Err(format!("重复权限: {permission:?}"));
            }
        }
        if !self.commands.is_empty() && self.handler.is_none() {
            return Err("声明了 commands 但缺 handler".into());
        }
        if !self.commands.is_empty() && !self.permissions.iter().any(|p| p == "process") {
            return Err("命令插件必须声明 process 权限".into());
        }
        if self.commands.is_empty() && self.widget.is_none() && self.skills.is_empty() {
            return Err("插件既没有 commands、widget 也没有 skills".into());
        }
        Ok(())
    }

    /// AI 工具名：点换下划线（OpenAI 工具名只允许 [a-zA-Z0-9_-]）。
    pub fn tool_name(cmd_id: &str) -> String {
        cmd_id.replace('.', "_")
    }

    pub fn sorted_permissions(&self) -> Vec<String> {
        let mut permissions = self.permissions.clone();
        permissions.sort();
        permissions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Manifest {
        serde_json::from_value(serde_json::json!({
            "id": "hello", "name": "Hello", "version": "0.1.0",
            "handler": "main.ps1",
            "commands": [{"id": "util.hello", "title": "打招呼"}],
            "permissions": ["process"]
        }))
        .unwrap()
    }

    #[test]
    fn valid_minimal() {
        base().validate().unwrap();
    }

    #[test]
    fn valid_skill_manifest() {
        let mut m = base();
        m.skills.push(SkillSpec {
            id: "triage".into(),
            name: "问题分流".into(),
            description: "说明何时使用插件".into(),
            file: "SKILL.md".into(),
            tags: vec!["workflow".into()],
        });
        m.validate().unwrap();
    }

    #[test]
    fn rejects_bad_ids() {
        let mut m = base();
        m.id = "Hello!".into();
        assert!(m.validate().is_err());
        let mut m = base();
        m.commands[0].id = "大写ABC".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_path_escape() {
        let mut m = base();
        m.handler = Some("../evil.exe".into());
        assert!(m.validate().is_err());
        let mut m = base();
        m.handler = None;
        m.commands.clear();
        m.widget = Some(WidgetSpec { file: "C:/abs.html".into(), title: "x".into(), span: None });
        assert!(m.validate().is_err());
        m.widget = None;
        m.skills.push(SkillSpec {
            id: "x".into(), name: "X".into(), description: String::new(), file: "../SKILL.md".into(), tags: vec![],
        });
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_empty_plugin() {
        let mut m = base();
        m.handler = None;
        m.commands.clear();
        m.permissions.clear();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validates_permission_names_and_duplicates() {
        let mut m = base();
        m.permissions = vec!["process".into(), "clipboard.read".into(), "network".into()];
        m.validate().unwrap();

        m.permissions.push("network".into());
        assert!(m.validate().unwrap_err().contains("重复权限"));

        m.permissions = vec!["everything".into()];
        assert!(m.validate().unwrap_err().contains("未知权限"));
    }

    #[test]
    fn command_plugin_requires_process_permission() {
        let mut m = base();
        m.permissions.clear();
        assert!(m.validate().unwrap_err().contains("process"));
    }

    #[test]
    fn tool_name_is_openai_safe() {
        assert_eq!(Manifest::tool_name("util.hello"), "util_hello");
        assert_eq!(Manifest::tool_name("util.say_hi"), "util_say_hi");
    }

    #[test]
    fn rejects_duplicate_registry_names() {
        let mut m = base();
        m.commands.push(m.commands[0].clone());
        assert!(m.validate().unwrap_err().contains("重复 command id"));

        let mut m = base();
        m.commands[0].ai = Some(AiSpec { description: "x".into(), properties: serde_json::json!({}), required: vec![] });
        let mut other = m.commands[0].clone();
        other.id = "util_hello".into();
        m.commands.push(other);
        assert!(m.validate().unwrap_err().contains("AI 工具名冲突"));
    }
}

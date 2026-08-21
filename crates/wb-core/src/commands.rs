//! 命令注册表（M3.5 核心）：一套数据，三处消费——
//! - 面板 `>` 命令模式（人）：cmd.list → 模糊过滤 → 回车 cmd.run
//! - AI 工具调用（模型）：tools_json → Responses API function calling
//! - wb CLI（外部 Agent）：wb cmd list / wb cmd run
//! 破坏性操作（清剪贴板、锁屏）故意不暴露给 AI 工具。

use serde_json::Value;

pub struct CmdSpec {
    /// daemon 方法名，或特殊 id（panel.* / system.* 由 daemon 特殊处理）
    pub id: &'static str,
    pub title: &'static str,
    pub hint: &'static str,
    /// 主参数：(参数名, `>` 模式下的输入提示)；None = 无参命令
    pub arg: Option<(&'static str, &'static str)>,
    /// 暴露给 AI 的工具描述；None = 不给模型用
    pub ai_tool: Option<AiTool>,
}

pub struct AiTool {
    pub name: &'static str,
    pub description: &'static str,
    pub properties: fn() -> Value,
    pub required: &'static [&'static str],
}

pub fn registry() -> &'static [CmdSpec] {
    &REGISTRY
}

static REGISTRY: &[CmdSpec] = &[
    CmdSpec {
        id: "todo.add",
        title: "加待办",
        hint: "随手记一条待办事项",
        arg: Some(("title", "要做什么？（可继续输入）")),
        ai_tool: Some(AiTool {
            name: "todo_add",
            description: "向用户的 WB 待办列表添加一条待办事项。用户表达「提醒我/记一下要做某事」时调用。",
            properties: || serde_json::json!({
                "title": {"type": "string", "description": "待办内容"},
                "due": {"type": "string", "description": "截止时间，ISO 8601 或自然日期，可选"}
            }),
            required: &["title"],
        }),
    },
    CmdSpec {
        id: "note.add",
        title: "记笔记",
        hint: "快速记一条随手笔记",
        arg: Some(("content", "记点什么？")),
        ai_tool: Some(AiTool {
            name: "note_add",
            description: "向用户的 WB 随手记添加一条笔记。用户说「记下来/记一下这段话」时调用。",
            properties: || serde_json::json!({
                "content": {"type": "string", "description": "笔记内容"}
            }),
            required: &["content"],
        }),
    },
    CmdSpec {
        id: "search",
        title: "全局搜索",
        hint: "搜应用 / 文件 / 剪贴板 / 笔记",
        arg: Some(("query", "搜什么？")),
        ai_tool: Some(AiTool {
            name: "search",
            description: "在用户电脑全局搜索应用、文件、剪贴板历史和笔记。用户问「我电脑里有没有xxx」时调用。",
            properties: || serde_json::json!({
                "query": {"type": "string", "description": "搜索关键词"}
            }),
            required: &["query"],
        }),
    },
    CmdSpec {
        id: "clip.get",
        title: "最近剪贴板",
        hint: "查看最近的剪贴板历史",
        arg: None,
        ai_tool: Some(AiTool {
            name: "clip_get",
            description: "获取用户最近的剪贴板历史（最多 5 条）。用户问「我刚才复制了什么」时调用。",
            properties: || serde_json::json!({}),
            required: &[],
        }),
    },
    CmdSpec {
        id: "clip.clear",
        title: "清空剪贴板历史",
        hint: "删除全部剪贴板历史记录",
        arg: None,
        ai_tool: None, // 破坏性：不交给模型
    },
    CmdSpec {
        id: "panel.hide",
        title: "收起面板",
        hint: "隐藏 WB 面板",
        arg: None,
        ai_tool: Some(AiTool {
            name: "panel_hide",
            description: "隐藏 WB 面板。用户说「收起面板/退下吧」时调用。",
            properties: || serde_json::json!({}),
            required: &[],
        }),
    },
    CmdSpec {
        id: "panel.show",
        title: "呼出面板",
        hint: "显示 WB 面板（没在跑则启动）",
        arg: None,
        ai_tool: None,
    },
    CmdSpec {
        id: "panel.toggle",
        title: "切换面板显隐",
        hint: "显示 ↔ 隐藏",
        arg: None,
        ai_tool: None,
    },
    CmdSpec {
        id: "system.lock",
        title: "锁定电脑",
        hint: "立即锁定 Windows 会话",
        arg: None,
        ai_tool: None, // 破坏性：不交给模型
    },
    CmdSpec {
        id: "agent.ask",
        title: "问 AI",
        hint: "同步问一次 AI（CLI 用，面板里直接用 ? 前缀更顺）",
        arg: Some(("prompt", "问什么？")),
        ai_tool: None,
    },
];

/// `cmd.list` 的返回：页面/CLI 渲染用
pub fn list_json() -> Value {
    Value::Array(
        registry()
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "title": c.title,
                    "hint": c.hint,
                    "arg": c.arg.map(|(name, prompt)| serde_json::json!({"name": name, "prompt": prompt})),
                })
            })
            .collect(),
    )
}

/// Responses API 的 tools 数组（function calling）
pub fn tools_json() -> Value {
    Value::Array(
        registry()
            .iter()
            .filter_map(|c| {
                c.ai_tool.as_ref().map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": {
                            "type": "object",
                            "properties": (t.properties)(),
                            "required": t.required,
                            "additionalProperties": false,
                        },
                    })
                })
            })
            .collect(),
    )
}

/// AI 工具名 → daemon 方法名（参数原样透传）
pub fn tool_to_method(tool: &str) -> Option<&'static str> {
    match tool {
        "todo_add" => Some("todo.add"),
        "note_add" => Some("note.add"),
        "search" => Some("search"),
        "clip_get" => Some("clip.get"),
        "panel_hide" => Some("panel.hide"),
        _ => None,
    }
}

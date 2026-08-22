//! wb.exe — thin CLI client. Full command surface; --json everywhere;
//! semantic exit codes; structured errors; plain output when piped.

use clap::{Parser, Subcommand, ValueEnum};
use interprocess::local_socket::{prelude::*, GenericNamespaced};
use interprocess::TryClone;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use wb_core::error::{CoreError, ErrorCode};
use wb_core::protocol::{Request, Response};

#[derive(Parser)]
#[command(name = "wb", version, about = "WB — Agent-Native desktop entry for Windows")]
struct Cli {
    /// Structured JSON output
    #[arg(long, global = true)]
    json: bool,
    /// Newline-delimited JSON stream (search)
    #[arg(long, global = true)]
    ndjson: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Unified search: files/apps/clips/notes/plugins
    Search { query: String, #[arg(long)] limit: Option<usize>, #[arg(long, value_name = "TYPE")] r#type: Option<String> },
    /// Quick notes (Markdown)
    Note {
        #[command(subcommand)]
        op: NoteOp,
    },
    /// Todo list
    Todo {
        #[command(subcommand)]
        op: TodoOp,
    },
    /// Clipboard history
    Clip {
        #[command(subcommand)]
        op: ClipOp,
    },
    /// Plugins
    Plugin {
        #[command(subcommand)]
        op: PluginOp,
    },
    /// Agent Skills bundled by plugins
    Skill {
        #[command(subcommand)]
        op: SkillOp,
    },
    /// Panel control
    Panel {
        #[command(subcommand)]
        op: PanelOp,
    },
    /// WB settings
    Settings {
        #[command(subcommand)]
        op: SettingsOp,
    },
    /// Ask AI
    Agent {
        #[command(subcommand)]
        op: AgentOp,
    },
    /// Command registry (shared with panel `>` mode and AI tools)
    Cmd {
        #[command(subcommand)]
        op: CmdOp,
    },
    /// Print the machine-readable command schema
    Schema,
    /// Daemon lifecycle
    Daemon {
        #[command(subcommand)]
        op: DaemonOp,
    },
    /// List indexed launcher apps (.lnk + UWP)
    Apps,
    /// Tail the audit log
    Audit,
    /// Generate configuration for external MCP clients
    Mcp {
        #[command(subcommand)]
        op: McpOp,
    },
}

#[derive(Subcommand)]
enum NoteOp {
    Add { content: String, #[arg(long)] tag: Vec<String> },
    List { #[arg(long)] limit: Option<usize> },
    Get { id: String },
    Rm { id: String },
}

#[derive(Subcommand)]
enum TodoOp {
    Add { title: String, #[arg(long)] due: Option<String>, #[arg(long)] repeat: Option<String>, #[arg(long)] tag: Vec<String> },
    List { #[arg(long)] all: bool },
    Done { id: String },
    Rm { id: String },
}

#[derive(Subcommand)]
enum ClipOp {
    Get { #[arg(long)] last: Option<usize> },
    Add { content: String, #[arg(long, default_value = "text")] kind: String },
    Clear,
}

#[derive(Subcommand)]
enum PluginOp {
    List,
    Reload,
    Install { source: String },
    Remove { id: String },
    Approve { id: String },
    Revoke { id: String },
    Pack { dir: String, #[arg(short, long)] output: Option<String> },
    Run { name: String, #[arg(long)] command: Option<String>, #[arg(long = "arg", value_parser = parse_kv)] args: Vec<(String, String)> },
}

#[derive(Subcommand)]
enum SkillOp {
    List,
    Get { plugin: String, id: String },
}

#[derive(Subcommand)]
enum PanelOp {
    Show { #[arg(long)] query: Option<String> },
    Hide,
    Toggle,
}

#[derive(Subcommand)]
enum SettingsOp {
    Get,
    Win { #[arg(action = clap::ArgAction::Set)] enabled: bool },
    Autostart { #[arg(action = clap::ArgAction::Set)] enabled: bool },
}

#[derive(Subcommand)]
enum AgentOp {
    Ask { prompt: String, #[arg(long)] provider: Option<String> },
}

#[derive(Subcommand)]
enum CmdOp {
    List,
    Run { id: String, #[arg(long = "arg", value_parser = parse_kv)] args: Vec<(String, String)> },
}

#[derive(Subcommand)]
enum DaemonOp {
    Start,
    Stop,
    Status,
}

#[derive(Subcommand)]
enum McpOp {
    /// Print a client configuration snippet using the current wb-mcp.exe
    Config { #[arg(value_enum, default_value_t = McpClient::Generic)] client: McpClient },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum McpClient {
    Claude,
    Cursor,
    Codex,
    Generic,
}

impl std::fmt::Display for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Generic => "generic",
        })
    }
}

fn mcp_config(client: McpClient) -> Result<String, CoreError> {
    let exe = std::env::current_exe()
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("定位 wb.exe 失败: {e}")))?
        .parent()
        .map(|p| p.join("wb-mcp.exe"))
        .ok_or_else(|| CoreError::new(ErrorCode::Internal, "定位 wb-mcp.exe 失败"))?;
    if !exe.is_file() {
        return Err(CoreError::new(ErrorCode::NotFound, format!("wb-mcp.exe 不在 {}", exe.display())));
    }
    let path = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let json = format!(
        "{{\n  \"mcpServers\": {{\n    \"wb\": {{\n      \"command\": \"{}\",\n      \"args\": []\n    }}\n  }}\n}}",
        path
    );
    let toml = format!("[mcp_servers.wb]\ncommand = \"{}\"\nargs = []\n", path);
    Ok(match client {
        McpClient::Codex => toml,
        McpClient::Claude | McpClient::Cursor | McpClient::Generic => json,
    })
}

fn parse_kv(s: &str) -> Result<(String, String), std::convert::Infallible> {
    let (k, v) = s.split_once('=').unwrap_or((s, ""));
    Ok((k.to_string(), v.to_string()))
}

#[cfg(windows)]
fn pack_plugin(dir: &str, output: Option<&str>) -> Result<serde_json::Value, CoreError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::path::PathBuf::from(dir)
        .canonicalize()
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("插件目录不存在: {dir} ({e})")))?;
    let root = if root.to_string_lossy().starts_with(r"\\?\") {
        std::path::PathBuf::from(&root.to_string_lossy()[4..])
    } else {
        root
    };
    if !root.is_dir() {
        return Err(CoreError::new(ErrorCode::InvalidParams, "pack 需要插件目录"));
    }
    let manifest_text = std::fs::read_to_string(root.join("plugin.json"))
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("读取 plugin.json 失败: {e}")))?;
    let manifest: wb_plugin_sdk::Manifest = serde_json::from_str(&manifest_text)
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("plugin.json 格式错误: {e}")))?;
    manifest.validate().map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("插件校验失败: {e}")))?;
    let out = output
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.parent().unwrap_or(&root).join(format!("{}-{}.zip", manifest.id, manifest.version)));
    let out = if out.is_absolute() { out } else { std::env::current_dir().unwrap_or_default().join(out) };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("创建输出目录失败: {e}")))?;
    }
    let ps_quote = |p: &std::path::Path| format!("'{}'", p.to_string_lossy().replace('\'', "''"));
    let script = format!(
        "Compress-Archive -Path (Join-Path {} '*') -DestinationPath {} -Force -ErrorAction Stop",
        ps_quote(&root), ps_quote(&out)
    );
    let outp = std::process::Command::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("打包插件失败: {e}")))?;
    if !outp.status.success() {
        let detail = String::from_utf8_lossy(&outp.stderr).trim().to_string();
        return Err(CoreError::new(ErrorCode::Internal, if detail.is_empty() { "打包插件失败".into() } else { detail }));
    }
    Ok(serde_json::json!({"packed": manifest.id, "version": manifest.version, "output": out}))
}

#[cfg(not(windows))]
fn pack_plugin(_dir: &str, _output: Option<&str>) -> Result<serde_json::Value, CoreError> {
    Err(CoreError::new(ErrorCode::Unimplemented, "plugin pack 仅支持 Windows"))
}

// ---------------- IPC client ----------------

struct Client {
    reader: BufReader<interprocess::local_socket::Stream>,
    writer: interprocess::local_socket::Stream,
}

impl Client {
    fn connect() -> Result<Self, CoreError> {
        let name = wb_core::paths::pipe_name()
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("pipe name: {e}")))?;
        match interprocess::local_socket::Stream::connect(name.clone()) {
            Ok(s) => Ok(Self {
                reader: BufReader::new(s.try_clone().map_err(io_err)?),
                writer: s,
            }),
            Err(_) => {
                spawn_daemon();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    match interprocess::local_socket::Stream::connect(name.clone()) {
                        Ok(s) => {
                            return Ok(Self {
                                reader: BufReader::new(s.try_clone().map_err(io_err)?),
                                writer: s,
                            })
                        }
                        Err(e) => {
                            if std::time::Instant::now() > deadline {
                                return Err(CoreError::new(
                                    ErrorCode::DaemonUnavailable,
                                    format!("cannot reach daemon: {e}"),
                                )
                                .with_hint("try `wb daemon start`"));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(80));
                        }
                    }
                }
            }
        }
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, CoreError> {
        let req = Request { jsonrpc: "2.0".into(), id: serde_json::json!(1), method: method.into(), params };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).map_err(io_err)?;
        self.writer.flush().map_err(io_err)?;
        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf).map_err(io_err)?;
        if n == 0 {
            return Err(CoreError::new(ErrorCode::DaemonUnavailable, "daemon closed connection"));
        }
        let resp: Response = serde_json::from_str(buf.trim())?;
        if let Some(err) = resp.error {
            let code = serde_json::from_value::<ErrorCode>(err.get("code").cloned().unwrap_or(serde_json::json!("INTERNAL")))
                .unwrap_or(ErrorCode::Internal);
            let mut e = CoreError::new(code, err.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string());
            e.hint = err.get("hint").and_then(|h| h.as_str()).map(String::from);
            return Err(e);
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}

fn io_err(e: std::io::Error) -> CoreError {
    CoreError::new(ErrorCode::DaemonUnavailable, format!("io: {e}"))
}

#[cfg(windows)]
fn spawn_daemon() {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wb-daemon.exe")))
        .unwrap_or_else(|| "wb-daemon.exe".into());
    let _ = std::process::Command::new(exe)
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(not(windows))]
fn spawn_daemon() {}

// ---------------- output contract ----------------

fn emit(value: &serde_json::Value, json: bool, ndjson: bool) {
    if ndjson {
        if let Some(arr) = value.as_array() {
            for item in arr {
                println!("{}", serde_json::to_string(item).unwrap_or_default());
            }
            return;
        }
    }
    if json || !std::io::stdout().is_terminal() {
        println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
        return;
    }
    // Human mode: pretty table-ish rendering.
    match value {
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let title = item.get("title").or_else(|| item.get("content")).and_then(|v| v.as_str()).unwrap_or("");
                let sub = item.get("subtitle").and_then(|v| v.as_str()).unwrap_or("");
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                println!("{:>3}. {}  {}{}", i + 1, title, sub, if id.is_empty() { String::new() } else { format!("  [{id}]") });
            }
        }
        other => println!("{}", serde_json::to_string_pretty(other).unwrap_or_default()),
    }
}

fn fail(e: &CoreError, json: bool) -> ! {
    if json || !std::io::stdout().is_terminal() {
        eprintln!("{}", serde_json::to_string(&e.to_envelope()).unwrap_or_default());
    } else {
        eprintln!("error: {}", e.message);
        if let Some(h) = &e.hint {
            eprintln!("hint: {h}");
        }
    }
    std::process::exit(e.code.exit_code());
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    let ndjson = cli.ndjson;

    if let Cmd::Plugin { op: PluginOp::Pack { dir, output } } = &cli.cmd {
        match pack_plugin(dir, output.as_deref()) {
            Ok(v) => emit(&v, json, false),
            Err(e) => fail(&e, json),
        }
        return;
    }

    if let Cmd::Schema = cli.cmd {
        emit(&wb_core::protocol::schema(), true, false);
        return;
    }
    if let Cmd::Mcp { op: McpOp::Config { client } } = &cli.cmd {
        match mcp_config(*client) {
            Ok(config) if json => emit(&serde_json::json!({"client": client.to_string(), "config": config}), true, false),
            Ok(config) => print!("{config}"),
            Err(e) => fail(&e, json),
        }
        return;
    }
    if let Cmd::Daemon { op: DaemonOp::Start } = cli.cmd {
        spawn_daemon();
        match Client::connect().and_then(|mut c| c.call("daemon.ping", serde_json::json!({}))) {
            Ok(v) => {
                emit(&v, json, false);
                return;
            }
            Err(e) => fail(&e, json),
        }
    }

    let mut client = match Client::connect() {
        Ok(c) => c,
        Err(e) => fail(&e, json),
    };

    let (method, params) = match cli.cmd {
        Cmd::Search { query, limit, r#type } => (
            "search",
            serde_json::json!({"query": query, "limit": limit.unwrap_or(20), "type": r#type}),
        ),
        Cmd::Note { op } => match op {
            NoteOp::Add { content, tag } => ("note.add", serde_json::json!({"content": content, "tags": tag})),
            NoteOp::List { limit } => ("note.list", serde_json::json!({"limit": limit.unwrap_or(50)})),
            NoteOp::Get { id } => ("note.get", serde_json::json!({"id": id})),
            NoteOp::Rm { id } => ("note.rm", serde_json::json!({"id": id})),
        },
        Cmd::Todo { op } => match op {
            TodoOp::Add { title, due, repeat, tag } => {
                ("todo.add", serde_json::json!({"title": title, "due": due, "repeat": repeat, "tags": tag}))
            }
            TodoOp::List { all } => ("todo.list", serde_json::json!({"all": all})),
            TodoOp::Done { id } => ("todo.done", serde_json::json!({"id": id})),
            TodoOp::Rm { id } => ("todo.rm", serde_json::json!({"id": id})),
        },
        Cmd::Clip { op } => match op {
            ClipOp::Get { last } => ("clip.get", serde_json::json!({"last": last.unwrap_or(10)})),
            ClipOp::Add { content, kind } => ("clip.add", serde_json::json!({"kind": kind, "content": content})),
            ClipOp::Clear => ("clip.clear", serde_json::json!({})),
        },
        Cmd::Plugin { op } => match op {
            PluginOp::List => ("plugin.list", serde_json::json!({})),
            PluginOp::Reload => ("plugin.reload", serde_json::json!({})),
            PluginOp::Install { source } => ("plugin.install", serde_json::json!({"source": source})),
            PluginOp::Remove { id } => ("plugin.remove", serde_json::json!({"id": id})),
            PluginOp::Approve { id } => ("plugin.approve", serde_json::json!({"id": id})),
            PluginOp::Revoke { id } => ("plugin.revoke", serde_json::json!({"id": id})),
            PluginOp::Pack { .. } => unreachable!(),
            PluginOp::Run { name, command, args } => {
                let obj: serde_json::Map<String, serde_json::Value> =
                    args.into_iter().map(|(k, v)| (k, serde_json::Value::String(v))).collect();
                ("plugin.run", serde_json::json!({"name": name, "command": command, "args": obj}))
            }
        },
        Cmd::Skill { op } => match op {
            SkillOp::List => ("skill.list", serde_json::json!({})),
            SkillOp::Get { plugin, id } => ("skill.get", serde_json::json!({"plugin": plugin, "id": id})),
        },
        Cmd::Panel { op } => match op {
            PanelOp::Show { query } => ("panel.show", serde_json::json!({"query": query})),
            PanelOp::Hide => ("panel.hide", serde_json::json!({})),
            PanelOp::Toggle => ("panel.toggle", serde_json::json!({})),
        },
        Cmd::Settings { op } => match op {
            SettingsOp::Get => ("settings.get", serde_json::json!({})),
            SettingsOp::Win { enabled } => ("settings.set", serde_json::json!({"takeover_win":enabled})),
            SettingsOp::Autostart { enabled } => ("settings.set", serde_json::json!({"autostart":enabled})),
        },
        Cmd::Agent { op } => match op {
            AgentOp::Ask { prompt, provider } => ("agent.ask", serde_json::json!({"prompt": prompt, "provider": provider})),
        },
        Cmd::Cmd { op } => match op {
            CmdOp::List => ("cmd.list", serde_json::json!({})),
            CmdOp::Run { id, args } => {
                let obj: serde_json::Map<String, serde_json::Value> =
                    args.into_iter().map(|(k, v)| (k, serde_json::Value::String(v))).collect();
                ("cmd.run", serde_json::json!({"id": id, "args": obj}))
            }
        },
        Cmd::Audit => ("audit.tail", serde_json::json!({})),
        Cmd::Apps => ("apps.list", serde_json::json!({})),
        Cmd::Daemon { op } => match op {
            DaemonOp::Status => ("daemon.ping", serde_json::json!({})),
            DaemonOp::Stop => {
                eprintln!("{}", serde_json::to_string(&CoreError::new(ErrorCode::Unimplemented, "daemon stop lands with tray/service work").to_envelope()).unwrap_or_default());
                std::process::exit(5);
            }
            DaemonOp::Start => unreachable!(),
        },
        Cmd::Schema => unreachable!(),
        Cmd::Mcp { .. } => unreachable!(),
    };

    match client.call(method, params) {
        Ok(v) => emit(&v, json, ndjson),
        Err(e) => fail(&e, json),
    }
}

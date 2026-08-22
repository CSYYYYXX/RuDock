//! wb.exe — thin CLI client. Full command surface; --json everywhere;
//! semantic exit codes; structured errors; plain output when piped.

use clap::{Parser, Subcommand, ValueEnum};
use interprocess::local_socket::{prelude::*, GenericNamespaced};
use interprocess::TryClone;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
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
    /// Create an Agent-ready plugin scaffold without overwriting existing files
    Create {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_enum, default_value_t = PluginKind::Command)]
        kind: PluginKind,
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Validate a plugin manifest and every declared local file
    Validate { dir: String },
    Install {
        source: String,
        /// Required for HTTP(S); accepts 64 hex characters or sha256:<hex>
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
    },
    Remove { id: String },
    Approve { id: String },
    Revoke { id: String },
    Pack { dir: String, #[arg(short, long)] output: Option<String> },
    Run { name: String, #[arg(long)] command: Option<String>, #[arg(long = "arg", value_parser = parse_kv)] args: Vec<(String, String)> },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PluginKind {
    Command,
    Widget,
    Hybrid,
}

impl PluginKind {
    fn has_command(self) -> bool {
        matches!(self, Self::Command | Self::Hybrid)
    }

    fn has_widget(self) -> bool {
        matches!(self, Self::Widget | Self::Hybrid)
    }
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

fn load_local_plugin(dir: &str) -> Result<wb_plugin_host::LoadedPlugin, CoreError> {
    let root = std::path::PathBuf::from(dir)
        .canonicalize()
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("插件目录不存在: {dir} ({e})")))?;
    let root = if root.to_string_lossy().starts_with(r"\\?\") {
        std::path::PathBuf::from(&root.to_string_lossy()[4..])
    } else {
        root
    };
    if !root.is_dir() {
        return Err(CoreError::new(ErrorCode::InvalidParams, "需要插件目录"));
    }
    let manifest_text = std::fs::read_to_string(root.join("plugin.json"))
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("读取 plugin.json 失败: {e}")))?;
    let manifest: wb_plugin_sdk::Manifest = serde_json::from_str(&manifest_text)
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("plugin.json 格式错误: {e}")))?;
    manifest.validate().map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("插件校验失败: {e}")))?;
    let plugin = wb_plugin_host::LoadedPlugin { dir: root, manifest };
    wb_plugin_host::validate_files(&plugin)
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("插件文件校验失败: {e}")))?;
    Ok(plugin)
}

fn validate_plugin(dir: &str) -> Result<serde_json::Value, CoreError> {
    let plugin = load_local_plugin(dir)?;
    let tools: Vec<String> = plugin
        .manifest
        .commands
        .iter()
        .filter(|command| command.ai.is_some())
        .map(|command| wb_plugin_sdk::Manifest::tool_name(&command.id))
        .collect();
    Ok(serde_json::json!({
        "valid": true,
        "id": plugin.manifest.id,
        "name": plugin.manifest.name,
        "version": plugin.manifest.version,
        "permissions": plugin.manifest.sorted_permissions(),
        "commands": plugin.manifest.commands.iter().map(|command| &command.id).collect::<Vec<_>>(),
        "tools": tools,
        "widget": plugin.manifest.widget.is_some(),
        "skills": plugin.manifest.skills.iter().map(|skill| &skill.id).collect::<Vec<_>>(),
        "dir": plugin.dir,
    }))
}

fn html_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn create_plugin(
    id: &str,
    name: Option<&str>,
    kind: PluginKind,
    output: Option<&str>,
) -> Result<serde_json::Value, CoreError> {
    let name = name.unwrap_or(id);
    let command_id = format!("{id}.run");
    let commands = if kind.has_command() {
        vec![serde_json::json!({
            "id": command_id,
            "title": format!("Run {name}"),
            "hint": format!("Run the {name} plugin"),
            "arg": {"name": "name", "prompt": "Name"},
            "ai": {
                "description": format!("Use {name} to greet a named person."),
                "properties": {"name": {"type": "string", "description": "Person to greet"}},
                "required": ["name"]
            }
        })]
    } else {
        Vec::new()
    };
    let widget = kind.has_widget().then(|| serde_json::json!({
        "file": "widget.html", "title": name, "span": 2
    }));
    let permissions = if kind.has_command() { vec!["process"] } else { Vec::new() };
    let manifest: wb_plugin_sdk::Manifest = serde_json::from_value(serde_json::json!({
        "id": id,
        "name": name,
        "version": "0.1.0",
        "description": format!("WB plugin scaffold for {name}."),
        "author": "",
        "handler": kind.has_command().then_some("main.ps1"),
        "commands": commands,
        "widget": widget,
        "skills": [{
            "id": "usage",
            "name": format!("{name} Usage"),
            "description": format!("How an Agent should use {name}."),
            "file": "SKILL.md",
            "tags": ["workflow"]
        }],
        "permissions": permissions
    }))
    .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("生成 manifest 失败: {e}")))?;
    manifest.validate().map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("插件 id/name 无效: {e}")))?;

    let target = output
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(id));
    let target = if target.is_absolute() {
        target
    } else {
        std::env::current_dir()
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("读取当前目录失败: {e}")))?
            .join(target)
    };
    if target.exists() {
        return Err(CoreError::new(
            ErrorCode::InvalidParams,
            format!("输出目录已存在，拒绝覆盖: {}", target.display()),
        ));
    }
    std::fs::create_dir_all(&target)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建插件目录失败: {e}")))?;
    std::fs::write(
        target.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("序列化 manifest 失败: {e}")))?,
    )
    .map_err(|e| CoreError::new(ErrorCode::Internal, format!("写 plugin.json 失败: {e}")))?;

    if kind.has_command() {
        let script = r#"$request = [Console]::In.ReadToEnd() | ConvertFrom-Json
$name = if ($request.args.name) { [string]$request.args.name } else { "World" }
@{ text = "Hello, $name!"; plugin = "__PLUGIN_ID__" } | ConvertTo-Json -Compress
"#
        .replace("__PLUGIN_ID__", id);
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(script.as_bytes());
        std::fs::write(target.join("main.ps1"), bytes)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("写 main.ps1 失败: {e}")))?;
    }
    if kind.has_widget() {
        let widget = format!(
            r#"<!doctype html><meta charset="utf-8"><style>
html,body{{margin:0;height:100%;background:transparent;color:#eaf0ff;font:13px "Segoe UI",sans-serif}}
.root{{height:100%;display:grid;place-content:center;text-align:center}}.time{{font-size:32px;font-weight:300}}.name{{opacity:.65;margin-top:6px}}
</style><div class="root"><div><div class="time" id="time"></div><div class="name">{}</div></div></div>
<script>const time=document.getElementById('time');function tick(){{time.textContent=new Date().toLocaleTimeString([],{{hour:'2-digit',minute:'2-digit'}})}}tick();setInterval(tick,1000)</script>
"#,
            html_escape(name)
        );
        std::fs::write(target.join("widget.html"), widget)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("写 widget.html 失败: {e}")))?;
    }
    let skill = if kind.has_command() {
        format!(
            "# {name}\n\nUse this skill when the user asks for a greeting. Call `{command_id}` with a `name` string.\n"
        )
    } else {
        format!("# {name}\n\nThis plugin provides the `{name}` dashboard widget.\n")
    };
    std::fs::write(target.join("SKILL.md"), skill)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("写 SKILL.md 失败: {e}")))?;

    validate_plugin(&target.to_string_lossy()).map(|mut summary| {
        if let Some(object) = summary.as_object_mut() {
            object.insert("created".into(), serde_json::Value::Bool(true));
        }
        summary
    })
}

#[cfg(windows)]
fn pack_plugin(dir: &str, output: Option<&str>) -> Result<serde_json::Value, CoreError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let plugin = load_local_plugin(dir)?;
    let root = plugin.dir;
    let manifest = plugin.manifest;
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
    let sha256 = archive_sha256(&out)?;
    Ok(serde_json::json!({
        "packed": manifest.id,
        "version": manifest.version,
        "output": out,
        "sha256": format!("sha256:{sha256}"),
    }))
}

fn archive_sha256(path: &std::path::Path) -> Result<String, CoreError> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| {
        CoreError::new(
            ErrorCode::Internal,
            format!("读取插件归档失败 {}: {e}", path.display()),
        )
    })?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("计算 SHA-256 失败: {e}")))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
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
    fn connect_existing() -> Result<Self, CoreError> {
        let name = wb_core::paths::pipe_name()
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("pipe name: {e}")))?;
        let stream = interprocess::local_socket::Stream::connect(name).map_err(|e| {
            CoreError::new(
                ErrorCode::DaemonUnavailable,
                format!("cannot reach daemon: {e}"),
            )
        })?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone().map_err(io_err)?),
            writer: stream,
        })
    }

    fn connect() -> Result<Self, CoreError> {
        if let Ok(client) = Self::connect_existing() {
            return Ok(client);
        }
        spawn_daemon();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match Self::connect_existing() {
                Ok(client) => return Ok(client),
                Err(e) => {
                    if std::time::Instant::now() > deadline {
                        return Err(e.with_hint("try `wb daemon start`"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(80));
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

    let local_plugin_result = match &cli.cmd {
        Cmd::Plugin { op: PluginOp::Create { id, name, kind, output } } => {
            Some(create_plugin(id, name.as_deref(), *kind, output.as_deref()))
        }
        Cmd::Plugin { op: PluginOp::Validate { dir } } => Some(validate_plugin(dir)),
        Cmd::Plugin { op: PluginOp::Pack { dir, output } } => Some(pack_plugin(dir, output.as_deref())),
        _ => None,
    };
    if let Some(result) = local_plugin_result {
        match result {
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
        match Client::connect().and_then(|mut c| c.call("daemon.ping", serde_json::json!({}))) {
            Ok(v) => {
                emit(&v, json, false);
                return;
            }
            Err(e) => fail(&e, json),
        }
    }
    if let Cmd::Daemon { op } = &cli.cmd {
        match op {
            DaemonOp::Status => {
                let value = match Client::connect_existing() {
                    Ok(mut client) => match client.call("daemon.ping", serde_json::json!({})) {
                        Ok(value) => value,
                        Err(e) => fail(&e, json),
                    },
                    Err(_) => serde_json::json!({"status":"stopped"}),
                };
                emit(&value, json, false);
                return;
            }
            DaemonOp::Stop => {
                let value = match Client::connect_existing() {
                    Ok(mut client) => match client.call("daemon.stop", serde_json::json!({})) {
                        Ok(value) => value,
                        Err(e) => fail(&e, json),
                    },
                    Err(_) => serde_json::json!({"status":"stopped","already_stopped":true}),
                };
                emit(&value, json, false);
                return;
            }
            DaemonOp::Start => unreachable!(),
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
            PluginOp::Install { source, sha256 } => (
                "plugin.install",
                serde_json::json!({"source": source, "sha256": sha256}),
            ),
            PluginOp::Remove { id } => ("plugin.remove", serde_json::json!({"id": id})),
            PluginOp::Approve { id } => ("plugin.approve", serde_json::json!({"id": id})),
            PluginOp::Revoke { id } => ("plugin.revoke", serde_json::json!({"id": id})),
            PluginOp::Create { .. } | PluginOp::Validate { .. } | PluginOp::Pack { .. } => unreachable!(),
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
        Cmd::Daemon { .. } => unreachable!(),
        Cmd::Schema => unreachable!(),
        Cmd::Mcp { .. } => unreachable!(),
    };

    match client.call(method, params) {
        Ok(v) => emit(&v, json, ndjson),
        Err(e) => fail(&e, json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn creates_valid_runnable_hybrid_scaffold_without_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "wb-cli-create-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = root.join("hello-world");
        let path = target.to_string_lossy();
        let summary = create_plugin("hello-world", Some("Hello World"), PluginKind::Hybrid, Some(&path)).unwrap();
        assert_eq!(summary["valid"], true);
        assert_eq!(summary["created"], true);
        assert_eq!(summary["widget"], true);
        assert_eq!(summary["commands"][0], "hello-world.run");
        assert_eq!(summary["skills"][0], "usage");

        let plugin = load_local_plugin(&path).unwrap();
        let handler = std::fs::read(target.join("main.ps1")).unwrap();
        assert!(handler.starts_with(&[0xef, 0xbb, 0xbf]));
        let widget = std::fs::read_to_string(target.join("widget.html")).unwrap();
        assert!(widget.contains("</script>"));
        let result = wb_plugin_host::run_command(
            &plugin,
            "hello-world.run",
            &serde_json::json!({"name": "WB"}),
        )
        .unwrap();
        assert_eq!(result["text"], "Hello, WB!");

        let error = create_plugin("hello-world", None, PluginKind::Command, Some(&path)).unwrap_err();
        assert!(error.message.contains("拒绝覆盖"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parses_checksummed_remote_plugin_install() {
        let hash = "a".repeat(64);
        let cli = Cli::try_parse_from([
            "wb",
            "plugin",
            "install",
            "https://plugins.example/test.zip",
            "--sha256",
            &hash,
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Plugin {
                op: PluginOp::Install { source, sha256 },
            } => {
                assert_eq!(source, "https://plugins.example/test.zip");
                assert_eq!(sha256.as_deref(), Some(hash.as_str()));
            }
            _ => panic!("unexpected command"),
        }
    }
}

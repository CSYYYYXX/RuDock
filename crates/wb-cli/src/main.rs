//! wb.exe — thin CLI client. Full command surface; --json everywhere;
//! semantic exit codes; structured errors; plain output when piped.

use clap::{Parser, Subcommand, ValueEnum};
use interprocess::local_socket::{prelude::*, GenericNamespaced};
use interprocess::TryClone;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
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
#[allow(clippy::enum_variant_names)]
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
    /// Read incremental daemon events with an optional long-poll wait
    Events {
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        wait_ms: u64,
    },
    /// Generate configuration for external MCP clients
    Mcp {
        #[command(subcommand)]
        op: McpOp,
    },
    /// Create a consistent local backup of RuDock data
    Backup {
        #[command(subcommand)]
        op: BackupOp,
    },
    /// Export a redacted support bundle without personal content
    Diagnostics {
        #[command(subcommand)]
        op: DiagnosticsOp,
    },
    /// Check the latest RuDock release without changing local files
    Update {
        #[command(subcommand)]
        op: UpdateOp,
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
    /// Discover, install, and update plugins from a versioned market index
    Market {
        #[command(subcommand)]
        op: MarketOp,
    },
    Remove { id: String },
    Approve { id: String },
    Revoke { id: String },
    Pack { dir: String, #[arg(short, long)] output: Option<String> },
    Run { name: String, #[arg(long)] command: Option<String>, #[arg(long = "arg", value_parser = parse_kv)] args: Vec<(String, String)> },
}

#[derive(Subcommand)]
enum MarketOp {
    /// Manage persistent official/community market sources
    Source {
        #[command(subcommand)]
        op: MarketSourceOp,
    },
    /// List catalog entries and their installed/update status
    List {
        #[arg(long)]
        index: Option<String>,
    },
    /// List only plugins with a newer SemVer release
    Check {
        #[arg(long)]
        index: Option<String>,
    },
    /// Install the catalog release and verify id, version, and SHA-256
    Install {
        id: String,
        #[arg(long)]
        index: Option<String>,
    },
    /// Upgrade an installed plugin when the catalog has a newer release
    Update {
        id: String,
        #[arg(long)]
        index: Option<String>,
    },
}

#[derive(Subcommand)]
enum MarketSourceOp {
    List,
    Add { index: String },
    Remove { index: String },
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
    /// Interface language; auto follows the Windows display language
    Language { #[arg(value_enum)] language: InterfaceLanguage },
    /// Widgets pinned to the desktop; pass no ids to disable the desktop host
    Desktop { widgets: Vec<String> },
    /// MCP write handling: trust client prompts, require elicitation, or block writes
    Mcp { #[arg(value_enum)] policy: McpWritePolicy },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum InterfaceLanguage {
    Auto,
    #[value(name = "zh-CN", alias = "zh-cn", alias = "zh")]
    ZhCn,
    En,
    Ja,
    Ko,
}

impl InterfaceLanguage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ZhCn => "zh-CN",
            Self::En => "en",
            Self::Ja => "ja",
            Self::Ko => "ko",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum McpWritePolicy {
    Client,
    Ask,
    ReadOnly,
}

impl McpWritePolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Ask => "ask",
            Self::ReadOnly => "read-only",
        }
    }
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
    /// Install RuDock into an MCP client's configuration
    Install {
        #[arg(value_enum)]
        client: McpClient,
        /// Override the client's default configuration path
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Replace an existing mcp server named "wb"
        #[arg(long)]
        force: bool,
    },
    /// Inspect RuDock's entry in an MCP client configuration
    Status {
        #[arg(value_enum)]
        client: McpClient,
        /// Override the client's default configuration path
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
    /// Remove RuDock from an MCP client's configuration
    Uninstall {
        #[arg(value_enum)]
        client: McpClient,
        /// Override the client's default configuration path
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Remove a conflicting mcp server named "wb"
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum BackupOp {
    /// Back up the SQLite database, settings, and user-installed plugins
    Create {
        /// Destination ZIP path; defaults to the Downloads folder
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Restore a backup after validating it and creating a rollback copy
    Restore { archive: PathBuf },
}

#[derive(Subcommand)]
enum DiagnosticsOp {
    /// Create a redacted support ZIP
    Create {
        /// Destination ZIP path; defaults to the Downloads folder
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum UpdateOp {
    /// Check GitHub for a newer stable RuDock release
    Check,
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

fn mcp_executable() -> Result<PathBuf, CoreError> {
    let current = std::env::current_exe()
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("定位 wb.exe 失败: {e}")))?;
    let parent = current.parent().ok_or_else(|| CoreError::new(ErrorCode::Internal, "定位 wb-mcp.exe 失败"))?;
    let direct = parent.join("wb-mcp.exe");
    if direct.is_file() {
        return Ok(direct);
    }
    let test_build = if parent.file_name().and_then(|name| name.to_str()) == Some("deps") {
        parent.parent().map(|p| p.join("wb-mcp.exe"))
    } else {
        None
    };
    if let Some(exe) = test_build.filter(|p| p.is_file()) {
        return Ok(exe);
    }
    Err(CoreError::new(ErrorCode::NotFound, format!("wb-mcp.exe 不在 {}", direct.display())))
}

fn mcp_config(client: McpClient) -> Result<String, CoreError> {
    let exe = mcp_executable()?;
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {"wb": {"command": exe, "args": []}}
    }))
    .map_err(|e| CoreError::new(ErrorCode::Internal, format!("生成 MCP JSON 失败: {e}")))?
        + "\n";
    let mut doc = toml_edit::DocumentMut::new();
    doc["mcp_servers"]["wb"]["command"] = toml_edit::value(exe.to_string_lossy().as_ref());
    doc["mcp_servers"]["wb"]["args"] = toml_edit::value(toml_edit::Array::new());
    let toml = doc.to_string();
    Ok(match client {
        McpClient::Codex => toml,
        McpClient::Claude | McpClient::Cursor | McpClient::Generic => json,
    })
}

fn mcp_config_path(client: McpClient, file: Option<&Path>) -> Result<PathBuf, CoreError> {
    if let Some(path) = file {
        return Ok(path.to_path_buf());
    }
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).ok_or_else(|| {
        CoreError::new(ErrorCode::NotFound, "未找到 USERPROFILE；请使用 --file 指定配置文件")
    })?;
    match client {
        McpClient::Codex => Ok(home.join(".codex").join("config.toml")),
        McpClient::Cursor => Ok(home.join(".cursor").join("mcp.json")),
        McpClient::Claude => std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("Claude").join("claude_desktop_config.json"))
            .ok_or_else(|| CoreError::new(ErrorCode::NotFound, "未找到 APPDATA；请使用 --file 指定配置文件")),
        McpClient::Generic => Err(CoreError::new(
            ErrorCode::InvalidParams,
            "generic 客户端没有默认配置路径；请使用 --file",
        )),
    }
}

fn read_config(path: &Path) -> Result<Option<String>, CoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CoreError::new(ErrorCode::Internal, format!("读取 {} 失败: {e}", path.display()))),
    }
}

fn same_mcp_command(actual: &str, expected: &Path) -> bool {
    let actual = Path::new(actual);
    match (std::fs::canonicalize(actual), std::fs::canonicalize(expected)) {
        (Ok(actual), Ok(expected)) => actual == expected,
        _ => actual == expected,
    }
}

fn json_entry_owned(entry: &serde_json::Value, exe: &Path) -> bool {
    entry.get("command").and_then(|v| v.as_str()).is_some_and(|command| same_mcp_command(command, exe))
        && entry.get("args").is_none_or(|args| args.as_array().is_some_and(Vec::is_empty))
}

fn toml_entry_owned(entry: &toml_edit::Item, exe: &Path) -> bool {
    entry.get("command").and_then(|v| v.as_str()).is_some_and(|command| same_mcp_command(command, exe))
        && entry.get("args").is_none_or(|args| args.as_array().is_some_and(toml_edit::Array::is_empty))
}

fn mcp_status(client: McpClient, file: Option<&Path>) -> Result<serde_json::Value, CoreError> {
    let exe = mcp_executable()?;
    mcp_status_with_exe(client, file, &exe)
}

fn mcp_status_with_exe(client: McpClient, file: Option<&Path>, exe: &Path) -> Result<serde_json::Value, CoreError> {
    let path = mcp_config_path(client, file)?;
    let text = read_config(&path)?;
    let (state, command) = match text {
        None => ("missing", None),
        Some(text) if matches!(client, McpClient::Codex) => {
            let doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| {
                CoreError::new(ErrorCode::InvalidParams, format!("{} 不是有效 TOML: {e}", path.display()))
            })?;
            validate_toml_servers(&doc, &path)?;
            match doc.get("mcp_servers").and_then(|v| v.get("wb")) {
                Some(entry) if toml_entry_owned(entry, exe) => ("installed", entry.get("command").and_then(|v| v.as_str()).map(str::to_owned)),
                Some(entry) => ("conflict", entry.get("command").and_then(|v| v.as_str()).map(str::to_owned)),
                None => ("missing", None),
            }
        }
        Some(text) => {
            let root: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                CoreError::new(ErrorCode::InvalidParams, format!("{} 不是有效 JSON: {e}", path.display()))
            })?;
            let object = root.as_object().ok_or_else(|| {
                CoreError::new(ErrorCode::InvalidParams, format!("{} 的 JSON 根节点必须是对象", path.display()))
            })?;
            let servers = match object.get("mcpServers") {
                Some(value) => Some(value.as_object().ok_or_else(|| {
                    CoreError::new(ErrorCode::InvalidParams, "mcpServers 必须是对象")
                })?),
                None => None,
            };
            match servers.and_then(|servers| servers.get("wb")) {
                Some(entry) if json_entry_owned(entry, exe) => ("installed", entry.get("command").and_then(|v| v.as_str()).map(str::to_owned)),
                Some(entry) => ("conflict", entry.get("command").and_then(|v| v.as_str()).map(str::to_owned)),
                None => ("missing", None),
            }
        }
    };
    Ok(serde_json::json!({
        "client": client.to_string(), "file": path, "status": state,
        "command": command, "expected_command": exe,
    }))
}

fn mcp_install(client: McpClient, file: Option<&Path>, force: bool) -> Result<serde_json::Value, CoreError> {
    let exe = mcp_executable()?;
    mcp_install_with_exe(client, file, force, &exe)
}

fn mcp_install_with_exe(
    client: McpClient,
    file: Option<&Path>,
    force: bool,
    exe: &Path,
) -> Result<serde_json::Value, CoreError> {
    let path = mcp_config_path(client, file)?;
    let existing = read_config(&path)?;
    let changed = if matches!(client, McpClient::Codex) {
        let mut doc = existing.as_deref().unwrap_or("").parse::<toml_edit::DocumentMut>().map_err(|e| {
            CoreError::new(ErrorCode::InvalidParams, format!("{} 不是有效 TOML: {e}", path.display()))
        })?;
        validate_toml_servers(&doc, &path)?;
        let changed = if let Some(entry) = doc.get("mcp_servers").and_then(|v| v.get("wb")) {
            if toml_entry_owned(entry, exe) {
                false
            } else if !force {
                return Err(mcp_name_conflict(&path));
            } else {
                doc["mcp_servers"]["wb"] = toml_edit::Item::Table(toml_edit::Table::new());
                true
            }
        } else {
            true
        };
        if changed {
            if doc.get("mcp_servers").is_none() {
                doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            doc["mcp_servers"]["wb"]["command"] = toml_edit::value(exe.to_string_lossy().as_ref());
            doc["mcp_servers"]["wb"]["args"] = toml_edit::value(toml_edit::Array::new());
            atomic_write(&path, doc.to_string().as_bytes())?;
        }
        changed
    } else {
        let mut root = match existing {
            Some(text) => serde_json::from_str::<serde_json::Value>(&text).map_err(|e| {
                CoreError::new(ErrorCode::InvalidParams, format!("{} 不是有效 JSON: {e}", path.display()))
            })?,
            None => serde_json::json!({}),
        };
        let object = root.as_object_mut().ok_or_else(|| {
            CoreError::new(ErrorCode::InvalidParams, format!("{} 的 JSON 根节点必须是对象", path.display()))
        })?;
        let servers = object.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
        let servers = servers.as_object_mut().ok_or_else(|| {
            CoreError::new(ErrorCode::InvalidParams, "mcpServers 必须是对象")
        })?;
        let changed = match servers.get("wb") {
            Some(entry) if json_entry_owned(entry, exe) => false,
            Some(_) if !force => return Err(mcp_name_conflict(&path)),
            _ => true,
        };
        if changed {
            servers.insert("wb".into(), serde_json::json!({"command": exe, "args": []}));
            let mut bytes = serde_json::to_vec_pretty(&root)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("生成 MCP JSON 失败: {e}")))?;
            bytes.push(b'\n');
            atomic_write(&path, &bytes)?;
        }
        changed
    };
    Ok(serde_json::json!({
        "client": client.to_string(), "file": path, "status": "installed", "changed": changed,
        "command": exe,
    }))
}

fn mcp_uninstall(client: McpClient, file: Option<&Path>, force: bool) -> Result<serde_json::Value, CoreError> {
    let exe = mcp_executable()?;
    mcp_uninstall_with_exe(client, file, force, &exe)
}

fn mcp_uninstall_with_exe(
    client: McpClient,
    file: Option<&Path>,
    force: bool,
    exe: &Path,
) -> Result<serde_json::Value, CoreError> {
    let path = mcp_config_path(client, file)?;
    let Some(text) = read_config(&path)? else {
        return Ok(serde_json::json!({"client":client.to_string(),"file":path,"status":"missing","changed":false}));
    };
    let changed = if matches!(client, McpClient::Codex) {
        let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| {
            CoreError::new(ErrorCode::InvalidParams, format!("{} 不是有效 TOML: {e}", path.display()))
        })?;
        validate_toml_servers(&doc, &path)?;
        match doc.get("mcp_servers").and_then(|v| v.get("wb")) {
            None => false,
            Some(entry) if toml_entry_owned(entry, exe) || force => {
                doc["mcp_servers"].as_table_like_mut().unwrap().remove("wb");
                atomic_write(&path, doc.to_string().as_bytes())?;
                true
            }
            Some(_) => return Err(mcp_name_conflict(&path)),
        }
    } else {
        let mut root: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            CoreError::new(ErrorCode::InvalidParams, format!("{} 不是有效 JSON: {e}", path.display()))
        })?;
        let object = root.as_object_mut().ok_or_else(|| {
            CoreError::new(ErrorCode::InvalidParams, format!("{} 的 JSON 根节点必须是对象", path.display()))
        })?;
        let Some(servers_value) = object.get_mut("mcpServers") else {
            return Ok(serde_json::json!({"client":client.to_string(),"file":path,"status":"missing","changed":false}));
        };
        let servers = servers_value.as_object_mut().ok_or_else(|| {
            CoreError::new(ErrorCode::InvalidParams, "mcpServers 必须是对象")
        })?;
        match servers.get("wb") {
            None => false,
            Some(entry) if json_entry_owned(entry, exe) || force => {
                servers.remove("wb");
                let mut bytes = serde_json::to_vec_pretty(&root)
                    .map_err(|e| CoreError::new(ErrorCode::Internal, format!("生成 MCP JSON 失败: {e}")))?;
                bytes.push(b'\n');
                atomic_write(&path, &bytes)?;
                true
            }
            Some(_) => return Err(mcp_name_conflict(&path)),
        }
    };
    Ok(serde_json::json!({
        "client": client.to_string(), "file": path, "status": "missing", "changed": changed,
    }))
}

fn validate_toml_servers(doc: &toml_edit::DocumentMut, path: &Path) -> Result<(), CoreError> {
    if doc.get("mcp_servers").is_some_and(|item| !item.is_table_like()) {
        return Err(CoreError::new(
            ErrorCode::InvalidParams,
            format!("{} 中的 mcp_servers 必须是 TOML table", path.display()),
        ));
    }
    Ok(())
}

fn mcp_name_conflict(path: &Path) -> CoreError {
    CoreError::new(ErrorCode::PermissionDenied, format!("{} 中已存在其他名为 wb 的 MCP server", path.display()))
        .with_hint("检查现有条目，确认后使用 --force 替换或删除")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建 {} 失败: {e}", parent.display())))?;
    let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("mcp-config");
    let temp = parent.join(format!(".{name}.wb-{}-{}.tmp", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建临时配置失败: {e}")))?;
        file.write_all(bytes).and_then(|_| file.sync_all())
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("写入临时配置失败: {e}")))?;
        replace_file(&temp, path)
    })();
    if result.is_err() { std::fs::remove_file(&temp).ok(); }
    result
}

#[cfg(windows)]
fn replace_file(temp: &Path, path: &Path) -> Result<(), CoreError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
    let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) };
    if ok == 0 {
        Err(CoreError::new(ErrorCode::Internal, format!("原子替换 {} 失败: {}", path.display(), std::io::Error::last_os_error())))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, path: &Path) -> Result<(), CoreError> {
    std::fs::rename(temp, path)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("原子替换 {} 失败: {e}", path.display())))
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
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            },
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
@container (max-width:180px){{.time{{font-size:24px}}.name{{font-size:11px}}}}
@container (max-height:90px){{.root>div{{display:flex;align-items:center;gap:9px}}.time{{font-size:22px}}.name{{margin-top:0}}}}
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

const BACKUP_MAX_FILES: usize = 1024;
const BACKUP_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const BACKUP_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

struct BackupStats {
    files: usize,
    bytes: u64,
}

impl BackupStats {
    fn add(&mut self, size: u64, name: &str) -> Result<(), CoreError> {
        if size > BACKUP_MAX_FILE_BYTES {
            return Err(CoreError::new(
                ErrorCode::InvalidParams,
                format!("备份文件超过 16 MiB 限制: {name}"),
            ));
        }
        if self.files >= BACKUP_MAX_FILES {
            return Err(CoreError::new(ErrorCode::InvalidParams, "备份文件数量超过 1024 项"));
        }
        let total = self
            .bytes
            .checked_add(size)
            .ok_or_else(|| CoreError::new(ErrorCode::InvalidParams, "备份总大小溢出"))?;
        if total > BACKUP_MAX_TOTAL_BYTES {
            return Err(CoreError::new(ErrorCode::InvalidParams, "备份总大小超过 256 MiB 限制"));
        }
        self.files += 1;
        self.bytes = total;
        Ok(())
    }
}

fn backup_options() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
}

fn backup_zip_error(error: zip::result::ZipError, action: &str) -> CoreError {
    CoreError::new(ErrorCode::Internal, format!("{action}: {error}"))
}

fn add_archive_bytes<W: Write + std::io::Seek>(
    writer: &mut zip::ZipWriter<W>,
    archive_name: &str,
    bytes: &[u8],
    stats: &mut BackupStats,
) -> Result<(), CoreError> {
    stats.add(bytes.len() as u64, archive_name)?;
    writer
        .start_file(archive_name.replace('\\', "/"), backup_options())
        .map_err(|e| backup_zip_error(e, "创建归档条目失败"))?;
    writer
        .write_all(bytes)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("写入归档条目失败: {e}")))?;
    Ok(())
}

fn add_backup_file<W: Write + std::io::Seek>(
    writer: &mut zip::ZipWriter<W>,
    archive_name: &str,
    source: &Path,
    stats: &mut BackupStats,
) -> Result<(), CoreError> {
    let metadata = std::fs::symlink_metadata(source).map_err(|e| {
        CoreError::new(
            ErrorCode::Internal,
            format!("读取备份文件失败 {}: {e}", source.display()),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CoreError::new(
            ErrorCode::InvalidParams,
            format!("备份不支持符号链接或特殊文件: {}", source.display()),
        ));
    }
    let bytes = std::fs::read(source).map_err(|e| {
        CoreError::new(
            ErrorCode::Internal,
            format!("读取备份文件失败 {}: {e}", source.display()),
        )
    })?;
    add_archive_bytes(writer, archive_name, &bytes, stats)
}

fn add_backup_tree<W: Write + std::io::Seek>(
    writer: &mut zip::ZipWriter<W>,
    root: &Path,
    current: &Path,
    prefix: &str,
    depth: usize,
    stats: &mut BackupStats,
) -> Result<(), CoreError> {
    if depth > 16 {
        return Err(CoreError::new(ErrorCode::InvalidParams, "插件目录层级超过 16 层"));
    }
    let mut entries = std::fs::read_dir(current)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("读取插件目录失败 {}: {e}", current.display())))?
        .flatten()
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| {
            CoreError::new(ErrorCode::Internal, format!("读取插件文件失败 {}: {e}", path.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CoreError::new(
                ErrorCode::InvalidParams,
                format!("备份不支持插件符号链接: {}", path.display()),
            ));
        }
        let relative = path.strip_prefix(root).map_err(|_| {
            CoreError::new(ErrorCode::Internal, format!("插件路径越界: {}", path.display()))
        })?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        let archive_name = format!("{prefix}{relative}");
        if metadata.is_dir() {
            add_backup_tree(writer, root, &path, prefix, depth + 1, stats)?;
        } else if metadata.is_file() {
            add_backup_file(writer, &archive_name, &path, stats)?;
        } else {
            return Err(CoreError::new(
                ErrorCode::InvalidParams,
                format!("备份不支持特殊插件文件: {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn backup_database(source: &Path, destination: &Path) -> Result<(), CoreError> {
    if !source.is_file() {
        return Err(CoreError::new(
            ErrorCode::NotFound,
            format!("数据库不存在: {}", source.display()),
        ));
    }
    if destination.exists() {
        std::fs::remove_file(destination).map_err(|e| {
            CoreError::new(ErrorCode::Internal, format!("清理临时数据库失败: {e}"))
        })?;
    }
    let source_conn = rusqlite::Connection::open(source)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("打开数据库失败: {e}")))?;
    source_conn
        .backup(rusqlite::DatabaseName::Main, destination, None)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("在线备份数据库失败: {e}")))
}

fn archive_output_path(output: Option<&Path>, prefix: &str) -> Result<PathBuf, CoreError> {
    let path = if let Some(output) = output {
        if output.as_os_str().is_empty() {
            return Err(CoreError::new(ErrorCode::InvalidParams, "备份输出路径不能为空"));
        }
        if output.is_absolute() {
            output.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("读取当前目录失败: {e}")))?
                .join(output)
        }
    } else {
        let base = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .map(|p| p.join("Downloads"))
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| CoreError::new(ErrorCode::Internal, "无法确定备份输出目录"))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("读取系统时间失败: {e}")))?
            .as_secs();
        base.join(format!("{prefix}-{stamp}.zip"))
    };
    let path = if path.extension().is_none() {
        path.with_extension("zip")
    } else {
        path
    };
    if path.exists() {
        return Err(CoreError::new(
            ErrorCode::InvalidParams,
            format!("备份文件已存在，拒绝覆盖: {}", path.display()),
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建备份目录失败: {e}")))?;
    }
    Ok(path)
}

fn backup_output_path(output: Option<&Path>) -> Result<PathBuf, CoreError> {
    archive_output_path(output, "RuDock-backup")
}

fn diagnostics_output_path(output: Option<&Path>) -> Result<PathBuf, CoreError> {
    archive_output_path(output, "RuDock-diagnostics")
}

fn create_backup(output: Option<&Path>) -> Result<serde_json::Value, CoreError> {
    let output = backup_output_path(output)?;
    let temp_db = std::env::temp_dir().join(format!(
        "rudock-backup-db-{}-{}.db",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("读取系统时间失败: {e}")))?
            .as_nanos()
    ));
    let result = (|| {
        backup_database(&wb_core::paths::db_path(), &temp_db)?;
        let file = std::fs::File::create(&output)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建备份归档失败: {e}")))?;
        let mut writer = zip::ZipWriter::new(file);
        let mut stats = BackupStats { files: 0, bytes: 0 };
        let settings = wb_core::paths::settings_path();
        let settings_present = settings.is_file();
        if settings_present {
            add_backup_file(&mut writer, "settings.json", &settings, &mut stats)?;
        }
        add_backup_file(&mut writer, "database/wb.db", &temp_db, &mut stats)?;
        let plugin_root = wb_core::paths::local_data_dir().join("plugins");
        if plugin_root.is_dir() {
            let output_parent = output.parent().unwrap_or_else(|| Path::new("."));
            let output_parent = output_parent.canonicalize().map_err(|e| {
                CoreError::new(ErrorCode::Internal, format!("解析备份目录失败: {e}"))
            })?;
            let plugin_root_canonical = plugin_root.canonicalize().map_err(|e| {
                CoreError::new(ErrorCode::Internal, format!("解析插件目录失败: {e}"))
            })?;
            if output_parent.starts_with(&plugin_root_canonical) {
                return Err(CoreError::new(
                    ErrorCode::InvalidParams,
                    "备份输出不能放在用户插件目录内",
                ));
            }
            add_backup_tree(&mut writer, &plugin_root, &plugin_root, "plugins/", 0, &mut stats)?;
        }
        let manifest = serde_json::json!({
            "schema_version": 1,
            "kind": "rudock-backup",
            "app_version": env!("CARGO_PKG_VERSION"),
            "created_unix": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            "database": "database/wb.db",
            "settings": settings_present.then_some("settings.json"),
            "plugins": "plugins/",
            "file_count": stats.files,
            "payload_bytes": stats.bytes,
        });
        writer
            .start_file("manifest.json", backup_options())
            .map_err(|e| backup_zip_error(e, "创建备份清单失败"))?;
        writer
            .write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("写入备份清单失败: {e}")))?;
        writer
            .start_file("README.txt", backup_options())
            .map_err(|e| backup_zip_error(e, "创建备份说明失败"))?;
        writer
            .write_all(b"RuDock local backup. Stop RuDock before restoring these files. This archive contains personal data and should be stored privately.\r\n")
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("写入备份说明失败: {e}")))?;
        writer
            .finish()
            .map_err(|e| backup_zip_error(e, "完成备份归档失败"))?;
        let sha256 = archive_sha256(&output)?;
        Ok(serde_json::json!({
            "created": true,
            "output": output,
            "sha256": format!("sha256:{sha256}"),
            "file_count": stats.files,
            "payload_bytes": stats.bytes,
        }))
    })();
    let _ = std::fs::remove_file(&temp_db);
    if result.is_err() {
        let _ = std::fs::remove_file(&output);
    }
    result
}

fn safe_backup_entry(raw: &str) -> Result<String, CoreError> {
    if raw.contains('\0') {
        return Err(CoreError::new(ErrorCode::InvalidParams, "备份条目包含 NUL 字符"));
    }
    let normalized = raw.replace('\\', "/").trim_end_matches('/').to_string();
    if normalized.is_empty() {
        return Err(CoreError::new(ErrorCode::InvalidParams, "备份包含空路径条目"));
    }
    for component in std::path::Path::new(&normalized).components() {
        if matches!(
            component,
            std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::ParentDir
                | std::path::Component::CurDir
        ) {
            return Err(CoreError::new(
                ErrorCode::InvalidParams,
                format!("备份条目路径不安全: {raw}"),
            ));
        }
    }
    Ok(normalized)
}

fn allowed_backup_entry(name: &str) -> bool {
    matches!(name, "manifest.json" | "README.txt" | "settings.json" | "database/wb.db")
        || name.starts_with("plugins/")
}

fn validate_restore_database(path: &Path) -> Result<(), CoreError> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("备份数据库无法打开: {e}")))?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("备份数据库完整性检查失败: {e}")))?;
    if integrity != "ok" {
        return Err(CoreError::new(
            ErrorCode::InvalidParams,
            format!("备份数据库完整性检查未通过: {integrity}"),
        ));
    }
    for table in ["notes", "todos", "clips", "audit"] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("检查备份表失败: {e}")))?;
        if !exists {
            return Err(CoreError::new(
                ErrorCode::InvalidParams,
                format!("备份数据库缺少表: {table}"),
            ));
        }
    }
    Ok(())
}

fn validate_restore_plugins(root: &Path) -> Result<usize, CoreError> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(root)
        .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("读取备份插件目录失败: {e}")))?
        .flatten()
    {
        let dir = entry.path();
        if !dir.is_dir() {
            return Err(CoreError::new(
                ErrorCode::InvalidParams,
                format!("备份插件目录包含非目录项: {}", dir.display()),
            ));
        }
        let text = std::fs::read_to_string(dir.join("plugin.json")).map_err(|e| {
            CoreError::new(ErrorCode::InvalidParams, format!("读取备份插件 manifest 失败: {e}"))
        })?;
        let manifest: wb_plugin_sdk::Manifest = serde_json::from_str(&text).map_err(|e| {
            CoreError::new(ErrorCode::InvalidParams, format!("备份插件 manifest 无效: {e}"))
        })?;
        manifest.validate().map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("备份插件 manifest 校验失败: {e}")))?;
        let plugin = wb_plugin_host::LoadedPlugin { dir, manifest };
        wb_plugin_host::validate_files(&plugin)
            .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("备份插件文件校验失败: {e}")))?;
        count += 1;
    }
    Ok(count)
}

fn extract_backup(archive_path: &Path) -> Result<(PathBuf, serde_json::Value, usize), CoreError> {
    if !archive_path.is_file() {
        return Err(CoreError::new(
            ErrorCode::NotFound,
            format!("备份归档不存在: {}", archive_path.display()),
        ));
    }
    let staging = std::env::temp_dir().join(format!(
        "rudock-restore-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("读取系统时间失败: {e}")))?
            .as_nanos()
    ));
    std::fs::create_dir_all(&staging)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建恢复暂存目录失败: {e}")))?;
    let result = (|| {
        let file = std::fs::File::open(archive_path)
            .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("打开备份归档失败: {e}")))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("备份 ZIP 无效: {e}")))?;
        let mut seen = std::collections::HashSet::new();
        let mut stats = BackupStats { files: 0, bytes: 0 };
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("读取备份条目失败: {e}")))?;
            let name = safe_backup_entry(entry.name())?;
            if !allowed_backup_entry(&name) {
                return Err(CoreError::new(
                    ErrorCode::InvalidParams,
                    format!("备份包含不支持的条目: {name}"),
                ));
            }
            if !seen.insert(name.clone()) {
                return Err(CoreError::new(
                    ErrorCode::InvalidParams,
                    format!("备份包含重复条目: {name}"),
                ));
            }
            if entry.is_dir() {
                continue;
            }
            stats.add(entry.size(), &name)?;
            let target = staging.join(&name);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CoreError::new(ErrorCode::Internal, format!("创建恢复目录失败: {e}"))
                })?;
            }
            let canonical_parent = target.parent().and_then(|p| p.canonicalize().ok()).ok_or_else(|| {
                CoreError::new(ErrorCode::Internal, format!("解析恢复目录失败: {}", target.display()))
            })?;
            let staging_canonical = staging.canonicalize().map_err(|e| {
                CoreError::new(ErrorCode::Internal, format!("解析恢复暂存目录失败: {e}"))
            })?;
            if !canonical_parent.starts_with(&staging_canonical) {
                return Err(CoreError::new(ErrorCode::InvalidParams, "备份条目越出恢复目录"));
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建恢复文件失败: {e}")))?;
            let copied = std::io::copy(&mut entry, &mut output)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("解压恢复文件失败: {e}")))?;
            if copied != entry.size() {
                return Err(CoreError::new(ErrorCode::InvalidParams, format!("恢复文件大小不一致: {name}")));
            }
        }
        let manifest_path = staging.join("manifest.json");
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("备份缺少 manifest.json: {e}")))?;
        let manifest: serde_json::Value = serde_json::from_str(&manifest_text)
            .map_err(|e| CoreError::new(ErrorCode::InvalidParams, format!("备份 manifest 无效: {e}")))?;
        if manifest.get("kind").and_then(|v| v.as_str()) != Some("rudock-backup")
            || manifest.get("schema_version").and_then(|v| v.as_u64()) != Some(1)
        {
            return Err(CoreError::new(ErrorCode::InvalidParams, "不是兼容的 RuDock 备份格式"));
        }
        let database = staging.join("database/wb.db");
        validate_restore_database(&database)?;
        let staged_settings = staging.join("settings.json");
        if staged_settings.is_file() {
            let text = std::fs::read_to_string(&staged_settings).map_err(|e| {
                CoreError::new(ErrorCode::InvalidParams, format!("读取备份设置失败: {e}"))
            })?;
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                CoreError::new(ErrorCode::InvalidParams, format!("备份设置不是 JSON: {e}"))
            })?;
            if !value.is_object() {
                return Err(CoreError::new(ErrorCode::InvalidParams, "备份设置必须是 JSON 对象"));
            }
        }
        let plugin_count = validate_restore_plugins(&staging.join("plugins"))?;
        Ok((staging.clone(), manifest, plugin_count))
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

fn move_restore_source(source: &Path, target: &Path, moved: &mut Vec<(PathBuf, PathBuf)>) -> Result<(), CoreError> {
    if std::fs::symlink_metadata(source).is_err() {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建回滚目录失败: {e}")))?;
    }
    std::fs::rename(source, target).map_err(|e| {
        CoreError::new(
            ErrorCode::Internal,
            format!("移动现有数据到回滚目录失败 {}: {e}", source.display()),
        )
    })?;
    moved.push((source.to_path_buf(), target.to_path_buf()));
    Ok(())
}

fn restore_moved_sources(moved: &[(PathBuf, PathBuf)]) {
    for (source, backup) in moved.iter().rev() {
        if std::fs::symlink_metadata(source).is_ok() {
            let _ = std::fs::remove_dir_all(source).or_else(|_| std::fs::remove_file(source));
        }
        if std::fs::symlink_metadata(backup).is_ok() {
            let _ = std::fs::rename(backup, source);
        }
    }
}

fn restore_backup(archive: &Path) -> Result<serde_json::Value, CoreError> {
    if Client::connect_existing().is_ok() {
        return Err(CoreError::new(
            ErrorCode::InvalidParams,
            "RuDock daemon 正在运行，请先执行 `wb daemon stop` 再恢复",
        ));
    }
    let (staging, manifest, plugin_count) = extract_backup(archive)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("读取系统时间失败: {e}")))?
        .as_secs();
    let rollback = wb_core::paths::local_data_dir()
        .join("restore-backups")
        .join(format!("{stamp}-{}", std::process::id()));
    let app_rollback = rollback.join("app");
    let local_rollback = rollback.join("local");
    let db = wb_core::paths::db_path();
    let settings = wb_core::paths::settings_path();
    let plugins = wb_core::paths::local_data_dir().join("plugins");
    let staged_db = staging.join("database/wb.db");
    let staged_settings = staging.join("settings.json");
    let staged_plugins = staging.join("plugins");
    let mut moved = Vec::new();
    let mut committed = Vec::new();
    std::fs::create_dir_all(&rollback)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建恢复回滚目录失败: {e}")))?;
    let result = (|| {
        move_restore_source(&db, &app_rollback.join("wb.db"), &mut moved)?;
        move_restore_source(&db.with_extension("db-wal"), &app_rollback.join("wb.db-wal"), &mut moved)?;
        move_restore_source(&db.with_extension("db-shm"), &app_rollback.join("wb.db-shm"), &mut moved)?;
        if staged_settings.is_file() {
            move_restore_source(&settings, &app_rollback.join("settings.json"), &mut moved)?;
        }
        if staged_plugins.is_dir() {
            move_restore_source(&plugins, &local_rollback.join("plugins"), &mut moved)?;
        }
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建数据库目录失败: {e}")))?;
        }
        std::fs::rename(&staged_db, &db)
            .map_err(|e| CoreError::new(ErrorCode::Internal, format!("提交恢复数据库失败: {e}")))?;
        committed.push(db.clone());
        if staged_settings.is_file() {
            if let Some(parent) = settings.parent() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建设置目录失败: {e}")))?;
            }
            std::fs::rename(&staged_settings, &settings)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("提交恢复设置失败: {e}")))?;
            committed.push(settings.clone());
        }
        if staged_plugins.is_dir() {
            if let Some(parent) = plugins.parent() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建插件目录失败: {e}")))?;
            }
            std::fs::rename(&staged_plugins, &plugins)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("提交恢复插件失败: {e}")))?;
            committed.push(plugins.clone());
        }
        Ok(serde_json::json!({
            "restored": true,
            "restart_required": true,
            "rollback": rollback,
            "plugin_count": plugin_count,
            "manifest": manifest,
        }))
    })();
    let result = match result {
        Ok(value) => Ok(value),
        Err(error) => {
            for path in committed.iter().rev() {
                if std::fs::symlink_metadata(path).is_ok() {
                    let _ = std::fs::remove_dir_all(path).or_else(|_| std::fs::remove_file(path));
                }
            }
            restore_moved_sources(&moved);
            let _ = std::fs::remove_dir_all(&rollback);
            Err(error)
        }
    };
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn sensitive_json_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["key", "token", "secret", "password", "authorization"]
        .iter()
        .any(|needle| key == *needle || key.ends_with(&format!("_{needle}")))
}

fn private_json_key(key: &str) -> bool {
    matches!(key.to_ascii_lowercase().as_str(), "path" | "dir" | "directory" | "cwd" | "home")
}

fn redact_json(value: &serde_json::Value, key: Option<&str>) -> serde_json::Value {
    if let Some(key) = key {
        if sensitive_json_key(key) {
            return serde_json::Value::String("<redacted>".into());
        }
        if private_json_key(key) {
            return serde_json::Value::String("<redacted>".into());
        }
    }
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(name, value)| (name.clone(), redact_json(value, Some(name))))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(|item| redact_json(item, None)).collect())
        }
        _ => value.clone(),
    }
}

fn diagnostic_call(client: &mut Option<Client>, method: &str) -> serde_json::Value {
    client
        .as_mut()
        .and_then(|client| client.call(method, serde_json::json!({})).ok())
        .unwrap_or_else(|| serde_json::json!({"status": "unavailable"}))
}

fn create_diagnostics(output: Option<&Path>) -> Result<serde_json::Value, CoreError> {
    let output = diagnostics_output_path(output)?;
    let mut client = Client::connect_existing().ok();
    let settings = redact_json(&diagnostic_call(&mut client, "settings.get"), None);
    let plugins = redact_json(&diagnostic_call(&mut client, "plugin.list"), None);
    let audit = redact_json(&diagnostic_call(&mut client, "audit.tail"), None);
    let daemon = diagnostic_call(&mut client, "daemon.ping");
    let plugin_count = plugins.as_array().map_or(0, Vec::len);
    let audit_count = audit.as_array().map_or(0, Vec::len);
    let summary = serde_json::json!({
        "schema_version": 1,
        "kind": "rudock-diagnostics",
        "app_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "daemon_connected": client.is_some(),
        "plugin_count": plugin_count,
        "audit_count": audit_count,
        "contains_personal_content": false,
        "omitted": ["database", "clipboard contents", "AI configuration", "user file index"],
    });
    let schema = wb_core::protocol::schema();
    let files = [
        ("diagnostic.json", serde_json::to_vec_pretty(&summary)?),
        ("daemon.json", serde_json::to_vec_pretty(&redact_json(&daemon, None))?),
        ("settings.redacted.json", serde_json::to_vec_pretty(&settings)?),
        ("plugins.redacted.json", serde_json::to_vec_pretty(&plugins)?),
        ("audit.redacted.json", serde_json::to_vec_pretty(&audit)?),
        ("schema.json", serde_json::to_vec_pretty(&schema)?),
    ];
    let file = std::fs::File::create(&output)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("创建诊断归档失败: {e}")))?;
    let mut writer = zip::ZipWriter::new(file);
    let mut stats = BackupStats { files: 0, bytes: 0 };
    let result = (|| {
        for (name, bytes) in &files {
            add_archive_bytes(&mut writer, name, bytes, &mut stats)?;
        }
        add_archive_bytes(
            &mut writer,
            "README.txt",
            b"RuDock redacted diagnostics. No database, clipboard content, AI configuration, or user file index is included.\r\n",
            &mut stats,
        )?;
        writer
            .finish()
            .map_err(|e| backup_zip_error(e, "完成诊断归档失败"))?;
        let sha256 = archive_sha256(&output)?;
        Ok(serde_json::json!({
            "created": true,
            "output": output,
            "sha256": format!("sha256:{sha256}"),
            "file_count": stats.files,
            "payload_bytes": stats.bytes,
        }))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&output);
    }
    result
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
    if let Cmd::Mcp { op } = &cli.cmd {
        let result = match op {
            McpOp::Config { client } => match mcp_config(*client) {
                Ok(config) if json => Ok(serde_json::json!({"client": client.to_string(), "config": config})),
                Ok(config) => {
                    print!("{config}");
                    return;
                }
                Err(e) => Err(e),
            },
            McpOp::Install { client, file, force } => mcp_install(*client, file.as_deref(), *force),
            McpOp::Status { client, file } => mcp_status(*client, file.as_deref()),
            McpOp::Uninstall { client, file, force } => mcp_uninstall(*client, file.as_deref(), *force),
        };
        match result {
            Ok(value) => emit(&value, json, false),
            Err(e) => fail(&e, json),
        }
        return;
    }

    let local_backup_result = match &cli.cmd {
        Cmd::Backup { op: BackupOp::Create { output } } => Some(create_backup(output.as_deref())),
        Cmd::Backup { op: BackupOp::Restore { archive } } => Some(restore_backup(archive)),
        _ => None,
    };
    if let Some(result) = local_backup_result {
        match result {
            Ok(v) => emit(&v, json, false),
            Err(e) => fail(&e, json),
        }
        return;
    }

    let local_diagnostics_result = match &cli.cmd {
        Cmd::Diagnostics { op: DiagnosticsOp::Create { output } } => {
            Some(create_diagnostics(output.as_deref()))
        }
        _ => None,
    };
    if let Some(result) = local_diagnostics_result {
        match result {
            Ok(v) => emit(&v, json, false),
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
            PluginOp::Market { op } => match op {
                MarketOp::Source { op } => match op {
                    MarketSourceOp::List => (
                        "plugin.market.sources",
                        serde_json::json!({}),
                    ),
                    MarketSourceOp::Add { index } => (
                        "plugin.market.source.add",
                        serde_json::json!({"index": index}),
                    ),
                    MarketSourceOp::Remove { index } => (
                        "plugin.market.source.remove",
                        serde_json::json!({"index": index}),
                    ),
                },
                MarketOp::List { index } => (
                    "plugin.market.list",
                    serde_json::json!({"index": index}),
                ),
                MarketOp::Check { index } => (
                    "plugin.market.check",
                    serde_json::json!({"index": index}),
                ),
                MarketOp::Install { id, index } => (
                    "plugin.market.install",
                    serde_json::json!({"id": id, "index": index}),
                ),
                MarketOp::Update { id, index } => (
                    "plugin.market.update",
                    serde_json::json!({"id": id, "index": index}),
                ),
            },
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
            SettingsOp::Language { language } => ("settings.set", serde_json::json!({"language":language.as_str()})),
            SettingsOp::Desktop { widgets } => ("settings.set", serde_json::json!({"desktop_widgets":widgets})),
            SettingsOp::Mcp { policy } => (
                "settings.set",
                serde_json::json!({"mcp_write_policy":policy.as_str()}),
            ),
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
        Cmd::Events {
            after,
            limit,
            wait_ms,
        } => (
            "events.tail",
            serde_json::json!({"after":after,"limit":limit,"wait_ms":wait_ms}),
        ),
        Cmd::Apps => ("apps.list", serde_json::json!({})),
        Cmd::Update { op: UpdateOp::Check } => ("app.update.check", serde_json::json!({})),
        Cmd::Daemon { .. } => unreachable!(),
        Cmd::Schema => unreachable!(),
        Cmd::Mcp { .. } => unreachable!(),
        Cmd::Backup { .. } => unreachable!(),
        Cmd::Diagnostics { .. } => unreachable!(),
    };

    match client.call(method, params) {
        Ok(v) => emit(&v, json, ndjson),
        Err(e) => fail(&e, json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "wb-cli-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn fake_mcp(root: &Path) -> PathBuf {
        let exe = root.join("wb-mcp.exe");
        std::fs::write(&exe, b"test").unwrap();
        exe
    }

    #[test]
    fn parses_backup_create_command() {
        let cli = Cli::try_parse_from([
            "wb",
            "backup",
            "create",
            "--output",
            "C:\\Backups\\rudock.zip",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Backup {
                op: BackupOp::Create { output: Some(_) }
            }
        ));
    }

    #[test]
    fn parses_backup_restore_command() {
        let cli = Cli::try_parse_from(["wb", "backup", "restore", "backup.zip"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Backup {
                op: BackupOp::Restore { .. }
            }
        ));
    }

    #[test]
    fn safe_backup_entries_reject_traversal_and_accept_nested_files() {
        assert_eq!(safe_backup_entry("plugins/demo/widget.html").unwrap(), "plugins/demo/widget.html");
        for entry in ["../escape", "database/../../escape", "C:/escape", "plugins/\0bad"] {
            assert!(safe_backup_entry(entry).is_err(), "accepted unsafe entry {entry:?}");
        }
    }

    #[test]
    fn extracts_and_validates_minimal_backup_archive() {
        let root = test_root("extract-backup");
        let source = root.join("source.db");
        let archive_path = root.join("backup.zip");
        {
            let conn = rusqlite::Connection::open(&source).unwrap();
            conn.execute_batch(
                "CREATE TABLE notes (id TEXT PRIMARY KEY, content TEXT NOT NULL, tags TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE todos (id TEXT PRIMARY KEY, title TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0, due TEXT, repeat TEXT, tags TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL);
                 CREATE TABLE clips (id TEXT PRIMARY KEY, kind TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL);
                 CREATE TABLE audit (id INTEGER PRIMARY KEY, actor TEXT NOT NULL, action TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL, event_version INTEGER NOT NULL DEFAULT 0);",
            )
            .unwrap();
        }
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
        writer
            .start_file("manifest.json", backup_options())
            .unwrap();
        writer
            .write_all(br#"{"kind":"rudock-backup","schema_version":1}"#)
            .unwrap();
        writer.start_file("database/wb.db", backup_options()).unwrap();
        writer.write_all(&std::fs::read(&source).unwrap()).unwrap();
        writer.finish().unwrap();

        let (staging, manifest, plugins) = extract_backup(&archive_path).unwrap();
        assert_eq!(manifest["kind"], "rudock-backup");
        assert_eq!(plugins, 0);
        assert!(staging.join("database/wb.db").is_file());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn online_database_backup_preserves_rows() {
        let root = test_root("backup-db");
        let source = root.join("source.db");
        let destination = root.join("destination.db");
        {
            let conn = rusqlite::Connection::open(&source).unwrap();
            conn.execute("CREATE TABLE sample (value TEXT NOT NULL)", []).unwrap();
            conn.execute("INSERT INTO sample (value) VALUES ('kept')", []).unwrap();
        }
        backup_database(&source, &destination).unwrap();
        let conn = rusqlite::Connection::open(&destination).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sample", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "kept");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn diagnostics_redaction_removes_secrets_and_paths() {
        let input = serde_json::json!({
            "api_key": "secret",
            "nested": {"access_token": "token", "path": "C:\\Users\\private"},
            "safe": "kept"
        });
        let redacted = redact_json(&input, None);
        assert_eq!(redacted["api_key"], "<redacted>");
        assert_eq!(redacted["nested"]["access_token"], "<redacted>");
        assert_eq!(redacted["nested"]["path"], "<redacted>");
        assert_eq!(redacted["safe"], "kept");
    }

    #[test]
    fn parses_diagnostics_create_command() {
        let cli = Cli::try_parse_from(["wb", "diagnostics", "create"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Diagnostics {
                op: DiagnosticsOp::Create { output: None }
            }
        ));
    }

    #[test]
    fn parses_mcp_config_management_commands() {
        let cli = Cli::try_parse_from(["wb", "mcp", "install", "codex", "--file", "config.toml", "--force"]).unwrap();
        match cli.cmd {
            Cmd::Mcp { op: McpOp::Install { client: McpClient::Codex, file, force } } => {
                assert_eq!(file.as_deref(), Some(Path::new("config.toml")));
                assert!(force);
            }
            _ => panic!("unexpected command"),
        }

        let cli = Cli::try_parse_from(["wb", "mcp", "status", "cursor", "--file", "mcp.json"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Mcp { op: McpOp::Status { client: McpClient::Cursor, .. } }));

        let cli = Cli::try_parse_from(["wb", "mcp", "uninstall", "claude", "--force"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Mcp { op: McpOp::Uninstall { client: McpClient::Claude, force: true, .. } }));
    }

    #[test]
    fn mcp_json_install_is_structured_idempotent_and_protected() {
        let root = test_root("mcp-json");
        let exe = fake_mcp(&root);
        let config = root.join("mcp.json");
        let original = r#"{
  "theme": "dark",
  "mcpServers": {
    "other": { "command": "other.exe" },
    "wb": { "command": "someone-else.exe", "args": [] }
  }
}
"#;
        std::fs::write(&config, original).unwrap();

        let error = mcp_install_with_exe(McpClient::Cursor, Some(&config), false, &exe).unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);

        let installed = mcp_install_with_exe(McpClient::Cursor, Some(&config), true, &exe).unwrap();
        assert_eq!(installed["changed"], true);
        let root_value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(root_value["theme"], "dark");
        assert_eq!(root_value["mcpServers"]["other"]["command"], "other.exe");
        assert_eq!(root_value["mcpServers"]["wb"]["command"], exe.to_string_lossy().as_ref());

        let before = std::fs::read(&config).unwrap();
        let repeated = mcp_install_with_exe(McpClient::Cursor, Some(&config), false, &exe).unwrap();
        assert_eq!(repeated["changed"], false);
        assert_eq!(std::fs::read(&config).unwrap(), before);
        assert_eq!(mcp_status_with_exe(McpClient::Cursor, Some(&config), &exe).unwrap()["status"], "installed");

        let removed = mcp_uninstall_with_exe(McpClient::Cursor, Some(&config), false, &exe).unwrap();
        assert_eq!(removed["changed"], true);
        let root_value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert!(root_value["mcpServers"].get("wb").is_none());
        assert_eq!(root_value["mcpServers"]["other"]["command"], "other.exe");
        assert_eq!(root_value["theme"], "dark");
        assert!(!std::fs::read_dir(&root)
            .unwrap()
            .any(|entry| entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));

        let malformed = root.join("malformed.json");
        std::fs::write(&malformed, "{\"mcpServers\":[]}").unwrap();
        let error = mcp_install_with_exe(McpClient::Generic, Some(&malformed), true, &exe).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidParams);
        assert_eq!(std::fs::read_to_string(malformed).unwrap(), "{\"mcpServers\":[]}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mcp_codex_install_preserves_other_toml_tables() {
        let root = test_root("mcp-toml");
        let exe = fake_mcp(&root);
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            "# keep this comment\nmodel = \"gpt-test\"\n\n[mcp_servers.other]\ncommand = \"other.exe\"\n",
        )
        .unwrap();

        let installed = mcp_install_with_exe(McpClient::Codex, Some(&config), false, &exe).unwrap();
        assert_eq!(installed["changed"], true);
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("# keep this comment"));
        let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some("gpt-test"));
        assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("other.exe"));
        assert_eq!(doc["mcp_servers"]["wb"]["command"].as_str(), Some(exe.to_string_lossy().as_ref()));
        assert_eq!(mcp_status_with_exe(McpClient::Codex, Some(&config), &exe).unwrap()["status"], "installed");

        mcp_uninstall_with_exe(McpClient::Codex, Some(&config), false, &exe).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert!(doc["mcp_servers"].get("wb").is_none());
        assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("other.exe"));
        assert!(text.contains("# keep this comment"));

        let conflict = text + "\n[mcp_servers.wb]\ncommand = \"someone-else.exe\"\nargs = []\n";
        std::fs::write(&config, &conflict).unwrap();
        let error = mcp_install_with_exe(McpClient::Codex, Some(&config), false, &exe).unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), conflict);
        mcp_install_with_exe(McpClient::Codex, Some(&config), true, &exe).unwrap();
        let doc = std::fs::read_to_string(&config).unwrap().parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(doc["mcp_servers"]["wb"]["command"].as_str(), Some(exe.to_string_lossy().as_ref()));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mcp_generic_requires_an_explicit_file() {
        let error = mcp_config_path(McpClient::Generic, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidParams);
    }

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

    #[test]
    fn parses_market_update() {
        let cli = Cli::try_parse_from([
            "wb",
            "plugin",
            "market",
            "update",
            "hello",
            "--index",
            "https://plugins.example/index.json",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Plugin {
                op:
                    PluginOp::Market {
                        op: MarketOp::Update { id, index },
                    },
            } => {
                assert_eq!(id, "hello");
                assert_eq!(index.as_deref(), Some("https://plugins.example/index.json"));
            }
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn parses_persistent_market_source() {
        let cli = Cli::try_parse_from([
            "wb",
            "plugin",
            "market",
            "source",
            "add",
            "https://plugins.example/index.json",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Plugin {
                op:
                    PluginOp::Market {
                        op:
                            MarketOp::Source {
                                op: MarketSourceOp::Add { index },
                            },
                    },
            } => assert_eq!(index, "https://plugins.example/index.json"),
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn parses_mcp_write_policy() {
        let cli = Cli::try_parse_from(["wb", "settings", "mcp", "ask"]).unwrap();
        match cli.cmd {
            Cmd::Settings {
                op: SettingsOp::Mcp { policy },
            } => assert_eq!(policy.as_str(), "ask"),
            _ => panic!("unexpected command"),
        }

        let cli = Cli::try_parse_from(["wb", "settings", "mcp", "read-only"]).unwrap();
        match cli.cmd {
            Cmd::Settings {
                op: SettingsOp::Mcp { policy },
            } => assert_eq!(policy.as_str(), "read-only"),
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn parses_supported_interface_languages() {
        for (input, expected) in [("auto", "auto"), ("zh-CN", "zh-CN"), ("en", "en"), ("ja", "ja"), ("ko", "ko")] {
            let cli = Cli::try_parse_from(["wb", "settings", "language", input]).unwrap();
            match cli.cmd {
                Cmd::Settings { op: SettingsOp::Language { language } } => assert_eq!(language.as_str(), expected),
                _ => panic!("unexpected command"),
            }
        }
    }

    #[test]
    fn parses_desktop_widget_selection() {
        let cli = Cli::try_parse_from([
            "wb",
            "settings",
            "desktop",
            "w-clock",
            "w-weather",
            "w-ai",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Settings {
                op: SettingsOp::Desktop { widgets },
            } => assert_eq!(widgets, ["w-clock", "w-weather", "w-ai"]),
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn parses_update_check_command() {
        let cli = Cli::try_parse_from(["wb", "update", "check"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Update { op: UpdateOp::Check }));
    }

    #[test]
    fn parses_incremental_events_options() {
        let cli = Cli::try_parse_from([
            "wb",
            "events",
            "--after",
            "123",
            "--limit",
            "25",
            "--wait-ms",
            "30000",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Events {
                after,
                limit,
                wait_ms,
            } => {
                assert_eq!(after, 123);
                assert_eq!(limit, 25);
                assert_eq!(wait_ms, 30_000);
            }
            _ => panic!("unexpected command"),
        }
    }
}

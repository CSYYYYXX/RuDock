//! wb-daemon: resident JSON-RPC server over a Windows named pipe.
//! Single fact source; panel/CLI/MCP are equal clients.

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};
use interprocess::TryClone;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use wb_core::error::{CoreError, ErrorCode};
use wb_core::models::{ClipEntry, ClipKind, Note, TodoItem};
use wb_core::protocol::{Request, Response};
use wb_core::search::Searcher;
use wb_core::storage::Storage;
use wb_plugin_host::LoadedPlugin;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

mod clipboard;
mod panelctl;

struct Ctx {
    storage: Arc<Storage>,
    plugins: RwLock<Vec<LoadedPlugin>>,
}

/// 插件目录：%LOCALAPPDATA%/WB/plugins（用户安装）+ 仓库 plugins/（开发态，exe 上三级）。
fn user_plugin_dir() -> PathBuf {
    wb_core::paths::local_data_dir().join("plugins")
}

fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![user_plugin_dir()];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(repo) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            let dev = repo.join("plugins");
            if dev.is_dir() {
                dirs.push(dev);
            }
        }
    }
    dirs
}

fn plugin_id_ok(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn copy_plugin_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("创建安装目录失败: {e}"))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("读取插件目录失败: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取插件目录项失败: {e}"))?;
        let ty = entry.file_type().map_err(|e| format!("读取插件文件类型失败: {e}"))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_symlink() {
            return Err(format!("插件包含不支持的符号链接: {}", from.display()));
        }
        if ty.is_dir() {
            copy_plugin_tree(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to).map_err(|e| format!("复制插件文件失败 {}: {e}", from.display()))?;
        }
    }
    Ok(())
}

fn find_manifest_root(root: &Path) -> Result<PathBuf, String> {
    if root.join("plugin.json").is_file() {
        return Ok(root.to_path_buf());
    }
    let mut found = Vec::new();
    fn visit(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("读取压缩包目录失败: {e}"))? {
            let entry = entry.map_err(|e| format!("读取压缩包目录项失败: {e}"))?;
            let ty = entry.file_type().map_err(|e| format!("读取压缩包文件类型失败: {e}"))?;
            if ty.is_symlink() {
                continue;
            }
            let path = entry.path();
            if ty.is_dir() {
                if path.join("plugin.json").is_file() {
                    found.push(path);
                } else {
                    visit(&path, found)?;
                }
            }
        }
        Ok(())
    }
    visit(root, &mut found)?;
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err("压缩包内找不到 plugin.json".into()),
        _ => Err("压缩包内包含多个插件，请一次安装一个插件".into()),
    }
}

fn read_manifest(dir: &Path) -> Result<wb_plugin_sdk::Manifest, String> {
    let text = std::fs::read_to_string(dir.join("plugin.json"))
        .map_err(|e| format!("读取 plugin.json 失败: {e}"))?;
    let manifest: wb_plugin_sdk::Manifest = serde_json::from_str(&text)
        .map_err(|e| format!("plugin.json 格式错误: {e}"))?;
    manifest.validate().map_err(|e| format!("插件校验失败: {e}"))?;
    Ok(manifest)
}

fn install_plugin(source: &str) -> Result<wb_plugin_sdk::Manifest, String> {
    let input = PathBuf::from(source);
    let input = input
        .canonicalize()
        .map_err(|e| format!("插件源不存在: {source} ({e})"))?;
    let input = if input.to_string_lossy().starts_with(r"\\?\") {
        PathBuf::from(&input.to_string_lossy()[4..])
    } else {
        input
    };
    let mut temp: Option<PathBuf> = None;
    let root = if input.is_dir() {
        input
    } else if input.is_file() && input.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("zip")).unwrap_or(false) {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        let tmp = user_plugin_dir().join(format!(".install-{stamp}-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;
        let ps_quote = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "''"));
        let script = format!(
            "Expand-Archive -LiteralPath {} -DestinationPath {} -Force -ErrorAction Stop",
            ps_quote(&input), ps_quote(&tmp)
        );
        let mut cmd = std::process::Command::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = cmd.output().map_err(|e| format!("解压插件失败: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("解压插件失败: {}", err.trim()));
        }
        let found = find_manifest_root(&tmp)?;
        temp = Some(tmp);
        found
    } else {
        return Err("插件源必须是目录或 .zip 文件".into());
    };

    let manifest = read_manifest(&root)?;
    let target_root = user_plugin_dir();
    std::fs::create_dir_all(&target_root).map_err(|e| format!("创建插件目录失败: {e}"))?;
    let staging = target_root.join(format!(".{}-installing-{}", manifest.id, std::process::id()));
    let target = target_root.join(&manifest.id);
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = copy_plugin_tree(&root, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        if let Some(t) = temp { let _ = std::fs::remove_dir_all(t); }
        return Err(e);
    }
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| format!("替换旧插件失败: {e}"))?;
    }
    std::fs::rename(&staging, &target).map_err(|e| format!("提交插件安装失败: {e}"))?;
    if let Some(t) = temp { let _ = std::fs::remove_dir_all(t); }
    Ok(manifest)
}

fn remove_plugin(id: &str) -> Result<(), String> {
    if !plugin_id_ok(id) {
        return Err(format!("非法插件 id: {id}"));
    }
    let target = user_plugin_dir().join(id);
    if !target.is_dir() {
        return Err(format!("用户插件不存在: {id}"));
    }
    std::fs::remove_dir_all(&target).map_err(|e| format!("删除插件失败: {e}"))
}

fn discover_plugins() -> Vec<LoadedPlugin> {
    let mut all: Vec<LoadedPlugin> = plugin_dirs()
        .iter()
        .flat_map(|d| wb_plugin_host::discover(d))
        .collect();
    // 用户目录优先，同 id 去重（开发目录里的同 id 插件被用户目录覆盖）
    all.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    all.dedup_by(|a, b| a.manifest.id == b.manifest.id);
    all
}

fn plugin_revision(p: &LoadedPlugin) -> u128 {
    let mut revision = 0u128;
    let mut consider = |path: PathBuf| {
        if let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) {
            if let Ok(ms) = modified.duration_since(UNIX_EPOCH) {
                revision = revision.max(ms.as_millis());
            }
        }
    };
    consider(p.dir.join("plugin.json"));
    if let Some(widget) = &p.manifest.widget {
        consider(p.dir.join(&widget.file));
    }
    revision
}

fn default_settings() -> serde_json::Value {
    serde_json::json!({"takeover_win": true, "autostart": false})
}

fn read_settings() -> serde_json::Value {
    let path = wb_core::paths::settings_path();
    let Ok(text) = std::fs::read_to_string(path) else { return default_settings(); };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text) else { return default_settings(); };
    let Some(obj) = value.as_object_mut() else { return default_settings(); };
    let defaults = default_settings();
    for (k, v) in defaults.as_object().unwrap() {
        obj.entry(k.clone()).or_insert_with(|| v.clone());
    }
    value
}

fn write_settings(value: &serde_json::Value) -> Result<(), String> {
    let path = wb_core::paths::settings_path();
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败: {e}"))?; }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写设置失败: {e}"))?;
    if path.exists() { std::fs::remove_file(&path).map_err(|e| format!("替换旧设置失败: {e}"))?; }
    std::fs::rename(&tmp, &path).map_err(|e| format!("提交设置失败: {e}"))
}

fn hook_exe() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("wb-hook-poc.exe"))).filter(|p| p.is_file())
}

fn hook_running() -> bool {
    std::process::Command::new("tasklist").args(["/FI", "IMAGENAME eq wb-hook-poc.exe", "/FO", "CSV", "/NH"])
        .output().ok().map(|o| String::from_utf8_lossy(&o.stdout).contains("wb-hook-poc.exe")).unwrap_or(false)
}

fn set_hook_running(enabled: bool) -> Result<(), String> {
    if enabled {
        if hook_running() { return Ok(()); }
        let Some(exe) = hook_exe() else { return Err("wb-hook-poc.exe 不在当前产物目录".into()); };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--panel").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(windows)] cmd.creation_flags(CREATE_NO_WINDOW | 0x0000_0008);
        cmd.spawn().map_err(|e| format!("启动 Win 键钩子失败: {e}"))?;
    } else if hook_running() {
        let status = std::process::Command::new("taskkill").args(["/F", "/IM", "wb-hook-poc.exe"]).status().map_err(|e| format!("停止 Win 键钩子失败: {e}"))?;
        if !status.success() { return Err("停止 Win 键钩子失败".into()); }
    }
    Ok(())
}

fn set_autostart(enabled: bool) -> Result<(), String> {
    let Some(exe) = hook_exe() else { return Err("wb-hook-poc.exe 不在当前产物目录".into()); };
    let command = format!("\"{}\" --panel", exe.display());
    let status = if enabled {
        std::process::Command::new("reg").args(["ADD", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/V", "WB", "/T", "REG_SZ", "/D", &command, "/F"]).status()
    } else {
        std::process::Command::new("reg").args(["DELETE", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/V", "WB", "/F"]).status()
    }.map_err(|e| format!("设置开机自启失败: {e}"))?;
    if !status.success() && enabled { return Err("设置开机自启失败".into()); }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("wb-daemon fatal: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let name = wb_core::paths::pipe_name().to_ns_name::<GenericNamespaced>()?;
    let listener = ListenerOptions::new().name(name).create_sync()?;
    let storage = Arc::new(Storage::open(&wb_core::paths::db_path())?);
    let plugins = discover_plugins();
    eprintln!("wb-daemon: 已加载 {} 个插件（{:?}）", plugins.len(), plugin_dirs());
    let ctx = Arc::new(Ctx {
        storage: Arc::clone(&storage),
        plugins: RwLock::new(plugins),
    });
    let settings = read_settings();
    if settings.get("takeover_win").and_then(|v| v.as_bool()).unwrap_or(true) {
        if let Err(e) = set_hook_running(true) { eprintln!("wb-daemon: Win 键接管启动失败: {e}"); }
    }
    ctx.storage.audit("daemon", "daemon.start", env!("CARGO_PKG_VERSION"))?;
    clipboard::start(storage);
    log_everything_presence();
    eprintln!("wb-daemon listening on named pipe: {}", wb_core::paths::pipe_name());

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let ctx = Arc::clone(&ctx);
                std::thread::spawn(move || {
                    if let Err(e) = handle_conn(stream, ctx) {
                        eprintln!("conn error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
    Ok(())
}

fn handle_conn(stream: interprocess::local_socket::Stream, ctx: Arc<Ctx>) -> wb_core::Result<()> {
    let mut reader = BufReader::new(stream.try_clone().map_err(io_err)?);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(io_err)?;
        if n == 0 {
            return Ok(()); // client hung up
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => dispatch(&ctx, req),
            Err(e) => Response::err(
                serde_json::Value::Null,
                &CoreError::new(ErrorCode::InvalidParams, format!("bad request: {e}")),
            ),
        };
        let mut out = serde_json::to_string(&resp)?;
        out.push('\n');
        writer.write_all(out.as_bytes()).map_err(io_err)?;
        writer.flush().map_err(io_err)?;
    }
}

fn io_err(e: std::io::Error) -> CoreError {
    CoreError::new(ErrorCode::Internal, format!("io: {e}"))
}

/// Everything (voidtools) detection — WM_COPYDATA query client lands in M1.5;
/// absent → search degrades to local stores + Start-menu apps (by design).
fn log_everything_presence() {
    unsafe {
        let hwnd = windows::Win32::UI::WindowsAndMessaging::FindWindowW(
            windows::core::w!("EVERYTHING_TASKBAR_NOTIFICATION"),
            None,
        );
        let present = hwnd.is_ok() && !hwnd.unwrap_or_default().0.is_null();
        eprintln!("wb-daemon: Everything present: {present}");
    }
}

fn dispatch(ctx: &Ctx, req: Request) -> Response {
    let id = req.id.clone();
    match call(ctx, &req.method, &req.params) {
        Ok(v) => Response::ok(id, v),
        Err(e) => Response::err(id, &e),
    }
}

fn str_param<'a>(params: &'a serde_json::Value, key: &str) -> wb_core::Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| CoreError::new(ErrorCode::InvalidParams, format!("missing param: {key}")))
}

fn call(ctx: &Ctx, method: &str, params: &serde_json::Value) -> wb_core::Result<serde_json::Value> {
    let _ = ctx.storage.audit("client", method, &params.to_string());
    match method {
        "daemon.ping" => Ok(serde_json::json!({
            "name": "wb-daemon",
            "version": env!("CARGO_PKG_VERSION"),
            "status": "ok",
        })),

        "schema" => Ok(wb_core::protocol::schema()),

        "settings.get" => {
            let mut settings = read_settings();
            if let Some(obj) = settings.as_object_mut() { obj.insert("hook_running".into(), serde_json::json!(hook_running())); }
            Ok(settings)
        }

        "settings.set" => {
            let mut settings = read_settings();
            let obj = settings.as_object_mut().unwrap();
            if let Some(value) = params.get("takeover_win").and_then(|v| v.as_bool()) {
                set_hook_running(value).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
                obj.insert("takeover_win".into(), serde_json::json!(value));
                if !value {
                    let _ = set_autostart(false);
                    obj.insert("autostart".into(), serde_json::json!(false));
                }
            }
            if let Some(value) = params.get("autostart").and_then(|v| v.as_bool()) {
                set_autostart(value).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
                obj.insert("autostart".into(), serde_json::json!(value));
                if value {
                    set_hook_running(true).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
                    obj.insert("takeover_win".into(), serde_json::json!(true));
                }
            }
            write_settings(&settings).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
            if let Some(obj) = settings.as_object_mut() { obj.insert("hook_running".into(), serde_json::json!(hook_running())); }
            Ok(settings)
        }

        "hook.status" => Ok(serde_json::json!({"running":hook_running(),"exe":hook_exe()})),

        "search" => {
            let query = str_param(params, "query")?;
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let results = Searcher::new(&ctx.storage).search(query, limit);
            if results.is_empty() {
                return Err(CoreError::new(ErrorCode::NoResults, format!("no results for: {query}")));
            }
            Ok(serde_json::to_value(results)?)
        }

        "note.add" => {
            let note = Note::new(
                wb_core::models::new_id(),
                str_param(params, "content")?.to_string(),
                serde_json::from_value(params.get("tags").cloned().unwrap_or(serde_json::json!([])))?,
            );
            ctx.storage.note_add(&note)?;
            Ok(serde_json::to_value(note)?)
        }
        "note.list" => {
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            Ok(serde_json::to_value(ctx.storage.note_list(limit)?)?)
        }
        "note.get" => Ok(serde_json::to_value(ctx.storage.note_get(str_param(params, "id")?)?)?),
        "note.rm" => {
            let id = str_param(params, "id")?;
            ctx.storage.note_rm(id)?;
            Ok(serde_json::json!({"removed": id}))
        }

        "todo.add" => {
            let item = TodoItem {
                id: wb_core::models::new_id(),
                title: str_param(params, "title")?.to_string(),
                done: false,
                due: params.get("due").and_then(|v| v.as_str()).map(String::from),
                repeat: params.get("repeat").and_then(|v| v.as_str()).map(String::from),
                tags: serde_json::from_value(params.get("tags").cloned().unwrap_or(serde_json::json!([])))?,
                created_at: chrono::Utc::now(),
            };
            ctx.storage.todo_add(&item)?;
            Ok(serde_json::to_value(item)?)
        }
        "todo.list" => {
            let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(serde_json::to_value(ctx.storage.todo_list(all)?)?)
        }
        "todo.done" => {
            let id = str_param(params, "id")?;
            ctx.storage.todo_set_done(id, true)?;
            Ok(serde_json::json!({"done": id}))
        }
        "todo.rm" => {
            let id = str_param(params, "id")?;
            ctx.storage.todo_rm(id)?;
            Ok(serde_json::json!({"removed": id}))
        }

        "clip.get" => {
            let last = params.get("last").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            Ok(serde_json::to_value(ctx.storage.clip_list(last)?)?)
        }
        "clip.add" => {
            let kind = match str_param(params, "kind")? {
                "image" => ClipKind::Image,
                "files" => ClipKind::Files,
                _ => ClipKind::Text,
            };
            let entry = ClipEntry {
                id: wb_core::models::new_id(),
                kind,
                content: str_param(params, "content")?.to_string(),
                created_at: chrono::Utc::now(),
            };
            ctx.storage.clip_add(&entry)?;
            Ok(serde_json::to_value(entry)?)
        }
        "clip.clear" => {
            let n = ctx.storage.clip_clear()?;
            Ok(serde_json::json!({"cleared": n}))
        }

        "apps.list" => Ok(serde_json::to_value(wb_core::search::list_apps())?),
        "recent.list" => {
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(6) as usize;
            Ok(serde_json::to_value(wb_core::search::list_recent_files(limit))?)
        }

        // ---- M3.5：面板控制 / AI 同步问答 / 命令注册表 ----
        "panel.show" => Ok(panelctl::show()),
        "panel.hide" => Ok(panelctl::hide()),
        "panel.toggle" => Ok(panelctl::toggle()),

        "agent.ask" => {
            let prompt = str_param(params, "prompt")?;
            let text = wb_core::ai::ask_sync(prompt)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("ai: {e}")))?;
            Ok(serde_json::json!({"text": text, "model": wb_core::ai::model_name()}))
        }

        // ---- M4：插件系统（命令注册表合并 + 进程式执行 + 挂件） ----
        "cmd.list" => {
            let mut v = wb_core::commands::list_json();
            let arr = v.as_array_mut().unwrap();
            for p in ctx.plugins.read().unwrap().iter() {
                for c in &p.manifest.commands {
                    arr.push(serde_json::json!({
                        "id": c.id,
                        "title": c.title,
                        "hint": c.hint,
                        "arg": c.arg.as_ref().map(|a| serde_json::json!({"name": a.name, "prompt": a.prompt})),
                        "source": "plugin",
                        "plugin": p.manifest.id,
                    }));
                }
            }
            Ok(v)
        }

        "cmd.tools" => {
            // AI function calling 的 tools：注册表内建 + 插件中声明了 ai 的命令
            let mut v = wb_core::commands::tools_json();
            let arr = v.as_array_mut().unwrap();
            for p in ctx.plugins.read().unwrap().iter() {
                for c in &p.manifest.commands {
                    if let Some(ai) = &c.ai {
                        arr.push(serde_json::json!({
                            "type": "function",
                            "name": wb_plugin_sdk::Manifest::tool_name(&c.id),
                            "description": ai.description,
                            "parameters": {
                                "type": "object",
                                "properties": ai.properties,
                                "required": ai.required,
                                "additionalProperties": false,
                            },
                        }));
                    }
                }
            }
            Ok(v)
        }

        "cmd.run" => {
            let cmd_id = str_param(params, "id")?;
            let args = params.get("args").cloned().unwrap_or(serde_json::json!({}));
            run_command(ctx, cmd_id, args)
        }

        "plugin.list" => {
            let list: Vec<serde_json::Value> = ctx
                .plugins
                .read()
                .unwrap()
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "id": p.manifest.id,
                        "name": p.manifest.name,
                        "version": p.manifest.version,
                        "description": p.manifest.description,
                        "author": p.manifest.author,
                        "permissions": p.manifest.permissions,
                        "commands": p.manifest.commands.iter().map(|c| serde_json::json!({
                            "id": c.id, "title": c.title, "ai": c.ai.is_some(),
                        })).collect::<Vec<_>>(),
                        "widget": p.manifest.widget.as_ref().map(|w| serde_json::json!({
                            "title": w.title, "span": w.span.unwrap_or(2),
                        })),
                        "revision": plugin_revision(p),
                        "skills": p.manifest.skills.iter().map(|s| serde_json::json!({
                            "id": s.id, "name": s.name, "description": s.description, "tags": s.tags,
                        })).collect::<Vec<_>>(),
                        "dir": p.dir.to_string_lossy(),
                    })
                })
                .collect();
            Ok(serde_json::Value::Array(list))
        }

        "plugin.reload" => {
            let found = discover_plugins();
            let n = found.len();
            *ctx.plugins.write().unwrap() = found;
            Ok(serde_json::json!({"reloaded": n}))
        }

        "plugin.install" => {
            let source = str_param(params, "source")?;
            let manifest = install_plugin(source)
                .map_err(|e| CoreError::new(ErrorCode::InvalidParams, e))?;
            let found = discover_plugins();
            let n = found.len();
            *ctx.plugins.write().unwrap() = found;
            Ok(serde_json::json!({
                "installed": manifest.id,
                "name": manifest.name,
                "version": manifest.version,
                "reloaded": n,
            }))
        }

        "plugin.remove" => {
            let id = str_param(params, "id")?;
            remove_plugin(id).map_err(|e| CoreError::new(ErrorCode::InvalidParams, e))?;
            let found = discover_plugins();
            let n = found.len();
            *ctx.plugins.write().unwrap() = found;
            Ok(serde_json::json!({"removed": id, "reloaded": n}))
        }

        "plugin.widget" => {
            let pid = str_param(params, "id")?;
            let guard = ctx.plugins.read().unwrap();
            let p = guard
                .iter()
                .find(|p| p.manifest.id == pid)
                .ok_or_else(|| CoreError::new(ErrorCode::InvalidParams, format!("unknown plugin: {pid}")))?;
            let html = wb_plugin_host::widget_html(p)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("widget: {e}")))?;
            let w = p.manifest.widget.clone().unwrap();
            Ok(serde_json::json!({"title": w.title, "span": w.span.unwrap_or(2), "html": html}))
        }

        "skill.list" => {
            let list: Vec<serde_json::Value> = ctx
                .plugins
                .read()
                .unwrap()
                .iter()
                .flat_map(|p| {
                    p.manifest.skills.iter().map(|s| serde_json::json!({
                        "plugin": p.manifest.id,
                        "plugin_name": p.manifest.name,
                        "id": s.id,
                        "name": s.name,
                        "description": s.description,
                        "tags": s.tags,
                    }))
                })
                .collect();
            Ok(serde_json::Value::Array(list))
        }

        "skill.get" => {
            let pid = str_param(params, "plugin")?;
            let sid = str_param(params, "id")?;
            let guard = ctx.plugins.read().unwrap();
            let p = guard
                .iter()
                .find(|p| p.manifest.id == pid)
                .ok_or_else(|| CoreError::new(ErrorCode::InvalidParams, format!("unknown plugin: {pid}")))?;
            let (skill, content) = wb_plugin_host::skill_content(p, sid)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("skill: {e}")))?;
            Ok(serde_json::json!({
                "plugin": pid,
                "id": skill.id,
                "name": skill.name,
                "description": skill.description,
                "tags": skill.tags,
                "content": content,
            }))
        }

        "plugin.run" => {
            let pid = str_param(params, "name")?;
            let args = params.get("args").cloned().unwrap_or(serde_json::json!({}));
            let guard = ctx.plugins.read().unwrap();
            let p = guard
                .iter()
                .find(|p| p.manifest.id == pid)
                .ok_or_else(|| CoreError::new(ErrorCode::InvalidParams, format!("unknown plugin: {pid}")))?;
            let cmd_id = params
                .get("command")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| p.manifest.commands.first().map(|c| c.id.clone()))
                .ok_or_else(|| CoreError::new(ErrorCode::InvalidParams, format!("插件 {pid} 无命令")))?;
            drop(guard);
            run_plugin_command(ctx, pid, &cmd_id, args)
        }

        "audit.tail" => {
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            Ok(serde_json::to_value(ctx.storage.audit_tail(limit)?)?)
        }

        "events.tail" => Err(CoreError::new(
            ErrorCode::Unimplemented,
            format!("{method} lands in a later milestone"),
        )),

        other => Err(CoreError::new(ErrorCode::InvalidParams, format!("unknown method: {other}"))),
    }
}

/// `cmd.run` 执行器：注册表里的特殊命令就地执行，其余直接当 daemon 方法转发。
fn run_command(ctx: &Ctx, id: &str, args: serde_json::Value) -> wb_core::Result<serde_json::Value> {
    match id {
        "panel.show" => Ok(panelctl::show()),
        "panel.hide" => Ok(panelctl::hide()),
        "panel.toggle" => Ok(panelctl::toggle()),
        "system.lock" => {
            // 经典可靠路子，零 API 风险
            let ok = std::process::Command::new("rundll32.exe")
                .args(["user32.dll,LockWorkStation"])
                .spawn()
                .is_ok();
            if ok {
                Ok(serde_json::json!({"locked": true}))
            } else {
                Err(CoreError::new(ErrorCode::Internal, "LockWorkStation spawn failed"))
            }
        }
        other => {
            // 注册表里的其余 id 就是 daemon 方法名（todo.add / note.add / search / clip.* / agent.ask）
            if wb_core::commands::registry().iter().any(|c| c.id == other) {
                call(ctx, other, &args)
            } else {
                run_plugin_command(ctx, "", other, args)
            }
        }
    }
}

/// 插件命令执行：`plugin.run`（按插件 id）与 `cmd.run`（按命令 id 反查）共用。
/// 先 clone 出插件再执行，避免持读锁跑子进程（10s 超时会堵死其他请求）。
fn run_plugin_command(
    ctx: &Ctx,
    pid: &str,
    cmd_id: &str,
    args: serde_json::Value,
) -> wb_core::Result<serde_json::Value> {
    let plugin: LoadedPlugin = {
        let guard = ctx.plugins.read().unwrap();
        let found = if pid.is_empty() {
            wb_plugin_host::find_command(&guard, cmd_id).map(|(p, _)| p.clone())
        } else {
            guard
                .iter()
                .find(|p| p.manifest.id == pid && p.manifest.commands.iter().any(|c| c.id == cmd_id))
                .cloned()
        };
        found.ok_or_else(|| {
            CoreError::new(
                ErrorCode::InvalidParams,
                if pid.is_empty() {
                    format!("unknown command: {cmd_id}")
                } else {
                    format!("unknown plugin command: {pid}/{cmd_id}")
                },
            )
        })?
    };
    wb_plugin_host::run_command(&plugin, cmd_id, &args)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("plugin: {e}")))
}

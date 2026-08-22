//! wb-plugin-host: 插件宿主（M4）——发现、校验、进程式执行、挂件 HTML 读取。
//!
//! 执行模型（参考 Raycast/uTools 的进程隔离）：每条插件命令 = 拉起一次 handler 子进程，
//! stdin 喂 `{"command": id, "args": {...}}`，stdout 收一个 JSON 值，10s 超时强杀。
//! 解释器按扩展名映射：.ps1 → powershell / .js → node / .py → python / 其余直接执行。
//! 插件是用户自己安装的本地代码；宿主负责路径约束、输出限额和进程超时，
//! 权限批准与能力路由由 daemon 统一执行。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use wb_plugin_sdk::Manifest;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const RUN_TIMEOUT: Duration = Duration::from_secs(10);
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const HANDLER_OUTPUT_MAX_BYTES: usize = 1024 * 1024;
const WIDGET_MAX_BYTES: u64 = 256 * 1024;
const SKILL_MAX_BYTES: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PipeKind {
    Stdout,
    Stderr,
}

#[derive(Debug)]
struct PipeCapture {
    kind: PipeKind,
    bytes: Vec<u8>,
    exceeded_limit: bool,
    error: Option<String>,
}

fn plugin_file(p: &LoadedPlugin, relative: &str, kind: &str) -> Result<PathBuf, String> {
    let root = p
        .dir
        .canonicalize()
        .map_err(|e| format!("插件目录读取失败: {e}"))?;
    let path = p
        .dir
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("{kind} 文件读取失败: {e}"))?;
    if !path.starts_with(&root) {
        return Err(format!("{kind} 路径越出插件目录: {relative}"));
    }
    if !path.is_file() {
        return Err(format!("{kind} 不是文件: {}", path.display()));
    }
    Ok(path)
}

fn spawn_pipe_reader<R>(kind: PipeKind, mut reader: R, tx: Sender<PipeCapture>)
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut exceeded_limit = false;
        let mut error = None;
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let remaining = HANDLER_OUTPUT_MAX_BYTES.saturating_sub(bytes.len());
                    bytes.extend_from_slice(&buffer[..read.min(remaining)]);
                    exceeded_limit |= read > remaining;
                }
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        let _ = tx.send(PipeCapture {
            kind,
            bytes,
            exceeded_limit,
            error,
        });
    });
}

fn collect_pipes(rx: Receiver<PipeCapture>) -> Result<(PipeCapture, PipeCapture), String> {
    let deadline = Instant::now() + PIPE_DRAIN_TIMEOUT;
    let mut stdout = None;
    let mut stderr = None;
    while stdout.is_none() || stderr.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let capture = rx
            .recv_timeout(remaining)
            .map_err(|_| "handler 退出后输出管道未及时关闭".to_string())?;
        match capture.kind {
            PipeKind::Stdout => stdout = Some(capture),
            PipeKind::Stderr => stderr = Some(capture),
        }
    }
    Ok((stdout.unwrap(), stderr.unwrap()))
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

/// 扫描 plugins 根目录：每个含 plugin.json 的子文件夹是一个插件。
/// 无效插件跳过并 eprintln 记录（不让一个烂插件拖垮全部）。
pub fn discover(root: &Path) -> Vec<LoadedPlugin> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for ent in rd.flatten() {
        let dir = ent.path();
        if !dir.is_dir() {
            continue;
        }
        let mpath = dir.join("plugin.json");
        let Ok(text) = std::fs::read_to_string(&mpath) else {
            continue;
        };
        match serde_json::from_str::<Manifest>(&text)
            .map_err(|e| e.to_string())
            .and_then(|m| m.validate().map(|_| m))
        {
            Ok(manifest) => out.push(LoadedPlugin { dir, manifest }),
            Err(e) => eprintln!(
                "wb-plugin-host: 跳过无效插件 {:?}: {e}",
                dir.file_name().unwrap_or_default()
            ),
        }
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    out
}

/// 找到声明了某命令 id 的插件与命令。
pub fn find_command<'a>(
    plugins: &'a [LoadedPlugin],
    cmd_id: &str,
) -> Option<(&'a LoadedPlugin, &'a wb_plugin_sdk::CommandSpec)> {
    plugins.iter().find_map(|p| {
        p.manifest
            .commands
            .iter()
            .find(|c| c.id == cmd_id)
            .map(|c| (p, c))
    })
}

/// 执行插件命令：spawn handler → stdin JSON → stdout JSON。
pub fn run_command(
    p: &LoadedPlugin,
    cmd_id: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handler = p.manifest.handler.clone().ok_or("插件无 handler")?;
    let hpath = plugin_file(p, &handler, "handler")?;
    let ext = hpath
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut cmd = match ext.as_str() {
        "ps1" => {
            // Windows PowerShell 5.1 默认按 ANSI 读写管道 → 强制控制台 UTF-8 后再跑脚本
            let path = hpath.to_string_lossy().replace('\'', "''");
            let mut c = Command::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
            c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]).arg(format!(
                "[Console]::InputEncoding=[System.Text.Encoding]::UTF8; [Console]::OutputEncoding=[System.Text.Encoding]::UTF8; & '{path}'"
            ));
            c
        }
        "js" => {
            let mut c = Command::new("node");
            c.arg(&hpath);
            c
        }
        "py" => {
            let mut c = Command::new("python");
            c.env("PYTHONIOENCODING", "utf-8").arg(&hpath);
            c
        }
        _ => Command::new(&hpath),
    };
    cmd.current_dir(&p.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd.spawn().map_err(|e| format!("spawn {handler}: {e}"))?;
    let payload = serde_json::json!({"command": cmd_id, "args": args}).to_string();
    child
        .stdin
        .as_mut()
        .ok_or("no stdin")?
        .write_all(payload.as_bytes())
        .map_err(|e| format!("write stdin: {e}"))?;
    drop(child.stdin.take());

    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let (tx, rx) = mpsc::channel();
    spawn_pipe_reader(PipeKind::Stdout, stdout, tx.clone());
    spawn_pipe_reader(PipeKind::Stderr, stderr, tx);

    // 输出从进程启动起就在独立线程中持续排空，避免管道缓冲区填满后互相等待。
    let start = Instant::now();
    loop {
        match child.try_wait().map_err(|e| format!("wait: {e}"))? {
            Some(status) => {
                let (out, err) = collect_pipes(rx)?;
                if out.exceeded_limit || err.exceeded_limit {
                    return Err(format!(
                        "handler 输出超过 {}KB 限制",
                        HANDLER_OUTPUT_MAX_BYTES / 1024
                    ));
                }
                if let Some(e) = out.error {
                    return Err(format!("read stdout: {e}"));
                }
                if let Some(e) = err.error {
                    return Err(format!("read stderr: {e}"));
                }
                let out = String::from_utf8_lossy(&out.bytes);
                let err = String::from_utf8_lossy(&err.bytes);
                if !status.success() {
                    return Err(format!(
                        "handler 退出码 {:?}: {}",
                        status.code(),
                        err.trim().chars().take(300).collect::<String>()
                    ));
                }
                let out = out.trim();
                if out.is_empty() {
                    return Ok(serde_json::Value::Null);
                }
                // 严格 JSON 优先；解析失败则包成 {"text": ...} 容错（社区插件鱼龙混杂）
                return Ok(serde_json::from_str(out).unwrap_or_else(
                    |_| serde_json::json!({"text": out.chars().take(4000).collect::<String>()}),
                ));
            }
            None => {
                if start.elapsed() > RUN_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("handler 超时（{}s）已终止", RUN_TIMEOUT.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    }
}

/// 读挂件 HTML（限 256KB，防巨型文件撑爆 WebView）。
pub fn widget_html(p: &LoadedPlugin) -> Result<String, String> {
    let w = p.manifest.widget.clone().ok_or("插件无 widget")?;
    let path = plugin_file(p, &w.file, "widget")?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("widget 文件读取失败: {e}"))?;
    if meta.len() > WIDGET_MAX_BYTES {
        return Err(format!("widget 文件超过 {}KB", WIDGET_MAX_BYTES / 1024));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("widget 文件读取失败: {e}"))
}

/// 读取插件声明的 Skill 文档，供 CLI/daemon/MCP 等 Agent 客户端使用。
pub fn skill_content(
    p: &LoadedPlugin,
    skill_id: &str,
) -> Result<(wb_plugin_sdk::SkillSpec, String), String> {
    let skill = p
        .manifest
        .skills
        .iter()
        .find(|s| s.id == skill_id)
        .cloned()
        .ok_or_else(|| format!("插件无 skill: {skill_id}"))?;
    let path = plugin_file(p, &skill.file, "skill")?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("skill 文件读取失败: {e}"))?;
    if meta.len() > SKILL_MAX_BYTES {
        return Err(format!("skill 文件超过 {}KB", SKILL_MAX_BYTES / 1024));
    }
    let content = std::fs::read_to_string(&path).map_err(|e| format!("skill 文件读取失败: {e}"))?;
    Ok((skill, content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_skips_garbage() {
        let root = std::env::temp_dir().join(format!("wb-phost-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nope")).unwrap(); // 无 plugin.json
        std::fs::create_dir_all(root.join("bad")).unwrap();
        std::fs::write(root.join("bad/plugin.json"), "{not json").unwrap();
        std::fs::create_dir_all(root.join("good")).unwrap();
        std::fs::write(
            root.join("good/plugin.json"),
            r#"{"id":"good","name":"G","version":"0.1.0","handler":"main.ps1","commands":[{"id":"util.g","title":"G"}],"permissions":["process"]}"#,
        )
        .unwrap();
        let found = discover(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.id, "good");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reads_skill_content_with_metadata() {
        let root = std::env::temp_dir().join(format!("wb-phost-skill-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("SKILL.md"), "# Triage\nUse this workflow.").unwrap();
        let p = LoadedPlugin {
            dir: root.clone(),
            manifest: serde_json::from_value(serde_json::json!({
                "id": "skill-test", "name": "Skill Test", "version": "0.1.0",
                "skills": [{"id":"triage","name":"Triage","file":"SKILL.md"}]
            }))
            .unwrap(),
        };
        let (spec, content) = skill_content(&p, "triage").unwrap();
        assert_eq!(spec.name, "Triage");
        assert!(content.contains("Use this workflow"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(windows)]
    fn runs_ps1_handler() {
        let root = std::env::temp_dir().join(format!("wb-phost-run-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.ps1"),
            "$in = [Console]::In.ReadToEnd() | ConvertFrom-Json; @{ echo = $in.args.name } | ConvertTo-Json -Compress",
        )
        .unwrap();
        let p = LoadedPlugin {
            dir: root.clone(),
            manifest: serde_json::from_str::<Manifest>(
                r#"{"id":"t","name":"T","version":"0.1.0","handler":"main.ps1","commands":[{"id":"util.t","title":"T"}],"permissions":["process"]}"#,
            )
            .unwrap(),
        };
        let v = run_command(&p, "util.t", &serde_json::json!({"name": "WB"})).unwrap();
        assert_eq!(v.get("echo").and_then(|s| s.as_str()), Some("WB"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(windows)]
    fn drains_large_handler_output_without_deadlock() {
        let root = std::env::temp_dir().join(format!("wb-phost-large-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.ps1"),
            "$null = [Console]::In.ReadToEnd(); @{ text = ('x' * 262144) } | ConvertTo-Json -Compress",
        )
        .unwrap();
        let p = LoadedPlugin {
            dir: root.clone(),
            manifest: serde_json::from_str::<Manifest>(
                r#"{"id":"large","name":"Large","version":"0.1.0","handler":"main.ps1","commands":[{"id":"util.large","title":"Large"}],"permissions":["process"]}"#,
            )
            .unwrap(),
        };
        let v = run_command(&p, "util.large", &serde_json::json!({})).unwrap();
        assert_eq!(v["text"].as_str().map(str::len), Some(262144));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_runtime_path_escape() {
        let parent = std::env::temp_dir().join(format!("wb-phost-escape-{}", std::process::id()));
        let root = parent.join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(parent.join("outside.js"), "{}").unwrap();
        let p = LoadedPlugin {
            dir: root,
            manifest: serde_json::from_value(serde_json::json!({
                "id": "escape", "name": "Escape", "version": "0.1.0",
                "handler": "../outside.js", "commands": [{"id":"util.escape","title":"Escape"}],
                "permissions": ["process"]
            }))
            .unwrap(),
        };
        let error = run_command(&p, "util.escape", &serde_json::json!({})).unwrap_err();
        assert!(error.contains("路径越出插件目录"), "{error}");
        std::fs::remove_dir_all(parent).ok();
    }
}

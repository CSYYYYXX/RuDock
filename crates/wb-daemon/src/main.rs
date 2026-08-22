//! wb-daemon: resident JSON-RPC server over a Windows named pipe.
//! Single fact source; panel/CLI/MCP are equal clients.

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};
use interprocess::TryClone;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use wb_core::error::{CoreError, ErrorCode};
use wb_core::models::{ClipEntry, ClipKind, Note, ResultKind, SearchResult, TodoItem};
use wb_core::protocol::{Request, Response};
use wb_core::search::Searcher;
use wb_core::storage::Storage;
use wb_plugin_host::LoadedPlugin;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
use windows::core::w;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
#[cfg(windows)]
use windows::Win32::System::Threading::CreateMutexW;

mod clipboard;
mod everything;
mod panelctl;
mod tray;

struct Ctx {
    storage: Arc<Storage>,
    plugins: RwLock<Vec<LoadedPlugin>>,
    apps: RwLock<Vec<SearchResult>>,
    files: RwLock<Vec<SearchResult>>,
    plugin_tx: Mutex<()>,
    settings_tx: Mutex<()>,
}

/// 插件目录：%LOCALAPPDATA%/WB/plugins（用户安装）+ 仓库 plugins/（开发态，exe 上三级）。
fn user_plugin_dir() -> PathBuf {
    wb_core::paths::local_data_dir().join("plugins")
}

fn plugin_install_work_dir() -> PathBuf {
    wb_core::paths::local_data_dir().join("plugin-installs")
}

fn plugin_backup_dir() -> PathBuf {
    wb_core::paths::local_data_dir().join("plugin-backups")
}

fn cleanup_plugin_backups() {
    let root = plugin_backup_dir();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(manifest) = read_manifest(&path) else {
            continue;
        };
        if user_plugin_dir().join(manifest.id).is_dir() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![user_plugin_dir()];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(repo) = exe
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            let dev = repo.join("plugins");
            if dev.is_dir() {
                dirs.push(dev);
            }
        }
    }
    dirs
}

fn plugin_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn copy_plugin_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("创建安装目录失败: {e}"))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("读取插件目录失败: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取插件目录项失败: {e}"))?;
        let ty = entry
            .file_type()
            .map_err(|e| format!("读取插件文件类型失败: {e}"))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_symlink() {
            return Err(format!("插件包含不支持的符号链接: {}", from.display()));
        }
        if ty.is_dir() {
            copy_plugin_tree(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("复制插件文件失败 {}: {e}", from.display()))?;
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
        for entry in std::fs::read_dir(dir).map_err(|e| format!("读取压缩包目录失败: {e}"))?
        {
            let entry = entry.map_err(|e| format!("读取压缩包目录项失败: {e}"))?;
            let ty = entry
                .file_type()
                .map_err(|e| format!("读取压缩包文件类型失败: {e}"))?;
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
    let manifest: wb_plugin_sdk::Manifest =
        serde_json::from_str(&text).map_err(|e| format!("plugin.json 格式错误: {e}"))?;
    manifest
        .validate()
        .map_err(|e| format!("插件校验失败: {e}"))?;
    Ok(manifest)
}

const REMOTE_ARCHIVE_MAX_BYTES: u64 = 32 * 1024 * 1024;
const MARKET_INDEX_MAX_BYTES: u64 = 2 * 1024 * 1024;
const INSTALL_TREE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const INSTALL_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const INSTALL_TREE_MAX_FILES: usize = 512;
const INSTALL_TREE_MAX_ENTRIES: usize = 1024;
const INSTALL_TREE_MAX_DEPTH: usize = 16;

fn is_remote_source(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    source.starts_with("https://") || source.starts_with("http://")
}

fn normalize_sha256(value: &str) -> Result<String, String> {
    let value = value.trim();
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("SHA-256 必须是 64 位十六进制字符串".into());
    }
    Ok(value.to_ascii_lowercase())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("读取插件归档失败 {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("读取插件归档失败 {}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn verify_archive(path: &Path, expected: &str) -> Result<String, String> {
    let expected = normalize_sha256(expected)?;
    let size = std::fs::metadata(path)
        .map_err(|e| format!("读取插件归档元数据失败: {e}"))?
        .len();
    if size > REMOTE_ARCHIVE_MAX_BYTES {
        return Err(format!(
            "插件归档超过 {}MB",
            REMOTE_ARCHIVE_MAX_BYTES / 1024 / 1024
        ));
    }
    let actual = sha256_file(path)?;
    if actual != expected {
        return Err(format!(
            "插件归档 SHA-256 不匹配: expected {expected}, actual {actual}"
        ));
    }
    Ok(actual)
}

fn download_http(
    source: &str,
    output_path: &Path,
    max_bytes: u64,
    max_time_seconds: u64,
    label: &str,
) -> Result<(), String> {
    let mut cmd = std::process::Command::new("curl.exe");
    cmd.args([
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "--connect-timeout",
        "10",
        "--max-time",
        &max_time_seconds.to_string(),
        "--max-filesize",
        &max_bytes.to_string(),
        "--output",
        &output_path.to_string_lossy(),
        source,
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let result = cmd.output().map_err(|e| format!("下载{label}失败: {e}"))?;
    if !result.status.success() {
        let _ = std::fs::remove_file(output_path);
        return Err(format!(
            "下载{label}失败: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    let size = match std::fs::metadata(output_path) {
        Ok(metadata) => metadata.len(),
        Err(e) => {
            let _ = std::fs::remove_file(output_path);
            return Err(format!("读取{label}元数据失败: {e}"));
        }
    };
    if size > max_bytes {
        let _ = std::fs::remove_file(output_path);
        return Err(format!("{label}超过 {}MB", max_bytes / 1024 / 1024));
    }
    Ok(())
}

fn download_plugin(source: &str, expected: Option<&str>) -> Result<(PathBuf, String), String> {
    let expected = expected.ok_or_else(|| "远程插件安装必须提供 --sha256".to_string())?;
    let expected = normalize_sha256(expected)?;
    let root = plugin_install_work_dir();
    std::fs::create_dir_all(&root).map_err(|e| format!("创建插件目录失败: {e}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let archive = root.join(format!(".download-{stamp}-{}.zip", std::process::id()));
    download_http(source, &archive, REMOTE_ARCHIVE_MAX_BYTES, 60, "插件")?;
    match verify_archive(&archive, &expected) {
        Ok(actual) => Ok((archive, actual)),
        Err(e) => {
            let _ = std::fs::remove_file(&archive);
            Err(e)
        }
    }
}

fn validate_install_tree(root: &Path) -> Result<(), String> {
    fn visit(
        dir: &Path,
        depth: usize,
        files: &mut usize,
        bytes: &mut u64,
    ) -> Result<(), String> {
        if depth > INSTALL_TREE_MAX_DEPTH {
            return Err(format!("插件目录层级超过 {INSTALL_TREE_MAX_DEPTH}"));
        }
        for entry in std::fs::read_dir(dir).map_err(|e| format!("读取插件目录失败: {e}"))? {
            let entry = entry.map_err(|e| format!("读取插件目录项失败: {e}"))?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("读取插件文件元数据失败: {e}"))?;
            if meta.file_type().is_symlink() {
                return Err(format!("插件包含不支持的符号链接: {}", path.display()));
            }
            if meta.is_dir() {
                visit(&path, depth + 1, files, bytes)?;
            } else if meta.is_file() {
                *files += 1;
                *bytes = bytes.saturating_add(meta.len());
                if *files > INSTALL_TREE_MAX_FILES {
                    return Err(format!("插件文件数超过 {INSTALL_TREE_MAX_FILES}"));
                }
                if meta.len() > INSTALL_FILE_MAX_BYTES {
                    return Err(format!(
                        "插件单文件超过 {}MB: {}",
                        INSTALL_FILE_MAX_BYTES / 1024 / 1024,
                        path.display()
                    ));
                }
                if *bytes > INSTALL_TREE_MAX_BYTES {
                    return Err(format!(
                        "插件解压后超过 {}MB",
                        INSTALL_TREE_MAX_BYTES / 1024 / 1024
                    ));
                }
            }
        }
        Ok(())
    }

    let mut files = 0;
    let mut bytes = 0;
    visit(root, 0, &mut files, &mut bytes)
}

fn safe_zip_path(path: &Path) -> Result<PathBuf, String> {
    use std::path::Component;

    let mut safe = PathBuf::new();
    let mut depth = 0;
    for component in path.components() {
        let Component::Normal(segment) = component else {
            return Err(format!("ZIP 包含非法路径: {}", path.display()));
        };
        let segment = segment
            .to_str()
            .ok_or_else(|| "ZIP 文件名必须是 UTF-8".to_string())?;
        let normalized = segment.trim_end_matches([' ', '.']).to_ascii_lowercase();
        let stem = normalized.split('.').next().unwrap_or_default();
        let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
            || stem
                .strip_prefix("com")
                .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
            || stem
                .strip_prefix("lpt")
                .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
        if normalized.is_empty()
            || normalized != segment.to_ascii_lowercase()
            || segment.contains(':')
            || reserved
        {
            return Err(format!("ZIP 包含 Windows 非法文件名: {segment}"));
        }
        depth += 1;
        if depth > INSTALL_TREE_MAX_DEPTH {
            return Err(format!("插件目录层级超过 {INSTALL_TREE_MAX_DEPTH}"));
        }
        safe.push(segment);
    }
    if safe.as_os_str().is_empty() {
        return Err("ZIP 包含空路径".into());
    }
    Ok(safe)
}

fn extract_plugin_archive(archive_path: &Path, output_root: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| format!("读取插件 ZIP 失败 {}: {e}", archive_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("插件 ZIP 格式错误: {e}"))?;
    if archive.len() > INSTALL_TREE_MAX_ENTRIES {
        return Err(format!("插件 ZIP 条目数超过 {INSTALL_TREE_MAX_ENTRIES}"));
    }

    let mut seen = std::collections::HashSet::new();
    let mut files = 0usize;
    let mut declared_bytes = 0u64;
    let mut written_bytes = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取插件 ZIP 条目失败: {e}"))?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| format!("ZIP 包含越界路径: {}", entry.name()))?;
        let relative = safe_zip_path(&enclosed)?;
        let key = relative.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
        if !seen.insert(key) {
            return Err(format!("ZIP 包含重复或大小写冲突路径: {}", relative.display()));
        }

        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o040000 && kind != 0o100000 {
                return Err(format!("ZIP 包含不支持的特殊文件: {}", relative.display()));
            }
        }
        let target = output_root.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("创建插件目录失败 {}: {e}", target.display()))?;
            continue;
        }

        files += 1;
        if files > INSTALL_TREE_MAX_FILES {
            return Err(format!("插件文件数超过 {INSTALL_TREE_MAX_FILES}"));
        }
        if entry.size() > INSTALL_FILE_MAX_BYTES {
            return Err(format!(
                "插件单文件超过 {}MB: {}",
                INSTALL_FILE_MAX_BYTES / 1024 / 1024,
                relative.display()
            ));
        }
        declared_bytes = declared_bytes.saturating_add(entry.size());
        if declared_bytes > INSTALL_TREE_MAX_BYTES {
            return Err(format!(
                "插件解压后超过 {}MB",
                INSTALL_TREE_MAX_BYTES / 1024 / 1024
            ));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建插件目录失败 {}: {e}", parent.display()))?;
        }
        let mut output = std::fs::File::create(&target)
            .map_err(|e| format!("创建插件文件失败 {}: {e}", target.display()))?;
        let written = std::io::copy(
            &mut entry.take(INSTALL_FILE_MAX_BYTES + 1),
            &mut output,
        )
        .map_err(|e| format!("解压插件文件失败 {}: {e}", relative.display()))?;
        if written > INSTALL_FILE_MAX_BYTES {
            return Err(format!(
                "插件单文件实际解压超过 {}MB: {}",
                INSTALL_FILE_MAX_BYTES / 1024 / 1024,
                relative.display()
            ));
        }
        written_bytes = written_bytes.saturating_add(written);
        if written_bytes > INSTALL_TREE_MAX_BYTES {
            return Err(format!(
                "插件实际解压超过 {}MB",
                INSTALL_TREE_MAX_BYTES / 1024 / 1024
            ));
        }
    }
    Ok(())
}

fn install_plugin_path(
    input: &Path,
    expected_identity: Option<(&str, &str)>,
) -> Result<wb_plugin_sdk::Manifest, String> {
    let input = input
        .canonicalize()
        .map_err(|e| format!("插件源不存在: {} ({e})", input.display()))?;
    let input = if input.to_string_lossy().starts_with(r"\\?\") {
        PathBuf::from(&input.to_string_lossy()[4..])
    } else {
        input
    };
    let mut temp: Option<PathBuf> = None;
    let root = if input.is_dir() {
        input
    } else if input.is_file()
        && input
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("zip"))
            .unwrap_or(false)
    {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let tmp = plugin_install_work_dir().join(format!(
            "extract-{stamp}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;
        if let Err(e) = extract_plugin_archive(&input, &tmp) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        if let Err(e) = validate_install_tree(&tmp) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        let found = match find_manifest_root(&tmp) {
            Ok(found) => found,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
        };
        temp = Some(tmp);
        found
    } else {
        return Err("插件源必须是目录或 .zip 文件".into());
    };

    let result = (|| {
        validate_install_tree(&root)?;
        let manifest = read_manifest(&root)?;
        if let Some((expected_id, expected_version)) = expected_identity {
            if manifest.id != expected_id || manifest.version != expected_version {
                return Err(format!(
                    "插件包身份与市场索引不匹配: expected {expected_id}@{expected_version}, actual {}@{}",
                    manifest.id, manifest.version
                ));
            }
        }
        let candidate = LoadedPlugin {
            dir: root.clone(),
            manifest: manifest.clone(),
        };
        wb_plugin_host::validate_files(&candidate)
            .map_err(|e| format!("插件文件校验失败: {e}"))?;
        let target_root = user_plugin_dir();
        std::fs::create_dir_all(&target_root)
            .map_err(|e| format!("创建插件目录失败: {e}"))?;
        let work_root = plugin_install_work_dir();
        std::fs::create_dir_all(&work_root)
            .map_err(|e| format!("创建插件安装工作目录失败: {e}"))?;
        let backup_root = plugin_backup_dir();
        std::fs::create_dir_all(&backup_root)
            .map_err(|e| format!("创建插件备份目录失败: {e}"))?;
        let transaction = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let staging = work_root.join(format!(
            "{}-staging-{}-{transaction}",
            manifest.id,
            std::process::id()
        ));
        let backup = backup_root.join(format!(
            "{}-backup-{}-{transaction}",
            manifest.id,
            std::process::id()
        ));
        let target = target_root.join(&manifest.id);
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir_all(&backup);
        if let Err(e) = copy_plugin_tree(&root, &staging) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
        let had_target = target.exists();
        if had_target {
            if let Err(e) = std::fs::rename(&target, &backup) {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(format!("暂存旧插件失败: {e}"));
            }
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            let _ = std::fs::remove_dir_all(&staging);
            if had_target {
                if let Err(rollback) = std::fs::rename(&backup, &target) {
                    return Err(format!(
                        "提交插件安装失败: {e}; 回滚旧版本失败: {rollback}; 备份位于 {}",
                        backup.display()
                    ));
                }
            }
            return Err(format!("提交插件安装失败: {e}"));
        }
        if had_target {
            let _ = std::fs::remove_dir_all(&backup);
        }
        Ok(manifest)
    })();
    if let Some(t) = temp {
        let _ = std::fs::remove_dir_all(t);
    }
    result
}

fn install_plugin(
    source: &str,
    expected_sha256: Option<&str>,
) -> Result<(wb_plugin_sdk::Manifest, Option<String>), String> {
    install_plugin_checked(source, expected_sha256, None)
}

fn install_plugin_checked(
    source: &str,
    expected_sha256: Option<&str>,
    expected_identity: Option<(&str, &str)>,
) -> Result<(wb_plugin_sdk::Manifest, Option<String>), String> {
    if is_remote_source(source) {
        let (archive, actual) = download_plugin(source, expected_sha256)?;
        let result = install_plugin_path(&archive, expected_identity)
            .map(|manifest| (manifest, Some(actual)));
        let _ = std::fs::remove_file(archive);
        return result;
    }

    let input = PathBuf::from(source);
    let actual = if let Some(expected) = expected_sha256 {
        if input.is_dir() {
            return Err("--sha256 只适用于 ZIP 文件或远程 URL".into());
        }
        Some(verify_archive(&input, expected)?)
    } else {
        None
    };
    install_plugin_path(&input, expected_identity).map(|manifest| (manifest, actual))
}

struct LoadedMarket {
    source: String,
    local_base: Option<PathBuf>,
    index: wb_plugin_sdk::MarketIndex,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MarketSource {
    name: String,
    index: String,
}

fn read_bounded(path: &Path, max_bytes: u64, label: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("读取{label}失败 {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取{label}失败 {}: {e}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("{label}超过 {}MB", max_bytes / 1024 / 1024));
    }
    Ok(bytes)
}

fn parse_market_index(bytes: &[u8]) -> Result<wb_plugin_sdk::MarketIndex, String> {
    let index: wb_plugin_sdk::MarketIndex = serde_json::from_slice(bytes)
        .map_err(|e| format!("插件市场索引格式错误: {e}"))?;
    index
        .validate()
        .map_err(|e| format!("插件市场索引校验失败: {e}"))?;
    Ok(index)
}

fn load_market(source: &str) -> Result<LoadedMarket, String> {
    if is_remote_source(source) {
        let root = plugin_install_work_dir();
        std::fs::create_dir_all(&root).map_err(|e| format!("创建插件目录失败: {e}"))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = root.join(format!(
            ".market-{}-{stamp}.json",
            std::process::id()
        ));
        download_http(source, &path, MARKET_INDEX_MAX_BYTES, 15, "插件市场索引")?;
        let result = read_bounded(&path, MARKET_INDEX_MAX_BYTES, "插件市场索引")
            .and_then(|bytes| parse_market_index(&bytes));
        let _ = std::fs::remove_file(path);
        return result.map(|index| LoadedMarket {
            source: source.into(),
            local_base: None,
            index,
        });
    }

    let path = PathBuf::from(source)
        .canonicalize()
        .map_err(|e| format!("插件市场索引不存在: {source} ({e})"))?;
    if !path.is_file() {
        return Err("插件市场索引必须是 JSON 文件".into());
    }
    let bytes = read_bounded(&path, MARKET_INDEX_MAX_BYTES, "插件市场索引")?;
    let index = parse_market_index(&bytes)?;
    let base = path
        .parent()
        .ok_or_else(|| "无法定位插件市场索引目录".to_string())?
        .to_path_buf();
    Ok(LoadedMarket {
        source: path.to_string_lossy().into_owned(),
        local_base: Some(base),
        index,
    })
}

fn resolve_market_download(
    market: &LoadedMarket,
    plugin: &wb_plugin_sdk::MarketPlugin,
) -> Result<String, String> {
    if is_remote_source(&plugin.download) {
        return Ok(plugin.download.clone());
    }
    let Some(base) = &market.local_base else {
        return Err(format!(
            "远程市场插件 {} 的 download 必须是绝对 HTTP(S) URL",
            plugin.id
        ));
    };
    let relative = Path::new(&plugin.download);
    if relative.is_absolute() {
        return Err(format!("本地市场插件 {} 的 download 必须是相对路径", plugin.id));
    }
    let archive = base
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("市场插件归档不存在 {}: {e}", relative.display()))?;
    if !archive.starts_with(base) {
        return Err(format!("市场插件 {} 的 download 越过索引目录", plugin.id));
    }
    Ok(archive.to_string_lossy().into_owned())
}

fn market_status(
    plugin: &wb_plugin_sdk::MarketPlugin,
    installed: Option<&LoadedPlugin>,
) -> (&'static str, bool) {
    let Some(installed) = installed else {
        return ("available", false);
    };
    match plugin.compare_installed_version(&installed.manifest.version) {
        Ok(std::cmp::Ordering::Greater) => ("update_available", true),
        Ok(std::cmp::Ordering::Equal) => ("installed", false),
        Ok(std::cmp::Ordering::Less) => ("ahead", false),
        Err(_) => ("invalid_installed_version", false),
    }
}

fn market_json(
    market: &LoadedMarket,
    installed: &[LoadedPlugin],
    updates_only: bool,
) -> serde_json::Value {
    let mut plugins = market.index.plugins.clone();
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    let mut rows = Vec::new();
    let mut update_count = 0usize;
    for plugin in plugins {
        let installed_plugin = installed.iter().find(|p| p.manifest.id == plugin.id);
        let (status, update_available) = market_status(&plugin, installed_plugin);
        if update_available {
            update_count += 1;
        }
        if updates_only && !update_available {
            continue;
        }
        rows.push(serde_json::json!({
            "id": plugin.id,
            "name": plugin.name,
            "version": plugin.version,
            "market": market.index.name,
            "index": market.source,
            "description": plugin.description,
            "author": plugin.author,
            "download": plugin.download,
            "sha256": format!("sha256:{}", plugin.normalized_sha256().unwrap_or_default()),
            "homepage": plugin.homepage,
            "tags": plugin.tags,
            "installed_version": installed_plugin.map(|p| p.manifest.version.clone()),
            "status": status,
            "update_available": update_available,
        }));
    }
    serde_json::json!({
        "market": market.index.name,
        "schema_version": market.index.schema_version,
        "index": market.source,
        "updates": update_count,
        "plugins": rows,
    })
}

fn aggregate_markets_json(
    installed: &[LoadedPlugin],
    updates_only: bool,
) -> serde_json::Value {
    let sources = market_sources_from_settings(&read_settings());
    let mut source_states = Vec::new();
    let mut rows = Vec::new();
    let mut updates = 0u64;
    let mut errors = 0usize;
    for source in sources {
        match load_market(&source.index) {
            Ok(market) => {
                let mut value = market_json(&market, installed, updates_only);
                let count = value["plugins"].as_array().map_or(0, Vec::len);
                updates += value["updates"].as_u64().unwrap_or(0);
                if let Some(items) = value["plugins"].as_array_mut() {
                    rows.append(items);
                }
                source_states.push(serde_json::json!({
                    "name": market.index.name,
                    "index": market.source,
                    "status": "ok",
                    "plugins": count,
                }));
            }
            Err(error) => {
                errors += 1;
                source_states.push(serde_json::json!({
                    "name": source.name,
                    "index": source.index,
                    "status": "error",
                    "error": error,
                }));
            }
        }
    }
    rows.sort_by(|a, b| {
        b["update_available"]
            .as_bool()
            .cmp(&a["update_available"].as_bool())
            .then_with(|| {
                a["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(b["name"].as_str().unwrap_or_default())
            })
    });
    let source_count = source_states.len();
    serde_json::json!({
        "sources": source_states,
        "source_count": source_count,
        "errors": errors,
        "updates": updates,
        "plugins": rows,
    })
}

fn market_for_plugin(
    index_source: Option<&str>,
    id: &str,
) -> Result<(LoadedMarket, wb_plugin_sdk::MarketPlugin), String> {
    let sources = if let Some(source) = index_source {
        vec![MarketSource {
            name: String::new(),
            index: source.into(),
        }]
    } else {
        let sources = market_sources_from_settings(&read_settings());
        if sources.is_empty() {
            return Err("尚未配置插件市场源，请先添加市场源或提供 --index".into());
        }
        sources
    };
    let mut matches = Vec::new();
    for source in sources {
        let market = load_market(&source.index)
            .map_err(|e| format!("读取市场源 {} 失败: {e}", source.index))?;
        if let Some(plugin) = market
            .index
            .plugins
            .iter()
            .find(|plugin| plugin.id == id)
            .cloned()
        {
            matches.push((market, plugin));
        }
    }
    match matches.len() {
        0 => Err(format!("插件市场中不存在: {id}")),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "多个市场源都包含插件 {id}，请使用 --index 指定来源"
        )),
    }
}

fn install_market_plugin(
    index_source: Option<&str>,
    id: &str,
    installed: &[LoadedPlugin],
    update_only: bool,
) -> Result<(wb_plugin_sdk::Manifest, Option<String>, String, String), String> {
    let (market, plugin) = market_for_plugin(index_source, id)?;
    if update_only {
        let current = installed
            .iter()
            .find(|current| current.manifest.id == id)
            .ok_or_else(|| format!("插件尚未安装: {id}"))?;
        match plugin.compare_installed_version(&current.manifest.version) {
            Ok(std::cmp::Ordering::Greater) => {}
            Ok(_) => return Err(format!("插件 {id} 已是最新版本或高于市场版本")),
            Err(e) => return Err(format!("无法比较插件 {id} 版本: {e}")),
        }
    }
    let download = resolve_market_download(&market, &plugin)?;
    let sha256 = plugin.normalized_sha256()?;
    let (manifest, actual) = install_plugin_checked(
        &download,
        Some(&sha256),
        Some((&plugin.id, &plugin.version)),
    )?;
    Ok((manifest, actual, market.index.name, market.source))
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
    let mut command_ids: std::collections::HashSet<String> = wb_core::commands::registry()
        .iter()
        .map(|c| c.id.to_string())
        .collect();
    let mut tool_names: std::collections::HashSet<String> = wb_core::commands::tools_json()
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(|v| v.as_str()).map(String::from))
        .collect();
    tool_names.extend(["skill_list".into(), "skill_get".into()]);
    all.into_iter()
        .filter(|plugin| {
            let command_collision = plugin
                .manifest
                .commands
                .iter()
                .find(|c| command_ids.contains(&c.id));
            let tool_collision = plugin
                .manifest
                .commands
                .iter()
                .filter(|c| c.ai.is_some())
                .map(|c| wb_plugin_sdk::Manifest::tool_name(&c.id))
                .find(|name| tool_names.contains(name));
            if let Some(command) = command_collision {
                eprintln!(
                    "wb-daemon: 跳过插件 {}，命令 id 冲突: {}",
                    plugin.manifest.id, command.id
                );
                return false;
            }
            if let Some(tool) = tool_collision {
                eprintln!(
                    "wb-daemon: 跳过插件 {}，AI 工具名冲突: {}",
                    plugin.manifest.id, tool
                );
                return false;
            }
            for command in &plugin.manifest.commands {
                command_ids.insert(command.id.clone());
                if command.ai.is_some() {
                    tool_names.insert(wb_plugin_sdk::Manifest::tool_name(&command.id));
                }
            }
            true
        })
        .collect()
}

fn plugin_files(p: &LoadedPlugin) -> Vec<PathBuf> {
    let mut files = vec![p.dir.join("plugin.json")];
    if let Some(handler) = &p.manifest.handler {
        files.push(p.dir.join(handler));
    }
    if let Some(widget) = &p.manifest.widget {
        files.push(p.dir.join(&widget.file));
    }
    files.extend(
        p.manifest
            .skills
            .iter()
            .map(|skill| p.dir.join(&skill.file)),
    );
    files.sort();
    files.dedup();
    files
}

fn plugin_revision(p: &LoadedPlugin) -> u128 {
    let mut revision = 0u128;
    for path in plugin_files(p) {
        if let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) {
            if let Ok(ms) = modified.duration_since(UNIX_EPOCH) {
                revision = revision.max(ms.as_millis());
            }
        }
    }
    revision
}

// Bind a grant to the exact local plugin files. Length framing prevents two
// different path/content segmentations from hashing as the same byte stream.
fn plugin_fingerprint(p: &LoadedPlugin) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let root = p.dir.canonicalize().ok()?;
    for path in plugin_files(p) {
        let path = path.canonicalize().ok()?;
        if !path.starts_with(&root) {
            return None;
        }
        let relative = path.strip_prefix(&root).ok()?.to_string_lossy();
        hash.update((relative.len() as u64).to_le_bytes());
        hash.update(relative.as_bytes());
        let bytes = std::fs::read(&path).ok()?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    let digest = hash.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(format!("sha256:{hex}"))
}

fn default_settings() -> serde_json::Value {
    serde_json::json!({
        "takeover_win": false,
        "autostart": false,
        "mcp_write_policy": "client",
        "desktop_widgets": [],
        "plugin_grants": {},
        "plugin_markets": [],
    })
}

fn read_settings() -> serde_json::Value {
    let path = wb_core::paths::settings_path();
    let Ok(text) = std::fs::read_to_string(path) else {
        return default_settings();
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return default_settings();
    };
    let Some(obj) = value.as_object_mut() else {
        return default_settings();
    };
    let defaults = default_settings();
    for (k, v) in defaults.as_object().unwrap() {
        obj.entry(k.clone()).or_insert_with(|| v.clone());
    }
    value
}

fn desktop_widgets_from_settings(settings: &serde_json::Value) -> Vec<String> {
    settings
        .get("desktop_widgets")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(String::from))
        .collect()
}

fn normalize_desktop_widgets(value: &serde_json::Value) -> wb_core::Result<Vec<String>> {
    let values = value.as_array().ok_or_else(|| {
        CoreError::new(
            ErrorCode::InvalidParams,
            "desktop_widgets must be a string array",
        )
    })?;
    if values.len() > 32 {
        return Err(CoreError::new(
            ErrorCode::InvalidParams,
            "desktop_widgets supports at most 32 entries",
        ));
    }
    let mut widgets = Vec::with_capacity(values.len());
    let mut seen = std::collections::HashSet::new();
    for value in values {
        let id = value.as_str().ok_or_else(|| {
            CoreError::new(
                ErrorCode::InvalidParams,
                "desktop_widgets must contain strings",
            )
        })?;
        if id.len() > 128
            || (!id.starts_with("w-") && !id.starts_with("plugin-"))
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(CoreError::new(
                ErrorCode::InvalidParams,
                format!("invalid desktop widget id: {id}"),
            ));
        }
        if seen.insert(id.to_string()) {
            widgets.push(id.to_string());
        }
    }
    Ok(widgets)
}

fn market_sources_from_settings(settings: &serde_json::Value) -> Vec<MarketSource> {
    settings
        .get("plugin_markets")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<MarketSource>(value.clone()).ok())
        .filter(|source| {
            !source.name.trim().is_empty()
                && !source.index.trim().is_empty()
                && source.name.len() <= 120
                && source.index.len() <= 2048
        })
        .collect()
}

fn set_market_sources(settings: &mut serde_json::Value, sources: &[MarketSource]) {
    settings.as_object_mut().unwrap().insert(
        "plugin_markets".into(),
        serde_json::to_value(sources).unwrap_or_else(|_| serde_json::json!([])),
    );
}

fn same_market_source(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn plugin_annotations_json(command: &wb_plugin_sdk::CommandSpec) -> serde_json::Value {
    let annotations = command.annotations;
    serde_json::json!({
        "title": command.title,
        "readOnlyHint": annotations.read_only_hint,
        "destructiveHint": annotations.destructive_hint,
        "idempotentHint": annotations.idempotent_hint,
        "openWorldHint": annotations.open_world_hint,
    })
}

fn write_settings(value: &serde_json::Value) -> Result<(), String> {
    let path = wb_core::paths::settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写设置失败: {e}"))?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("替换旧设置失败: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("提交设置失败: {e}"))
}

fn plugin_approved(plugin: &LoadedPlugin, settings: &serde_json::Value) -> bool {
    if plugin.manifest.permissions.is_empty() {
        return true;
    }
    let Some(grant) = settings.pointer(&format!("/plugin_grants/{}", plugin.manifest.id)) else {
        return false;
    };
    if grant.get("version").and_then(|v| v.as_str()) != Some(plugin.manifest.version.as_str()) {
        return false;
    }
    let Some(fingerprint) = plugin_fingerprint(plugin) else {
        return false;
    };
    if grant.get("fingerprint").and_then(|v| v.as_str()) != Some(fingerprint.as_str()) {
        return false;
    }
    let mut granted: Vec<String> = grant
        .get("permissions")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    granted.sort();
    granted == plugin.manifest.sorted_permissions()
}

fn approved_plugin(ctx: &Ctx, id: &str) -> wb_core::Result<LoadedPlugin> {
    let plugin = ctx
        .plugins
        .read()
        .unwrap()
        .iter()
        .find(|p| p.manifest.id == id)
        .cloned()
        .ok_or_else(|| CoreError::new(ErrorCode::NotFound, format!("unknown plugin: {id}")))?;
    if !plugin_approved(&plugin, &read_settings()) {
        return Err(CoreError::new(
            ErrorCode::PermissionDenied,
            format!("plugin {} requires approval", plugin.manifest.id),
        )
        .with_hint(format!("run `wb plugin approve {}`", plugin.manifest.id)));
    }
    Ok(plugin)
}

fn widget_rpc_permissions(method: &str) -> Option<&'static [&'static str]> {
    match method {
        "clip.get" => Some(&["clipboard.read"]),
        "clip.add" | "clip.clear" => Some(&["clipboard.write"]),
        "note.list" | "note.get" | "todo.list" => Some(&["data.read"]),
        "note.add" | "note.rm" | "todo.add" | "todo.done" | "todo.rm" => Some(&["data.write"]),
        "apps.list" | "recent.list" => Some(&["filesystem"]),
        "panel.show" | "panel.hide" | "panel.toggle" => Some(&["panel.control"]),
        _ => None,
    }
}

fn hook_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wb-hook-poc.exe")))
        .filter(|p| p.is_file())
}

fn daemon_exe() -> Option<PathBuf> {
    std::env::current_exe().ok().filter(|p| p.is_file())
}

#[cfg(windows)]
fn hook_running() -> bool {
    let Ok(handle) = (unsafe { CreateMutexW(None, false, w!("Local\\WBHookSingleInstance")) })
    else {
        return false;
    };
    let running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    unsafe {
        let _ = CloseHandle(handle);
    }
    running
}

#[cfg(not(windows))]
fn hook_running() -> bool {
    false
}

fn set_hook_running(enabled: bool) -> Result<(), String> {
    if enabled {
        if hook_running() {
            return Ok(());
        }
        let Some(exe) = hook_exe() else {
            return Err("wb-hook-poc.exe 不在当前产物目录".into());
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--panel")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW | 0x0000_0008);
        cmd.spawn()
            .map_err(|e| format!("启动 Win 键钩子失败: {e}"))?;
    } else if hook_running() {
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/F", "/IM", "wb-hook-poc.exe"]);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let status = cmd
            .status()
            .map_err(|e| format!("停止 Win 键钩子失败: {e}"))?;
        if !status.success() {
            return Err("停止 Win 键钩子失败".into());
        }
    }
    Ok(())
}

fn set_autostart(enabled: bool) -> Result<(), String> {
    let Some(exe) = daemon_exe() else {
        return Err("无法定位 wb-daemon.exe".into());
    };
    let command = format!("\"{}\"", exe.display());
    let mut cmd = std::process::Command::new("reg");
    if enabled {
        cmd.args([
                "ADD",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/V",
                "WB",
                "/T",
                "REG_SZ",
                "/D",
                &command,
                "/F",
            ]);
    } else {
        cmd.args([
                "DELETE",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/V",
                "WB",
                "/F",
            ]);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let status = cmd
        .status()
        .map_err(|e| format!("设置开机自启失败: {e}"))?;
    if !status.success() && enabled {
        return Err("设置开机自启失败".into());
    }
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
    let _ = std::fs::remove_dir_all(plugin_install_work_dir());
    cleanup_plugin_backups();
    let storage = Arc::new(Storage::open(&wb_core::paths::db_path())?);
    let plugins = discover_plugins();
    eprintln!(
        "wb-daemon: 已加载 {} 个插件（{:?}）",
        plugins.len(),
        plugin_dirs()
    );
    let apps = wb_core::search::index_apps();
    eprintln!("wb-daemon: 应用索引 {} 项", apps.len());
    let ctx = Arc::new(Ctx {
        storage: Arc::clone(&storage),
        plugins: RwLock::new(plugins),
        apps: RwLock::new(apps),
        files: RwLock::new(wb_core::search::list_recent_files(200)),
        plugin_tx: Mutex::new(()),
        settings_tx: Mutex::new(()),
    });
    {
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(300));
            let apps = wb_core::search::index_apps();
            eprintln!("wb-daemon: 应用索引已刷新，共 {} 项", apps.len());
            *ctx.apps.write().unwrap() = apps;
        });
    }
    {
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || {
            // Reserve room for the separate Recent-items provider while keeping
            // the complete in-memory file result set bounded at 50,000 entries.
            let mut files = wb_core::search::index_user_files(49_800);
            files.extend(wb_core::search::list_recent_files(200));
            let mut seen = std::collections::HashSet::new();
            files.retain(|f| {
                f.path
                    .as_ref()
                    .is_some_and(|p| seen.insert(p.to_lowercase()))
            });
            files.truncate(50_000);
            eprintln!("wb-daemon: 用户文件索引 {} 项", files.len());
            *ctx.files.write().unwrap() = files;
        });
    }
    let settings = read_settings();
    if settings
        .get("takeover_win")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        if let Err(e) = set_hook_running(true) {
            eprintln!("wb-daemon: Win 键接管启动失败: {e}");
        }
    }
    if !desktop_widgets_from_settings(&settings).is_empty() {
        panelctl::sync_desktop(true);
    }
    ctx.storage.audit_event(
        "daemon",
        "daemon.start",
        &serde_json::json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}),
    )?;
    clipboard::start(storage);
    tray::start();
    eprintln!(
        "wb-daemon: Everything IPC available: {}, database loaded: {}",
        everything::available(),
        everything::database_loaded()
    );
    eprintln!(
        "wb-daemon listening on named pipe: {}",
        wb_core::paths::pipe_name()
    );

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
        let (resp, stop_after_response) = match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => {
                let stop = req.method == "daemon.stop";
                (dispatch(&ctx, req), stop)
            }
            Err(e) => (
                Response::err(
                    serde_json::Value::Null,
                    &CoreError::new(ErrorCode::InvalidParams, format!("bad request: {e}")),
                ),
                false,
            ),
        };
        let should_stop = stop_after_response && resp.error.is_none();
        let mut out = serde_json::to_string(&resp)?;
        out.push('\n');
        writer.write_all(out.as_bytes()).map_err(io_err)?;
        writer.flush().map_err(io_err)?;
        if should_stop {
            std::process::exit(0);
        }
    }
}

fn io_err(e: std::io::Error) -> CoreError {
    CoreError::new(ErrorCode::Internal, format!("io: {e}"))
}

fn dispatch(ctx: &Ctx, req: Request) -> Response {
    let id = req.id.clone();
    let started = std::time::Instant::now();
    let result = call(ctx, &req.method, &req.params);
    if should_audit(&req.method) {
        let (status, error_code) = match &result {
            Ok(_) => ("ok", serde_json::Value::Null),
            Err(error) => (
                "error",
                serde_json::to_value(error.code).unwrap_or(serde_json::Value::Null),
            ),
        };
        let _ = ctx.storage.audit_event(
            audit_actor(&req.method, &req.params),
            &req.method,
            &serde_json::json!({
                "status": status,
                "error_code": error_code,
                "duration_ms": started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                "params": audit_params(&req.params),
            }),
        );
    }
    match result {
        Ok(value) => Response::ok(id, value),
        Err(error) => Response::err(id, &error),
    }
}

fn should_audit(method: &str) -> bool {
    !matches!(
        method,
        "daemon.ping"
            | "settings.get"
            | "hook.status"
            | "schema"
            | "cmd.list"
            | "cmd.tools"
            | "plugin.list"
            | "plugin.market.sources"
            | "plugin.market.list"
            | "plugin.market.check"
            | "skill.list"
            | "skill.get"
            | "audit.tail"
            | "events.tail"
            | "apps.list"
    )
}

fn audit_actor<'a>(method: &str, params: &'a serde_json::Value) -> &'a str {
    if method == "plugin.rpc" {
        return "widget";
    }
    match params.get("origin").and_then(|value| value.as_str()) {
        Some("mcp") => "mcp",
        Some("panel-ai") => "panel-ai",
        _ => "client",
    }
}

fn audit_params(params: &serde_json::Value) -> serde_json::Value {
    let Some(object) = params.as_object() else {
        return audit_value_shape(params);
    };
    let mut summary = serde_json::Map::new();
    for (key, value) in object {
        summary.insert(key.clone(), audit_value_shape(value));
    }
    serde_json::Value::Object(summary)
}

fn audit_value_shape(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Null => serde_json::json!({"type":"null"}),
        serde_json::Value::Bool(_) => serde_json::json!({"type":"boolean"}),
        serde_json::Value::Number(_) => serde_json::json!({"type":"number"}),
        serde_json::Value::String(text) => {
            serde_json::json!({"type":"string","length":text.chars().count()})
        }
        serde_json::Value::Array(items) => {
            serde_json::json!({"type":"array","length":items.len()})
        }
        serde_json::Value::Object(object) => {
            let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
            keys.sort_unstable();
            serde_json::json!({"type":"object","keys":keys})
        }
    }
}

fn str_param<'a>(params: &'a serde_json::Value, key: &str) -> wb_core::Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| CoreError::new(ErrorCode::InvalidParams, format!("missing param: {key}")))
}

fn call(ctx: &Ctx, method: &str, params: &serde_json::Value) -> wb_core::Result<serde_json::Value> {
    match method {
        "daemon.ping" => Ok(serde_json::json!({
            "name": "wb-daemon",
            "version": env!("CARGO_PKG_VERSION"),
            "status": "ok",
            "apps_indexed": ctx.apps.read().unwrap().len(),
            "apps_index_ready": true,
            "files_indexed": ctx.files.read().unwrap().len(),
            "everything_available": everything::available(),
            "everything_database_loaded": everything::database_loaded(),
        })),

        "daemon.stop" => {
            set_hook_running(false).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
            let panel = panelctl::close();
            let desktop = panelctl::sync_desktop(false);
            tray::remove();
            Ok(serde_json::json!({"status":"stopped","hook":"stopped","panel":panel["panel"],"desktop":desktop["desktop"]}))
        }

        "schema" => Ok(wb_core::protocol::schema()),

        "settings.get" => {
            let mut settings = read_settings();
            if let Some(obj) = settings.as_object_mut() {
                obj.insert("hook_running".into(), serde_json::json!(hook_running()));
                obj.insert("desktop_running".into(), serde_json::json!(panelctl::desktop_running()));
            }
            Ok(settings)
        }

        "settings.set" => {
            let _settings = ctx.settings_tx.lock().unwrap();
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
            if let Some(value) = params.get("mcp_write_policy") {
                let value = value.as_str().ok_or_else(|| {
                    CoreError::new(
                        ErrorCode::InvalidParams,
                        "mcp_write_policy must be client, ask, or read-only",
                    )
                })?;
                if !matches!(value, "client" | "ask" | "read-only") {
                    return Err(CoreError::new(
                        ErrorCode::InvalidParams,
                        "mcp_write_policy must be client, ask, or read-only",
                    ));
                }
                obj.insert("mcp_write_policy".into(), serde_json::json!(value));
            }
            if let Some(value) = params.get("desktop_widgets") {
                let widgets = normalize_desktop_widgets(value)?;
                obj.insert("desktop_widgets".into(), serde_json::json!(widgets));
            }
            write_settings(&settings).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
            panelctl::sync_desktop(!desktop_widgets_from_settings(&settings).is_empty());
            if let Some(obj) = settings.as_object_mut() {
                obj.insert("hook_running".into(), serde_json::json!(hook_running()));
                obj.insert("desktop_running".into(), serde_json::json!(panelctl::desktop_running()));
            }
            Ok(settings)
        }

        "hook.status" => Ok(serde_json::json!({"running":hook_running(),"exe":hook_exe()})),

        "search" => {
            let query = str_param(params, "query")?;
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .clamp(1, 200) as usize;
            let q = query.trim().to_lowercase();
            if q.is_empty() {
                return Err(CoreError::new(ErrorCode::NoResults, "empty search query"));
            }
            // Type filters are applied after aggregation. Ask providers for their
            // full bounded result sets first, otherwise a large app/clip set can
            // truncate a lower-scoring note or todo before it is filtered in.
            let provider_limit = if params.get("type").and_then(|v| v.as_str()).is_some() {
                usize::MAX
            } else {
                limit.max(100)
            };
            let apps = ctx.apps.read().unwrap().clone();
            let mut results = Searcher::new(&ctx.storage, &apps).search(query, provider_limit);
            let include_files = params
                .get("type")
                .and_then(|v| v.as_str())
                .is_none_or(|kind| kind == "file");
            if include_files {
                let file_limit = limit.max(100).min(200);
                match everything::search(query, file_limit) {
                    Ok(files) => results.extend(files),
                    Err(_) => results.extend(wb_core::search::search_indexed_files(
                        &ctx.files.read().unwrap(),
                        query,
                        provider_limit,
                    )),
                }
            }
            let settings = read_settings();
            for plugin in ctx.plugins.read().unwrap().iter() {
                if !plugin_approved(plugin, &settings) {
                    continue;
                }
                for command in &plugin.manifest.commands {
                    if command.id.to_lowercase().contains(&q)
                        || command.title.to_lowercase().contains(&q)
                        || command.hint.to_lowercase().contains(&q)
                    {
                        results.push(SearchResult {
                            kind: ResultKind::Plugin,
                            title: command.title.clone(),
                            subtitle: Some(format!("{} · {}", plugin.manifest.name, command.hint)),
                            preview: Some(command.hint.clone()),
                            path: Some(format!("wb://cmd/{}", command.id)),
                            score: if command.title.to_lowercase().starts_with(&q) {
                                0.86
                            } else {
                                0.67
                            },
                            source: "plugin".into(),
                        });
                    }
                }
            }
            if let Some(kind) = params.get("type").and_then(|v| v.as_str()) {
                results.retain(|r| {
                    matches!(
                        (kind, r.kind),
                        ("file", ResultKind::File)
                            | ("app", ResultKind::App)
                            | ("clip", ResultKind::Clip)
                            | ("note", ResultKind::Note)
                            | ("todo", ResultKind::Todo)
                            | ("plugin", ResultKind::Plugin)
                    )
                });
            }
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            results.truncate(limit);
            if results.is_empty() {
                return Err(CoreError::new(
                    ErrorCode::NoResults,
                    format!("no results for: {query}"),
                ));
            }
            Ok(serde_json::to_value(results)?)
        }

        "note.add" => {
            let note = Note::new(
                wb_core::models::new_id(),
                str_param(params, "content")?.to_string(),
                serde_json::from_value(
                    params.get("tags").cloned().unwrap_or(serde_json::json!([])),
                )?,
            );
            ctx.storage.note_add(&note)?;
            Ok(serde_json::to_value(note)?)
        }
        "note.list" => {
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            Ok(serde_json::to_value(ctx.storage.note_list(limit)?)?)
        }
        "note.get" => Ok(serde_json::to_value(
            ctx.storage.note_get(str_param(params, "id")?)?,
        )?),
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
                repeat: params
                    .get("repeat")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                tags: serde_json::from_value(
                    params.get("tags").cloned().unwrap_or(serde_json::json!([])),
                )?,
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

        "apps.list" => Ok(serde_json::to_value(ctx.apps.read().unwrap().as_slice())?),
        "recent.list" => {
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(6) as usize;
            Ok(serde_json::to_value(wb_core::search::list_recent_files(
                limit,
            ))?)
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
            let settings = read_settings();
            for p in ctx.plugins.read().unwrap().iter() {
                if !plugin_approved(p, &settings) {
                    continue;
                }
                for c in &p.manifest.commands {
                    arr.push(serde_json::json!({
                        "id": c.id,
                        "title": c.title,
                        "hint": c.hint,
                        "arg": c.arg.as_ref().map(|a| serde_json::json!({"name": a.name, "prompt": a.prompt})),
                        "annotations": plugin_annotations_json(c),
                        "source": "plugin",
                        "plugin": p.manifest.id,
                    }));
                }
            }
            Ok(v)
        }

        "cmd.tools" => {
            // AI function calling 的 tools：注册表内建 + 插件中声明了 ai 的命令
            let include_annotations = params
                .get("include_annotations")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let mut v = if include_annotations {
                wb_core::commands::tools_json_with_annotations()
            } else {
                wb_core::commands::tools_json()
            };
            let arr = v.as_array_mut().unwrap();
            let settings = read_settings();
            for p in ctx.plugins.read().unwrap().iter() {
                if !plugin_approved(p, &settings) {
                    continue;
                }
                for c in &p.manifest.commands {
                    if let Some(ai) = &c.ai {
                        let mut tool = serde_json::json!({
                            "type": "function",
                            "name": wb_plugin_sdk::Manifest::tool_name(&c.id),
                            "description": ai.description,
                            "parameters": {
                                "type": "object",
                                "properties": ai.properties,
                                "required": ai.required,
                                "additionalProperties": false,
                            },
                        });
                        if include_annotations {
                            tool["annotations"] = plugin_annotations_json(c);
                        }
                        arr.push(tool);
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

        "cmd.tool.run" => {
            let tool = str_param(params, "name")?;
            let args = params.get("args").cloned().unwrap_or(serde_json::json!({}));
            if let Some(method) = wb_core::commands::tool_to_method(tool) {
                return run_command(ctx, method, args);
            }
            let matches: Vec<String> = ctx
                .plugins
                .read()
                .unwrap()
                .iter()
                .flat_map(|p| p.manifest.commands.iter())
                .filter(|c| c.ai.is_some() && wb_plugin_sdk::Manifest::tool_name(&c.id) == tool)
                .map(|c| c.id.clone())
                .collect();
            match matches.as_slice() {
                [id] => run_command(ctx, id, args),
                [] => Err(CoreError::new(
                    ErrorCode::NotFound,
                    format!("unknown tool: {tool}"),
                )),
                _ => Err(CoreError::new(
                    ErrorCode::InvalidParams,
                    format!("ambiguous tool name: {tool}"),
                )),
            }
        }

        "plugin.list" => {
            let found = discover_plugins();
            *ctx.plugins.write().unwrap() = found;
            let settings = read_settings();
            let list: Vec<serde_json::Value> = ctx
                .plugins
                .read()
                .unwrap()
                .iter()
                .map(|p| {
                    let approved = plugin_approved(p, &settings);
                    serde_json::json!({
                        "id": p.manifest.id,
                        "name": p.manifest.name,
                        "version": p.manifest.version,
                        "description": p.manifest.description,
                        "author": p.manifest.author,
                        "permissions": p.manifest.permissions,
                        "approved": approved,
                        "approval_required": !p.manifest.permissions.is_empty() && !approved,
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
                        "user_installed": p.dir.starts_with(user_plugin_dir()),
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
            let _tx = ctx.plugin_tx.lock().unwrap();
            let source = str_param(params, "source")?;
            let expected_sha256 = params.get("sha256").and_then(|value| value.as_str());
            let (manifest, archive_sha256) = install_plugin(source, expected_sha256)
                .map_err(|e| CoreError::new(ErrorCode::InvalidParams, e))?;
            let found = discover_plugins();
            let n = found.len();
            *ctx.plugins.write().unwrap() = found;
            Ok(serde_json::json!({
                "installed": manifest.id,
                "name": manifest.name,
                "version": manifest.version,
                "permissions": manifest.permissions,
                "approval_required": !manifest.permissions.is_empty(),
                "archive_sha256": archive_sha256,
                "reloaded": n,
            }))
        }

        "plugin.market.sources" => Ok(serde_json::json!({
            "sources": market_sources_from_settings(&read_settings()),
        })),

        "plugin.market.source.add" => {
            let source = str_param(params, "index")?;
            if source.len() > 2048 {
                return Err(CoreError::new(
                    ErrorCode::InvalidParams,
                    "插件市场源地址超过 2048 字符",
                ));
            }
            let market = load_market(source)
                .map_err(|e| CoreError::new(ErrorCode::InvalidParams, e))?;
            let canonical = MarketSource {
                name: market.index.name.clone(),
                index: market.source.clone(),
            };
            let _settings = ctx.settings_tx.lock().unwrap();
            let mut settings = read_settings();
            let mut sources = market_sources_from_settings(&settings);
            let added = if let Some(existing) = sources
                .iter_mut()
                .find(|existing| same_market_source(&existing.index, &canonical.index))
            {
                existing.name = canonical.name.clone();
                false
            } else {
                if sources.len() >= 8 {
                    return Err(CoreError::new(
                        ErrorCode::InvalidParams,
                        "最多配置 8 个插件市场源",
                    ));
                }
                sources.push(canonical.clone());
                true
            };
            set_market_sources(&mut settings, &sources);
            write_settings(&settings).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
            Ok(serde_json::json!({
                "added": added,
                "source": canonical,
                "sources": sources,
            }))
        }

        "plugin.market.source.remove" => {
            let source = str_param(params, "index")?;
            let _settings = ctx.settings_tx.lock().unwrap();
            let mut settings = read_settings();
            let mut sources = market_sources_from_settings(&settings);
            let before = sources.len();
            sources.retain(|existing| !same_market_source(&existing.index, source));
            let removed = sources.len() != before;
            if removed {
                set_market_sources(&mut settings, &sources);
                write_settings(&settings)
                    .map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
            }
            Ok(serde_json::json!({
                "removed": removed,
                "index": source,
                "sources": sources,
            }))
        }

        "plugin.market.list" | "plugin.market.check" => {
            let installed = ctx.plugins.read().unwrap().clone();
            let updates_only = method == "plugin.market.check";
            if let Some(source) = params.get("index").and_then(|value| value.as_str()) {
                let market = load_market(source)
                    .map_err(|e| CoreError::new(ErrorCode::InvalidParams, e))?;
                Ok(market_json(&market, &installed, updates_only))
            } else {
                Ok(aggregate_markets_json(&installed, updates_only))
            }
        }

        "plugin.market.install" | "plugin.market.update" => {
            let _tx = ctx.plugin_tx.lock().unwrap();
            let source = params.get("index").and_then(|value| value.as_str());
            let id = str_param(params, "id")?;
            let installed = ctx.plugins.read().unwrap().clone();
            let previous_version = installed
                .iter()
                .find(|plugin| plugin.manifest.id == id)
                .map(|plugin| plugin.manifest.version.clone());
            let update_only = method == "plugin.market.update";
            let (manifest, archive_sha256, market, index) =
                install_market_plugin(source, id, &installed, update_only)
                    .map_err(|e| CoreError::new(ErrorCode::InvalidParams, e))?;
            let found = discover_plugins();
            let n = found.len();
            *ctx.plugins.write().unwrap() = found;
            Ok(serde_json::json!({
                "action": if update_only { "updated" } else { "installed" },
                "market": market,
                "index": index,
                "id": manifest.id,
                "name": manifest.name,
                "version": manifest.version,
                "previous_version": previous_version,
                "permissions": manifest.permissions,
                "approval_required": !manifest.permissions.is_empty(),
                "archive_sha256": archive_sha256,
                "reloaded": n,
            }))
        }

        "plugin.approve" => {
            let pid = str_param(params, "id")?;
            let found = discover_plugins();
            let plugin = found
                .iter()
                .find(|p| p.manifest.id == pid)
                .cloned();
            *ctx.plugins.write().unwrap() = found;
            let plugin = plugin.ok_or_else(|| {
                    CoreError::new(ErrorCode::NotFound, format!("unknown plugin: {pid}"))
                })?;
            let fingerprint = plugin_fingerprint(&plugin).ok_or_else(|| {
                CoreError::new(
                    ErrorCode::InvalidParams,
                    format!("plugin {pid} contains missing or escaped files"),
                )
            })?;
            let _settings = ctx.settings_tx.lock().unwrap();
            let mut settings = read_settings();
            let grants = settings
                .as_object_mut()
                .unwrap()
                .entry("plugin_grants")
                .or_insert_with(|| serde_json::json!({}));
            let grants = grants.as_object_mut().ok_or_else(|| {
                CoreError::new(ErrorCode::Internal, "plugin_grants setting is invalid")
            })?;
            grants.insert(
                pid.into(),
                serde_json::json!({
                    "version": plugin.manifest.version,
                    "permissions": plugin.manifest.sorted_permissions(),
                    "fingerprint": fingerprint,
                }),
            );
            write_settings(&settings).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
            Ok(serde_json::json!({
                "id": pid,
                "approved": true,
                "version": plugin.manifest.version,
                "permissions": plugin.manifest.sorted_permissions(),
                "fingerprint": fingerprint,
            }))
        }

        "plugin.revoke" => {
            let pid = str_param(params, "id")?;
            let _settings = ctx.settings_tx.lock().unwrap();
            let mut settings = read_settings();
            let removed = settings
                .get_mut("plugin_grants")
                .and_then(|v| v.as_object_mut())
                .and_then(|grants| grants.remove(pid))
                .is_some();
            write_settings(&settings).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
            Ok(serde_json::json!({"id": pid, "approved": false, "revoked": removed}))
        }

        "plugin.remove" => {
            let _tx = ctx.plugin_tx.lock().unwrap();
            let id = str_param(params, "id")?;
            remove_plugin(id).map_err(|e| CoreError::new(ErrorCode::InvalidParams, e))?;
            let _settings = ctx.settings_tx.lock().unwrap();
            let mut settings = read_settings();
            if let Some(grants) = settings
                .get_mut("plugin_grants")
                .and_then(|v| v.as_object_mut())
            {
                grants.remove(id);
                write_settings(&settings).map_err(|e| CoreError::new(ErrorCode::Internal, e))?;
            }
            let found = discover_plugins();
            let n = found.len();
            *ctx.plugins.write().unwrap() = found;
            Ok(serde_json::json!({"removed": id, "reloaded": n}))
        }

        "plugin.widget" => {
            let pid = str_param(params, "id")?;
            let plugin = approved_plugin(ctx, pid)?;
            let html = wb_plugin_host::widget_html(&plugin)
                .map_err(|e| CoreError::new(ErrorCode::Internal, format!("widget: {e}")))?;
            let w = plugin.manifest.widget.clone().unwrap();
            Ok(serde_json::json!({
                "title": w.title,
                "span": w.span.unwrap_or(2),
                "html": html,
                "network": plugin.manifest.permissions.iter().any(|p| p == "network"),
            }))
        }

        "plugin.rpc" => {
            let pid = str_param(params, "plugin")?;
            let method = str_param(params, "method")?;
            let plugin = approved_plugin(ctx, pid)?;
            let required = widget_rpc_permissions(method).ok_or_else(|| {
                CoreError::new(
                    ErrorCode::PermissionDenied,
                    format!("plugin widget method is not allowed: {method}"),
                )
            })?;
            let missing: Vec<&str> = required
                .iter()
                .copied()
                .filter(|permission| !plugin.manifest.permissions.iter().any(|p| p == permission))
                .collect();
            if !missing.is_empty() {
                return Err(CoreError::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "plugin {pid} did not declare permissions: {}",
                        missing.join(", ")
                    ),
                ));
            }
            let inner = params
                .get("params")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            call(ctx, method, &inner)
        }

        "skill.list" => {
            let settings = read_settings();
            let list: Vec<serde_json::Value> = ctx
                .plugins
                .read()
                .unwrap()
                .iter()
                .filter(|p| plugin_approved(p, &settings))
                .flat_map(|p| {
                    p.manifest.skills.iter().map(|s| {
                        serde_json::json!({
                            "plugin": p.manifest.id,
                            "plugin_name": p.manifest.name,
                            "id": s.id,
                            "name": s.name,
                            "description": s.description,
                            "tags": s.tags,
                        })
                    })
                })
                .collect();
            Ok(serde_json::Value::Array(list))
        }

        "skill.get" => {
            let pid = str_param(params, "plugin")?;
            let sid = str_param(params, "id")?;
            let plugin = approved_plugin(ctx, pid)?;
            let (skill, content) = wb_plugin_host::skill_content(&plugin, sid)
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
            let p = guard.iter().find(|p| p.manifest.id == pid).ok_or_else(|| {
                CoreError::new(ErrorCode::InvalidParams, format!("unknown plugin: {pid}"))
            })?;
            let cmd_id = params
                .get("command")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| p.manifest.commands.first().map(|c| c.id.clone()))
                .ok_or_else(|| {
                    CoreError::new(ErrorCode::InvalidParams, format!("插件 {pid} 无命令"))
                })?;
            drop(guard);
            run_plugin_command(ctx, pid, &cmd_id, args)
        }

        "audit.tail" => {
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .clamp(1, 200) as usize;
            Ok(serde_json::to_value(ctx.storage.audit_tail(limit)?)?)
        }

        "events.tail" => {
            let after = params.get("after").and_then(|v| v.as_u64()).unwrap_or(0);
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(50)
                .clamp(1, 200) as usize;
            let wait_ms = params
                .get("wait_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .min(30_000);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
            loop {
                let events = ctx.storage.audit_after(after, limit)?;
                if !events.is_empty() || std::time::Instant::now() >= deadline {
                    let cursor = events
                        .last()
                        .and_then(|event| event.get("id"))
                        .and_then(|id| id.as_u64())
                        .unwrap_or(after);
                    return Ok(serde_json::json!({
                        "events": events,
                        "cursor": cursor,
                        "timed_out": wait_ms > 0 && cursor == after,
                    }));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }

        other => Err(CoreError::new(
            ErrorCode::InvalidParams,
            format!("unknown method: {other}"),
        )),
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
                Err(CoreError::new(
                    ErrorCode::Internal,
                    "LockWorkStation spawn failed",
                ))
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
    let settings = read_settings();
    let plugin: LoadedPlugin = {
        let guard = ctx.plugins.read().unwrap();
        let found = if pid.is_empty() {
            wb_plugin_host::find_command(&guard, cmd_id).map(|(p, _)| p.clone())
        } else {
            guard
                .iter()
                .find(|p| {
                    p.manifest.id == pid && p.manifest.commands.iter().any(|c| c.id == cmd_id)
                })
                .cloned()
        };
        let plugin = found.ok_or_else(|| {
            CoreError::new(
                ErrorCode::InvalidParams,
                if pid.is_empty() {
                    format!("unknown command: {cmd_id}")
                } else {
                    format!("unknown plugin command: {pid}/{cmd_id}")
                },
            )
        })?;
        if !plugin_approved(&plugin, &settings) {
            return Err(CoreError::new(
                ErrorCode::PermissionDenied,
                format!("plugin {} requires approval", plugin.manifest.id),
            )
            .with_hint(format!("run `wb plugin approve {}`", plugin.manifest.id)));
        }
        plugin
    };
    wb_plugin_host::run_command(&plugin, cmd_id, &args)
        .map_err(|e| CoreError::new(ErrorCode::Internal, format!("plugin: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission_plugin() -> (PathBuf, LoadedPlugin) {
        let root = std::env::temp_dir().join(format!(
            "wb-daemon-permission-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.js"), "console.log('{}')").unwrap();
        std::fs::write(root.join("plugin.json"), "{}").unwrap();
        let manifest: wb_plugin_sdk::Manifest = serde_json::from_value(serde_json::json!({
            "id": "permission-test",
            "name": "Permission Test",
            "version": "1.0.0",
            "handler": "main.js",
            "commands": [{"id": "test.run", "title": "Run"}],
            "permissions": ["process"]
        }))
        .unwrap();
        (
            root.clone(),
            LoadedPlugin {
                dir: root,
                manifest,
            },
        )
    }

    #[test]
    fn validates_archive_sha256() {
        let path = std::env::temp_dir().join(format!(
            "wb-daemon-sha-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"abc").unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(verify_archive(&path, expected).unwrap(), expected);
        assert!(verify_archive(&path, &"0".repeat(64))
            .unwrap_err()
            .contains("不匹配"));
        assert_eq!(
            normalize_sha256(&format!("sha256:{}", expected.to_uppercase())).unwrap(),
            expected
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn remote_install_requires_checksum_before_network() {
        let error = install_plugin("https://plugins.invalid/example.zip", None).unwrap_err();
        assert!(error.contains("必须提供 --sha256"));
    }

    #[test]
    fn rejects_oversized_install_tree() {
        let root = std::env::temp_dir().join(format!(
            "wb-daemon-tree-limit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = std::fs::File::create(root.join("too-large.bin")).unwrap();
        file.set_len(INSTALL_FILE_MAX_BYTES + 1).unwrap();
        assert!(validate_install_tree(&root)
            .unwrap_err()
            .contains("单文件超过"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_unsafe_windows_zip_paths() {
        assert_eq!(
            safe_zip_path(Path::new("assets/widget.html")).unwrap(),
            PathBuf::from("assets/widget.html")
        );
        for path in [
            "../escape.txt",
            "assets/../../escape.txt",
            "payload:stream",
            "CON.txt",
            "assets/name.",
        ] {
            assert!(safe_zip_path(Path::new(path)).is_err(), "accepted {path}");
        }
        let deep = (0..=INSTALL_TREE_MAX_DEPTH)
            .map(|_| "d")
            .collect::<Vec<_>>()
            .join("/");
        assert!(safe_zip_path(Path::new(&deep)).is_err());
    }

    #[test]
    fn local_market_resolves_only_archives_below_its_directory() {
        let root = std::env::temp_dir().join(format!(
            "wb-daemon-market-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("hello.zip"), b"zip").unwrap();
        let index_path = root.join("index.json");
        std::fs::write(
            &index_path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "name": "Local Test",
                "plugins": [{
                    "id": "hello",
                    "name": "Hello",
                    "version": "1.1.0",
                    "download": "hello.zip",
                    "sha256": "a".repeat(64)
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let market = load_market(&index_path.to_string_lossy()).unwrap();
        let resolved = resolve_market_download(&market, &market.index.plugins[0]).unwrap();
        assert!(Path::new(&resolved).ends_with("hello.zip"));

        let remote_market = LoadedMarket {
            source: "https://plugins.example/index.json".into(),
            local_base: None,
            index: market.index.clone(),
        };
        assert!(resolve_market_download(&remote_market, &remote_market.index.plugins[0])
            .unwrap_err()
            .contains("绝对 HTTP(S) URL"));

        let outside = root.parent().unwrap().join(format!(
            "outside-{}-{}.zip",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&outside, b"zip").unwrap();
        let mut escaped = market.index.plugins[0].clone();
        escaped.download = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(resolve_market_download(&market, &escaped)
            .unwrap_err()
            .contains("越过索引目录"));
        std::fs::remove_file(outside).ok();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn market_identity_is_checked_before_install_commit() {
        let root = std::env::temp_dir().join(format!(
            "wb-daemon-market-identity-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("widget.html"), "<p>Hello</p>").unwrap();
        std::fs::write(
            root.join("plugin.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "actual-plugin",
                "name": "Actual",
                "version": "1.0.0",
                "widget": {"file": "widget.html", "title": "Actual"}
            }))
            .unwrap(),
        )
        .unwrap();
        let error = install_plugin_path(&root, Some(("expected-plugin", "1.0.0")))
            .unwrap_err();
        assert!(error.contains("身份与市场索引不匹配"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn market_sources_are_typed_and_round_trip_through_settings() {
        let mut settings = default_settings();
        let sources = vec![
            MarketSource {
                name: "WB Official".into(),
                index: "https://plugins.example/index.json".into(),
            },
            MarketSource {
                name: "Local".into(),
                index: r"E:\markets\index.json".into(),
            },
        ];
        set_market_sources(&mut settings, &sources);
        let restored = market_sources_from_settings(&settings);
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].name, "WB Official");
        assert!(same_market_source(
            &restored[0].index,
            "HTTPS://PLUGINS.EXAMPLE/INDEX.JSON"
        ));

        settings["plugin_markets"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"unexpected": true}));
        assert_eq!(market_sources_from_settings(&settings).len(), 2);
    }

    #[test]
    fn desktop_widget_ids_are_validated_and_deduplicated() {
        let widgets = normalize_desktop_widgets(&serde_json::json!([
            "w-clock",
            "plugin-stopwatch",
            "w-clock"
        ]))
        .unwrap();
        assert_eq!(widgets, ["w-clock", "plugin-stopwatch"]);

        for invalid in [
            serde_json::json!("w-clock"),
            serde_json::json!(["clock"]),
            serde_json::json!(["w-clock/path"]),
            serde_json::json!([1]),
        ] {
            assert!(normalize_desktop_widgets(&invalid).is_err());
        }
        assert!(normalize_desktop_widgets(&serde_json::json!(vec!["w-clock"; 33])).is_err());
    }

    #[test]
    fn grant_is_bound_to_plugin_content() {
        let (root, plugin) = permission_plugin();
        assert!(!plugin_approved(
            &plugin,
            &serde_json::json!({"plugin_grants": {}})
        ));

        let settings = serde_json::json!({"plugin_grants": {
            "permission-test": {
                "version": "1.0.0",
                "permissions": ["process"],
                "fingerprint": plugin_fingerprint(&plugin).unwrap(),
            }
        }});
        assert!(plugin_approved(&plugin, &settings));
        std::fs::write(root.join("main.js"), "console.log('changed')").unwrap();
        assert!(!plugin_approved(&plugin, &settings));

        std::fs::remove_file(root.join("main.js")).unwrap();
        let incomplete_grant = serde_json::json!({"plugin_grants": {
            "permission-test": {
                "version": "1.0.0",
                "permissions": ["process"]
            }
        }});
        assert!(!plugin_approved(&plugin, &incomplete_grant));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn widget_rpc_has_explicit_permission_map() {
        assert_eq!(
            widget_rpc_permissions("clip.get"),
            Some(&["clipboard.read"][..])
        );
        assert_eq!(
            widget_rpc_permissions("todo.add"),
            Some(&["data.write"][..])
        );
        assert!(widget_rpc_permissions("settings.set").is_none());
        assert!(widget_rpc_permissions("plugin.approve").is_none());
    }

    #[test]
    fn audit_parameter_summary_never_keeps_values() {
        let secret = "wb-secret-audit-value";
        let summary = audit_params(&serde_json::json!({
            "id": secret,
            "name": secret,
            "args": {"prompt": secret},
            "tags": [secret],
            "enabled": true,
            "count": 2
        }));
        let encoded = summary.to_string();
        assert!(!encoded.contains(secret));
        assert_eq!(summary["id"]["type"], "string");
        assert_eq!(summary["id"]["length"], secret.chars().count());
        assert_eq!(summary["args"]["keys"], serde_json::json!(["prompt"]));
        assert_eq!(summary["tags"]["length"], 1);
    }

    #[test]
    fn audit_actor_uses_declared_internal_origins_only() {
        assert_eq!(
            audit_actor("cmd.tool.run", &serde_json::json!({"origin":"mcp"})),
            "mcp"
        );
        assert_eq!(
            audit_actor("cmd.tool.run", &serde_json::json!({"origin":"panel-ai"})),
            "panel-ai"
        );
        assert_eq!(
            audit_actor("cmd.tool.run", &serde_json::json!({"origin":"unknown"})),
            "client"
        );
        assert_eq!(
            audit_actor("plugin.rpc", &serde_json::json!({"origin":"mcp"})),
            "widget"
        );
    }
}

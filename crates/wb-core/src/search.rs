//! Search aggregation: local stores (notes/todos/clips) + Start-menu apps.
//! Everything IPC and the USN fallback land in M1 polish; provider isolation
//! rule: every provider returns Result and a failure must never block others.

use crate::models::{ResultKind, SearchResult};
use crate::storage::Storage;

pub struct Searcher<'a> {
    pub storage: &'a Storage,
}

impl<'a> Searcher<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    /// Unified search. Provider failures degrade to empty, never to an error
    /// that kills the whole result set (DeskBox lesson: isolate sources).
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        out.extend(self.search_notes(&q).unwrap_or_default());
        out.extend(self.search_todos(&q).unwrap_or_default());
        out.extend(self.search_clips(&q).unwrap_or_default());
        out.extend(search_apps(&q).unwrap_or_default());
        out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(limit);
        out
    }

    fn search_notes(&self, q: &str) -> crate::Result<Vec<SearchResult>> {
        Ok(self
            .storage
            .note_list(200)?
            .into_iter()
            .filter(|n| n.content.to_lowercase().contains(q))
            .map(|n| SearchResult {
                kind: ResultKind::Note,
                title: n.content.chars().take(60).collect(),
                subtitle: Some(format!("note · {}", n.created_at.format("%Y-%m-%d"))),
                path: Some(format!("wb://note/{}", n.id)),
                score: 0.6,
                source: "notes".into(),
            })
            .collect())
    }

    fn search_todos(&self, q: &str) -> crate::Result<Vec<SearchResult>> {
        Ok(self
            .storage
            .todo_list(true)?
            .into_iter()
            .filter(|t| t.title.to_lowercase().contains(q))
            .map(|t| SearchResult {
                kind: ResultKind::Todo,
                title: t.title,
                subtitle: Some(if t.done { "todo · done".into() } else { "todo".into() }),
                path: Some(format!("wb://todo/{}", t.id)),
                score: 0.6,
                source: "todos".into(),
            })
            .collect())
    }

    fn search_clips(&self, q: &str) -> crate::Result<Vec<SearchResult>> {
        Ok(self
            .storage
            .clip_list(200)?
            .into_iter()
            .filter(|c| c.content.to_lowercase().contains(q))
            .map(|c| SearchResult {
                kind: ResultKind::Clip,
                title: c.content.chars().take(60).collect(),
                subtitle: Some(format!("clipboard · {:?}", c.kind)),
                path: Some(format!("wb://clip/{}", c.id)),
                score: 0.5,
                source: "clips".into(),
            })
            .collect())
    }
}

/// Scan Start Menu shortcuts (.lnk) — pure filesystem, no shell deps.
fn search_apps(q: &str) -> std::io::Result<Vec<SearchResult>> {
    Ok(list_apps()
        .into_iter()
        .filter(|a| a.title.to_lowercase().contains(q))
        .take(50)
        .collect())
}

/// All Start Menu apps, unfiltered (panel app-grid uses this).
/// 5 分钟进程内缓存：daemon 的 search 每次查询也走这里，不能每次都重扫。
pub fn list_apps() -> Vec<SearchResult> {
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, Vec<SearchResult>)>> =
        std::sync::Mutex::new(None);
    if let Some((at, apps)) = &*CACHE.lock().unwrap() {
        if at.elapsed() < std::time::Duration::from_secs(300) {
            return apps.clone();
        }
    }
    let apps = list_apps_fresh();
    *CACHE.lock().unwrap() = Some((std::time::Instant::now(), apps.clone()));
    apps
}

fn list_apps_fresh() -> Vec<SearchResult> {
    let mut roots = Vec::new();
    if let Some(p) = std::env::var_os("APPDATA") {
        roots.push(std::path::PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(p) = std::env::var_os("ProgramData") {
        roots.push(std::path::PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    let mut out = Vec::new();
    for root in roots {
        visit(&root, &mut out, 0);
    }
    // UWP/Store 应用没有 .lnk，必须用 Get-StartApps 补齐（桌面应用也会列出来，按名去重）
    let mut seen: std::collections::HashSet<String> =
        out.iter().map(|a| a.title.to_lowercase()).collect();
    for app in list_start_apps_uwp() {
        if seen.insert(app.title.to_lowercase()) {
            out.push(app);
        }
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// Get-StartApps 覆盖开始菜单里的一切（含 UWP）：返回 Name + AppID，
/// 用 shell:AppsFolder\<AppID> 作为可启动、可提取图标的 shell 解析名。
#[cfg(windows)]
fn list_start_apps_uwp() -> Vec<SearchResult> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Git Bash 等环境下 PATH 可能不含 powershell，用 SYSTEMROOT 拼全路径兜底
    let ps = std::env::var_os("SYSTEMROOT")
        .map(|r| {
            std::path::PathBuf::from(r)
                .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
        })
        .filter(|p| p.exists())
        .unwrap_or_else(|| "powershell.exe".into());
    let out = std::process::Command::new(ps)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Get-StartApps | Select-Object Name,AppID | ConvertTo-Json -Compress",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let Ok(out) = out else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return Vec::new();
    };
    // 只有 1 个结果时 ConvertTo-Json 输出对象而非数组
    let items: Vec<&serde_json::Value> = match &v {
        serde_json::Value::Array(a) => a.iter().collect(),
        serde_json::Value::Object(_) => vec![&v],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|it| {
            let name = it.get("Name")?.as_str()?.trim().to_string();
            let appid = it.get("AppID")?.as_str()?.trim().to_string();
            if name.is_empty() || appid.is_empty() {
                return None;
            }
            Some(SearchResult {
                kind: ResultKind::App,
                title: name,
                subtitle: Some("app".into()),
                path: Some(format!("shell:AppsFolder\\{appid}")),
                score: 0.8,
                source: "start-apps".into(),
            })
        })
        .collect()
}

#[cfg(not(windows))]
fn list_start_apps_uwp() -> Vec<SearchResult> {
    Vec::new()
}

/// 最近使用的文件（%APPDATA%\Microsoft\Windows\Recent 的 .lnk，按修改时间倒序）。
pub fn list_recent_files(limit: usize) -> Vec<SearchResult> {
    let Some(appdata) = std::env::var_os("APPDATA") else { return Vec::new() };
    let dir = std::path::PathBuf::from(appdata).join(r"Microsoft\Windows\Recent");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut items: Vec<(std::time::SystemTime, SearchResult)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "lnk") {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            if stem.is_empty() {
                return None;
            }
            Some((mtime, SearchResult {
                kind: ResultKind::File,
                title: stem,
                subtitle: Some("最近使用".into()),
                path: Some(p.to_string_lossy().to_string()),
                score: 0.7,
                source: "recent".into(),
            }))
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.into_iter().take(limit).map(|(_, r)| r).collect()
}

fn visit(dir: &std::path::Path, out: &mut Vec<SearchResult>, depth: u32) {
    if depth > 3 || out.len() >= 500 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            visit(&p, out, depth + 1);
        } else if p.extension().is_some_and(|x| x == "lnk") {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            out.push(SearchResult {
                kind: ResultKind::App,
                title: stem,
                subtitle: Some("app".into()),
                path: Some(p.to_string_lossy().to_string()),
                score: 0.8,
                source: "start-menu".into(),
            });
        }
    }
}

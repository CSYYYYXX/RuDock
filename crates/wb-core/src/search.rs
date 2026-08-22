//! Search aggregation primitives for local stores, apps, and the bounded file fallback.
//! The daemon adds Everything IPC as its primary file provider; provider isolation rule:
//! a source failure must never block results from the others.

use crate::models::{ResultKind, SearchResult};
use crate::storage::Storage;

pub struct Searcher<'a> {
    pub storage: &'a Storage,
    apps: &'a [SearchResult],
}

impl<'a> Searcher<'a> {
    pub fn new(storage: &'a Storage, apps: &'a [SearchResult]) -> Self {
        Self { storage, apps }
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
        out.extend(search_indexed_apps(self.apps, &q, 50));
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
                subtitle: Some(format!("随手记 · {}", n.created_at.format("%Y-%m-%d"))),
                preview: Some(n.content.chars().take(4_000).collect()),
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
                subtitle: Some(if t.done { "待办 · 已完成".into() } else { "待办".into() }),
                preview: None,
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
            .map(|c| {
                let kind = match c.kind {
                    crate::models::ClipKind::Text => "文本",
                    crate::models::ClipKind::Image => "图片",
                    crate::models::ClipKind::Files => "文件",
                };
                SearchResult {
                    kind: ResultKind::Clip,
                    title: c.content.chars().take(60).collect(),
                    subtitle: Some(format!("剪贴板 · {kind}")),
                    preview: Some(c.content.chars().take(4_000).collect()),
                    path: Some(format!("wb://clip/{}", c.id)),
                    score: 0.5,
                    source: "clips".into(),
                }
            })
            .collect())
    }
}

/// Search a daemon-owned application snapshot without touching the filesystem
/// or spawning PowerShell on the request path.
pub fn search_indexed_apps(
    apps: &[SearchResult],
    query: &str,
    limit: usize,
) -> Vec<SearchResult> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    apps
        .iter()
        .filter(|a| a.title.to_lowercase().contains(&q))
        .take(limit)
        .cloned()
        .collect()
}

/// Build the complete Start Menu application snapshot. The daemon calls this
/// during startup and on its refresh thread, never from an interactive request.
pub fn index_apps() -> Vec<SearchResult> {
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
                subtitle: Some("应用".into()),
                preview: None,
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
                preview: None,
                path: Some(p.to_string_lossy().to_string()),
                score: 0.7,
                source: "recent".into(),
            }))
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.into_iter().take(limit).map(|(_, r)| r).collect()
}

/// Build a bounded index of user-visible files without blocking search queries.
/// The daemon runs this in a background thread and atomically swaps the result.
pub fn index_user_files(limit: usize) -> Vec<SearchResult> {
    let Some(profile) = std::env::var_os("USERPROFILE") else { return Vec::new() };
    let profile = std::path::PathBuf::from(profile);
    let mut roots = vec![profile.join("Desktop"), profile.join("Documents"), profile.join("Downloads")];
    if let Some(one_drive) = std::env::var_os("OneDrive") {
        roots.push(std::path::PathBuf::from(one_drive));
    }
    index_files_from_roots(&roots, limit)
}

pub fn index_files_from_roots(roots: &[std::path::PathBuf], limit: usize) -> Vec<SearchResult> {
    let mut out = Vec::new();
    let mut queue: std::collections::VecDeque<(std::path::PathBuf, u8)> =
        roots.iter().filter(|p| p.is_dir()).cloned().map(|p| (p, 0)).collect();
    while let Some((dir, depth)) = queue.pop_front() {
        if out.len() >= limit { break; }
        if depth > 12 { continue; }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if out.len() >= limit { break; }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() { continue; }
            let path = entry.path();
            if kind.is_dir() {
                queue.push_back((path, depth + 1));
                continue;
            }
            if !kind.is_file() { continue; }
            let title = entry.file_name().to_string_lossy().to_string();
            if title.is_empty() || title.starts_with('~') { continue; }
            out.push(SearchResult {
                kind: ResultKind::File,
                title,
                subtitle: path.parent().map(|p| p.to_string_lossy().to_string()),
                preview: None,
                path: Some(path.to_string_lossy().to_string()),
                score: 0.72,
                source: "files".into(),
            });
        }
    }
    out
}

pub fn search_indexed_files(files: &[SearchResult], query: &str, limit: usize) -> Vec<SearchResult> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<SearchResult> = files
        .iter()
        .filter_map(|item| {
            let title = item.title.to_lowercase();
            let path_hit = item.path.as_deref().is_some_and(|p| p.to_lowercase().contains(&q));
            if !title.contains(&q) && !path_hit {
                return None;
            }
            let mut item = item.clone();
            if title.starts_with(&q) {
                item.score += 0.12;
            }
            Some(item)
        })
        .collect();
    matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    matches.truncate(limit);
    matches
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
                subtitle: Some("应用".into()),
                preview: None,
                path: Some(p.to_string_lossy().to_string()),
                score: 0.8,
                source: "start-menu".into(),
            });
        }
    }
}

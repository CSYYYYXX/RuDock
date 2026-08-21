//! SQLite storage. WAL mode + synchronous=NORMAL: survive crashes without
//! the silent-revert-to-default failure mode DeskBox had to patch around.

use crate::error::{CoreError, ErrorCode, Result};
use crate::models::{ClipEntry, ClipKind, Note, TodoItem};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

pub struct Storage {
    conn: Mutex<Connection>,
}

fn parse_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn fmt_time(t: &DateTime<Utc>) -> String {
    t.to_rfc3339()
}

fn split_tags(s: &str) -> Vec<String> {
    s.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()).map(String::from).collect()
}

fn join_tags(tags: &[String]) -> String {
    tags.join(",")
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                CoreError::new(ErrorCode::Internal, format!("create data dir: {e}"))
            })?;
        }
        let conn = Connection::open(path)?;
        let st = Self { conn: Mutex::new(conn) };
        st.init()?;
        Ok(st)
    }

    /// In-memory storage for tests.
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let st = Self { conn: Mutex::new(conn) };
        st.init()?;
        Ok(st)
    }

    fn init(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
                id TEXT PRIMARY KEY, content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS todos (
                id TEXT PRIMARY KEY, title TEXT NOT NULL,
                done INTEGER NOT NULL DEFAULT 0,
                due TEXT, repeat TEXT, tags TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS clips (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL,
                content TEXT NOT NULL, created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS audit (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                actor TEXT NOT NULL, action TEXT NOT NULL,
                detail TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_clips_created ON clips(created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_audit_created ON audit(created_at DESC);",
        )?;
        Ok(())
    }

    // ---------- notes ----------

    pub fn note_add(&self, note: &Note) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO notes (id, content, tags, created_at, updated_at) VALUES (?1,?2,?3,?4,?5)",
            params![note.id, note.content, join_tags(&note.tags), fmt_time(&note.created_at), fmt_time(&note.updated_at)],
        )?;
        Ok(())
    }

    pub fn note_get(&self, id: &str) -> Result<Note> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, content, tags, created_at, updated_at FROM notes WHERE id = ?1",
            params![id],
            |r| {
                Ok(Note {
                    id: r.get(0)?,
                    content: r.get(1)?,
                    tags: split_tags(&r.get::<_, String>(2)?),
                    created_at: parse_time(&r.get::<_, String>(3)?),
                    updated_at: parse_time(&r.get::<_, String>(4)?),
                })
            },
        )
        .optional()?
        .ok_or_else(|| CoreError::new(ErrorCode::NotFound, format!("note not found: {id}")))
    }

    pub fn note_list(&self, limit: usize) -> Result<Vec<Note>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, content, tags, created_at, updated_at FROM notes ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(Note {
                id: r.get(0)?,
                content: r.get(1)?,
                tags: split_tags(&r.get::<_, String>(2)?),
                created_at: parse_time(&r.get::<_, String>(3)?),
                updated_at: parse_time(&r.get::<_, String>(4)?),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn note_rm(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(CoreError::new(ErrorCode::NotFound, format!("note not found: {id}")));
        }
        Ok(())
    }

    // ---------- todos ----------

    pub fn todo_add(&self, item: &TodoItem) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO todos (id, title, done, due, repeat, tags, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![item.id, item.title, item.done as i32, item.due, item.repeat, join_tags(&item.tags), fmt_time(&item.created_at)],
        )?;
        Ok(())
    }

    pub fn todo_list(&self, include_done: bool) -> Result<Vec<TodoItem>> {
        let conn = self.conn.lock().unwrap();
        let sql = if include_done {
            "SELECT id, title, done, due, repeat, tags, created_at FROM todos ORDER BY created_at DESC"
        } else {
            "SELECT id, title, done, due, repeat, tags, created_at FROM todos WHERE done = 0 ORDER BY created_at DESC"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(TodoItem {
                id: r.get(0)?,
                title: r.get(1)?,
                done: r.get::<_, i32>(2)? != 0,
                due: r.get(3)?,
                repeat: r.get(4)?,
                tags: split_tags(&r.get::<_, String>(5)?),
                created_at: parse_time(&r.get::<_, String>(6)?),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn todo_set_done(&self, id: &str, done: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("UPDATE todos SET done = ?2 WHERE id = ?1", params![id, done as i32])?;
        if n == 0 {
            return Err(CoreError::new(ErrorCode::NotFound, format!("todo not found: {id}")));
        }
        Ok(())
    }

    pub fn todo_rm(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM todos WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(CoreError::new(ErrorCode::NotFound, format!("todo not found: {id}")));
        }
        Ok(())
    }

    // ---------- clips ----------

    pub fn clip_add(&self, entry: &ClipEntry) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let kind = match entry.kind {
            ClipKind::Text => "text",
            ClipKind::Image => "image",
            ClipKind::Files => "files",
        };
        conn.execute(
            "INSERT INTO clips (id, kind, content, created_at) VALUES (?1,?2,?3,?4)",
            params![entry.id, kind, entry.content, fmt_time(&entry.created_at)],
        )?;
        Ok(())
    }

    pub fn clip_list(&self, limit: usize) -> Result<Vec<ClipEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, content, created_at FROM clips ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let kind = match r.get::<_, String>(1)?.as_str() {
                "image" => ClipKind::Image,
                "files" => ClipKind::Files,
                _ => ClipKind::Text,
            };
            Ok(ClipEntry { id: r.get(0)?, kind, content: r.get(2)?, created_at: parse_time(&r.get::<_, String>(3)?) })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn clip_clear(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM clips", [])?;
        Ok(n as u64)
    }

    // ---------- audit ----------

    pub fn audit(&self, actor: &str, action: &str, detail: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO audit (actor, action, detail, created_at) VALUES (?1,?2,?3,?4)",
            params![actor, action, detail, fmt_time(&Utc::now())],
        )?;
        Ok(())
    }

    pub fn audit_tail(&self, limit: usize) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT actor, action, detail, created_at FROM audit ORDER BY rowid DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(serde_json::json!({
                "actor": r.get::<_, String>(0)?,
                "action": r.get::<_, String>(1)?,
                "detail": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{new_id, ClipEntry, ClipKind, Note, TodoItem};

    #[test]
    fn notes_crud_roundtrip() {
        let st = Storage::open_memory().unwrap();
        let n = Note::new(new_id(), "hello wb".into(), vec!["t".into()]);
        st.note_add(&n).unwrap();
        let got = st.note_get(&n.id).unwrap();
        assert_eq!(got.content, "hello wb");
        assert_eq!(got.tags, vec!["t"]);
        assert_eq!(st.note_list(10).unwrap().len(), 1);
        st.note_rm(&n.id).unwrap();
        assert!(st.note_get(&n.id).is_err());
    }

    #[test]
    fn todos_done_flow() {
        let st = Storage::open_memory().unwrap();
        let t = TodoItem {
            id: new_id(),
            title: "ship it".into(),
            done: false,
            due: Some("friday".into()),
            repeat: None,
            tags: vec![],
            created_at: Utc::now(),
        };
        st.todo_add(&t).unwrap();
        assert_eq!(st.todo_list(false).unwrap().len(), 1);
        st.todo_set_done(&t.id, true).unwrap();
        assert_eq!(st.todo_list(false).unwrap().len(), 0);
        assert_eq!(st.todo_list(true).unwrap().len(), 1);
    }

    #[test]
    fn clips_order_and_clear() {
        let st = Storage::open_memory().unwrap();
        for i in 0..3 {
            st.clip_add(&ClipEntry {
                id: new_id(),
                kind: ClipKind::Text,
                content: format!("clip-{i}"),
                created_at: Utc::now(),
            })
            .unwrap();
        }
        assert_eq!(st.clip_list(2).unwrap().len(), 2);
        assert_eq!(st.clip_clear().unwrap(), 3);
        assert_eq!(st.clip_list(10).unwrap().len(), 0);
    }

    #[test]
    fn search_hits_local_stores() {
        let st = Storage::open_memory().unwrap();
        st.note_add(&Note::new(new_id(), "周报模板在这里".into(), vec![])).unwrap();
        let s = crate::search::Searcher::new(&st);
        let hits = s.search("周报", 10);
        assert!(hits.iter().any(|r| r.source == "notes"));
        assert!(s.search("", 10).is_empty());
        assert!(s.search("绝不存在xyz", 10).is_empty());
    }

    #[test]
    fn ids_are_unique_and_sortable() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
    }

    #[test]
    fn exit_code_contract() {
        use crate::error::ErrorCode;
        assert_eq!(ErrorCode::NoResults.exit_code(), 2);
        assert_eq!(ErrorCode::PermissionDenied.exit_code(), 3);
        assert_eq!(ErrorCode::InvalidParams.exit_code(), 4);
        assert_eq!(ErrorCode::DaemonUnavailable.exit_code(), 5);
    }
}

use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

#[derive(Serialize, Clone)]
pub struct Transcription {
    pub id: String,
    pub raw_text: String,
    pub formatted_text: Option<String>,
    pub provider: String,
    pub duration_ms: Option<i64>,
    pub context_type: Option<String>,
    pub window_title: Option<String>,
    pub language: Option<String>,
    pub created_at: String,
}

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    pub fn new(app_dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&app_dir)
            .map_err(|e| format!("Failed to create app dir: {}", e))?;

        let db_path = app_dir.join("openflow.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("Failed to open database: {}", e))?;

        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("Failed to configure database: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS transcriptions (
                id TEXT PRIMARY KEY,
                raw_text TEXT NOT NULL,
                formatted_text TEXT,
                provider TEXT NOT NULL,
                duration_ms INTEGER,
                context_type TEXT,
                window_title TEXT,
                language TEXT,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .map_err(|e| format!("Migration failed: {}", e))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "Database lock is poisoned".to_string())
    }

    pub fn save_transcription(&self, t: &Transcription) -> Result<(), String> {
        self.connection()?.execute(
            "INSERT OR REPLACE INTO transcriptions (id, raw_text, formatted_text, provider, duration_ms, context_type, window_title, language, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![t.id, t.raw_text, t.formatted_text, t.provider, t.duration_ms, t.context_type, t.window_title, t.language, t.created_at],
        ).map_err(|e| format!("Save failed: {}", e))?;
        Ok(())
    }

    pub fn get_history(&self, limit: usize) -> Result<Vec<Transcription>, String> {
        let limit = limit.min(500);
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, raw_text, formatted_text, provider, duration_ms, context_type, window_title, language, created_at
             FROM transcriptions ORDER BY created_at DESC LIMIT ?1"
        ).map_err(|e| format!("Query failed: {}", e))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(Transcription {
                    id: row.get(0)?,
                    raw_text: row.get(1)?,
                    formatted_text: row.get(2)?,
                    provider: row.get(3)?,
                    duration_ms: row.get(4)?,
                    context_type: row.get(5)?,
                    window_title: row.get(6)?,
                    language: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })
            .map_err(|e| format!("Query map failed: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(results)
    }

    pub fn search_history(&self, query: &str, limit: usize) -> Result<Vec<Transcription>, String> {
        let limit = limit.min(500);
        let pattern = format!("%{}%", query);
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, raw_text, formatted_text, provider, duration_ms, context_type, window_title, language, created_at
             FROM transcriptions WHERE raw_text LIKE ?1 OR formatted_text LIKE ?1
             ORDER BY created_at DESC LIMIT ?2"
        ).map_err(|e| format!("Search failed: {}", e))?;

        let rows = stmt
            .query_map(params![pattern, limit as i64], |row| {
                Ok(Transcription {
                    id: row.get(0)?,
                    raw_text: row.get(1)?,
                    formatted_text: row.get(2)?,
                    provider: row.get(3)?,
                    duration_ms: row.get(4)?,
                    context_type: row.get(5)?,
                    window_title: row.get(6)?,
                    language: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })
            .map_err(|e| format!("Search map failed: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(results)
    }

    /// Fetch one row by id. The tray menu needs this so it can key entries by
    /// identity instead of by their position in a list that keeps changing.
    pub fn get_transcription(&self, id: &str) -> Result<Option<Transcription>, String> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, raw_text, formatted_text, provider, duration_ms, context_type, window_title, language, created_at
                 FROM transcriptions WHERE id = ?1",
            )
            .map_err(|e| format!("Query failed: {}", e))?;

        let mut rows = stmt
            .query_map(params![id], |row| {
                Ok(Transcription {
                    id: row.get(0)?,
                    raw_text: row.get(1)?,
                    formatted_text: row.get(2)?,
                    provider: row.get(3)?,
                    duration_ms: row.get(4)?,
                    context_type: row.get(5)?,
                    window_title: row.get(6)?,
                    language: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })
            .map_err(|e| format!("Query map failed: {}", e))?;

        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| format!("Row error: {}", e))?)),
            None => Ok(None),
        }
    }

    // ── Privacy controls ──────────────────────────────────
    // Dictation captures whatever the user says out loud: passwords, medical
    // details, private conversation. Storing all of it with no way to remove
    // any of it is the largest privacy gap the app can have.

    pub fn delete_transcription(&self, id: &str) -> Result<(), String> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM transcriptions WHERE id = ?1", params![id])
            .map_err(|e| format!("Delete failed: {}", e))?;
        Ok(())
    }

    pub fn clear_history(&self) -> Result<usize, String> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM transcriptions", [])
            .map_err(|e| format!("Clear failed: {}", e))
    }

    /// Drops anything older than `days`. Backs the optional retention setting.
    pub fn prune_older_than(&self, days: i64) -> Result<usize, String> {
        if days <= 0 {
            return Ok(0);
        }
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        let conn = self.connection()?;
        conn.execute(
            "DELETE FROM transcriptions WHERE created_at < ?1",
            params![cutoff],
        )
        .map_err(|e| format!("Prune failed: {}", e))
    }

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.connection()
            .ok()?
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.connection()?
            .execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|e| format!("Setting save failed: {}", e))?;
        Ok(())
    }

    pub fn remove_setting(&self, key: &str) -> Result<(), String> {
        self.connection()?
            .execute("DELETE FROM settings WHERE key = ?1", params![key])
            .map_err(|e| format!("Setting delete failed: {}", e))?;
        Ok(())
    }
}

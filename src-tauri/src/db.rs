use rusqlite::{Connection, params};
use serde::Serialize;
use std::path::PathBuf;

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
    conn: Connection,
}

impl Database {
    pub fn new(app_dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&app_dir)
            .map_err(|e| format!("Failed to create app dir: {}", e))?;

        let db_path = app_dir.join("openflow.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open database: {}", e))?;

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
            );"
        ).map_err(|e| format!("Migration failed: {}", e))?;

        Ok(Self { conn })
    }

    pub fn save_transcription(&self, t: &Transcription) -> Result<(), String> {
        self.conn.execute(
            "INSERT OR REPLACE INTO transcriptions (id, raw_text, formatted_text, provider, duration_ms, context_type, window_title, language, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![t.id, t.raw_text, t.formatted_text, t.provider, t.duration_ms, t.context_type, t.window_title, t.language, t.created_at],
        ).map_err(|e| format!("Save failed: {}", e))?;
        Ok(())
    }

    pub fn get_history(&self, limit: usize) -> Result<Vec<Transcription>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT id, raw_text, formatted_text, provider, duration_ms, context_type, window_title, language, created_at
             FROM transcriptions ORDER BY created_at DESC LIMIT ?1"
        ).map_err(|e| format!("Query failed: {}", e))?;

        let rows = stmt.query_map(params![limit as i64], |row| {
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
        }).map_err(|e| format!("Query map failed: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(results)
    }

    pub fn search_history(&self, query: &str, limit: usize) -> Result<Vec<Transcription>, String> {
        let pattern = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, raw_text, formatted_text, provider, duration_ms, context_type, window_title, language, created_at
             FROM transcriptions WHERE raw_text LIKE ?1 OR formatted_text LIKE ?1
             ORDER BY created_at DESC LIMIT ?2"
        ).map_err(|e| format!("Search failed: {}", e))?;

        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
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
        }).map_err(|e| format!("Search map failed: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(results)
    }

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        ).ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        ).map_err(|e| format!("Setting save failed: {}", e))?;
        Ok(())
    }
}

unsafe impl Send for Database {}
unsafe impl Sync for Database {}

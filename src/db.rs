//! SQLite storage for the video library.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// A video known to the library.
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    pub id: i64,
    pub library_root: String,
    pub rel_path: String,
    pub title: String,
    /// Relative parent folder with `/` separators; empty for files in the root.
    pub folder: String,
    pub size: i64,
    pub mtime: i64,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
    pub quest_playable: Option<bool>,
}

impl Video {
    /// Absolute path on disk. Only ever derived from a database row, never from request input.
    pub fn abs_path(&self) -> PathBuf {
        Path::new(&self.library_root).join(&self.rel_path)
    }
}

/// Size/mtime of an already-stored row, used to decide whether to re-read metadata.
#[derive(Debug, Clone, Copy)]
pub struct Existing {
    pub size: i64,
    pub mtime: i64,
}

pub struct Db {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS videos (
    id INTEGER PRIMARY KEY,
    library_root TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    title TEXT NOT NULL,
    folder TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    duration REAL,
    width INTEGER,
    height INTEGER,
    codec TEXT,
    quest_playable INTEGER,
    missing INTEGER NOT NULL DEFAULT 0,
    UNIQUE (library_root, rel_path)
);";

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback(&self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }

    pub fn mark_all_missing(&self) -> Result<()> {
        self.conn.execute("UPDATE videos SET missing = 1", [])?;
        Ok(())
    }

    pub fn find(&self, root: &str, rel: &str) -> Result<Option<Existing>> {
        Ok(self
            .conn
            .query_row(
                "SELECT size, mtime FROM videos WHERE library_root = ?1 AND rel_path = ?2",
                params![root, rel],
                |r| {
                    Ok(Existing {
                        size: r.get(0)?,
                        mtime: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Insert or update a row (keeping its id) and clear the missing flag.
    pub fn upsert(&self, v: &Video) -> Result<()> {
        self.conn.execute(
            "INSERT INTO videos (library_root, rel_path, title, folder, size, mtime, duration,
                                 width, height, codec, quest_playable, missing)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)
             ON CONFLICT (library_root, rel_path) DO UPDATE SET
                title = excluded.title, folder = excluded.folder, size = excluded.size,
                mtime = excluded.mtime, duration = excluded.duration, width = excluded.width,
                height = excluded.height, codec = excluded.codec,
                quest_playable = excluded.quest_playable, missing = 0",
            params![
                v.library_root,
                v.rel_path,
                v.title,
                v.folder,
                v.size,
                v.mtime,
                v.duration,
                v.width,
                v.height,
                v.codec,
                v.quest_playable
            ],
        )?;
        Ok(())
    }

    /// Mark one row as present without touching its metadata.
    pub fn touch_present(&self, root: &str, rel: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE videos SET missing = 0 WHERE library_root = ?1 AND rel_path = ?2",
            params![root, rel],
        )?;
        Ok(())
    }

    /// All non-missing videos ordered by id.
    pub fn list_present(&self) -> Result<Vec<Video>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, library_root, rel_path, title, folder, size, mtime, duration, width,
                    height, codec, quest_playable
             FROM videos WHERE missing = 0 ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Video {
                id: r.get(0)?,
                library_root: r.get(1)?,
                rel_path: r.get(2)?,
                title: r.get(3)?,
                folder: r.get(4)?,
                size: r.get(5)?,
                mtime: r.get(6)?,
                duration: r.get(7)?,
                width: r.get(8)?,
                height: r.get(9)?,
                codec: r.get(10)?,
                quest_playable: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Count of all rows including missing ones.
    pub fn total_rows(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM videos", [], |r| r.get(0))?)
    }
}

//! Style memory (opt-in): recent messages the user sent, per app, kept in
//! SQLite on this device so suggestions can match their voice. The engine
//! decides what to remember and has already run the secret filter on it.

use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;

/// Messages kept per app; older ones are deleted.
const KEPT_PER_APP: u32 = 50;
/// Samples handed to the engine on focus, most recent.
const SAMPLES: u32 = 8;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS sent (
    id INTEGER PRIMARY KEY,
    app TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    text TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS sent_app ON sent (app, id);
";

type Reply = Box<dyn FnOnce(Vec<String>) + Send>;

enum Msg {
    Remember {
        app: String,
        text: String,
        at_ms: u64,
    },
    Load {
        app: String,
        reply: Reply,
    },
    Delete {
        id: Option<i64>,
        app: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct StyleMemory {
    tx: Sender<Msg>,
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sample {
    pub id: i64,
    pub app: String,
    pub at_ms: u64,
    pub text: String,
}

impl StyleMemory {
    pub fn open(data_dir: &Path) -> rusqlite::Result<StyleMemory> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let path = data_dir.join("style.sqlite");
        let conn = Connection::open(&path)?;
        conn.execute_batch(SCHEMA)?;
        let (tx, rx) = unbounded();
        thread::Builder::new()
            .name("oft-style".into())
            .spawn(move || worker(conn, rx))
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        Ok(StyleMemory { tx, path })
    }

    pub fn remember(&self, app: String, text: String, at_ms: u64) {
        let _ = self.tx.send(Msg::Remember { app, text, at_ms });
    }

    /// Calls `reply` on the style thread with the app's recent samples,
    /// oldest first.
    pub fn load(&self, app: String, reply: impl FnOnce(Vec<String>) + Send + 'static) {
        let _ = self.tx.send(Msg::Load {
            app,
            reply: Box::new(reply),
        });
    }

    /// Deletes one sample, every sample of one app, or everything.
    pub fn delete(&self, id: Option<i64>, app: Option<String>) {
        let _ = self.tx.send(Msg::Delete { id, app });
    }

    /// Everything stored, newest first, for the privacy page.
    pub fn all(&self) -> rusqlite::Result<Vec<Sample>> {
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = conn.prepare("SELECT id, app, at_ms, text FROM sent ORDER BY id DESC")?;
        stmt.query_map([], |r| {
            Ok(Sample {
                id: r.get(0)?,
                app: r.get(1)?,
                at_ms: r.get(2)?,
                text: r.get(3)?,
            })
        })?
        .collect()
    }
}

fn worker(conn: Connection, rx: Receiver<Msg>) {
    for msg in rx {
        if let Err(e) = apply(&conn, msg) {
            log::warn!("style memory failed: {e}");
        }
    }
}

fn apply(conn: &Connection, msg: Msg) -> rusqlite::Result<()> {
    match msg {
        Msg::Remember { app, text, at_ms } => {
            conn.prepare_cached("INSERT INTO sent (app, at_ms, text) VALUES (?1, ?2, ?3)")?
                .execute(params![app, at_ms, text])?;
            conn.prepare_cached(
                "DELETE FROM sent WHERE app = ?1 AND id NOT IN
                 (SELECT id FROM sent WHERE app = ?1 ORDER BY id DESC LIMIT ?2)",
            )?
            .execute(params![app, KEPT_PER_APP])?;
        }
        Msg::Load { app, reply } => {
            let samples = recent(conn, &app)?;
            reply(samples);
        }
        Msg::Delete { id: Some(id), .. } => {
            conn.prepare_cached("DELETE FROM sent WHERE id = ?1")?
                .execute([id])?;
        }
        Msg::Delete {
            id: None,
            app: Some(app),
        } => {
            conn.prepare_cached("DELETE FROM sent WHERE app = ?1")?
                .execute([app])?;
        }
        Msg::Delete {
            id: None,
            app: None,
        } => conn.execute_batch("DELETE FROM sent; VACUUM;")?,
    }
    Ok(())
}

fn recent(conn: &Connection, app: &str) -> rusqlite::Result<Vec<String>> {
    let mut samples: Vec<String> = conn
        .prepare_cached("SELECT text FROM sent WHERE app = ?1 ORDER BY id DESC LIMIT ?2")?
        .query_map(params![app, SAMPLES], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    samples.reverse();
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    fn remember(conn: &Connection, app: &str, text: &str) {
        apply(
            conn,
            Msg::Remember {
                app: app.into(),
                text: text.into(),
                at_ms: 1,
            },
        )
        .unwrap();
    }

    #[test]
    fn recent_samples_are_oldest_first_and_per_app() {
        let conn = memory();
        for i in 0..12 {
            remember(&conn, "chat", &format!("m{i}"));
        }
        remember(&conn, "mail", "other");
        let samples = recent(&conn, "chat").unwrap();
        assert_eq!(
            samples,
            (4..12).map(|i| format!("m{i}")).collect::<Vec<_>>()
        );
        assert_eq!(recent(&conn, "mail").unwrap(), vec!["other"]);
    }

    #[test]
    fn keeps_a_bounded_number_per_app() {
        let conn = memory();
        for i in 0..(KEPT_PER_APP + 10) {
            remember(&conn, "chat", &i.to_string());
        }
        let n: u32 = conn
            .query_row("SELECT COUNT(*) FROM sent", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, KEPT_PER_APP);
    }

    #[test]
    fn deletes_by_id_app_and_all() {
        let conn = memory();
        remember(&conn, "a", "1");
        remember(&conn, "a", "2");
        remember(&conn, "b", "3");
        apply(
            &conn,
            Msg::Delete {
                id: Some(1),
                app: None,
            },
        )
        .unwrap();
        assert_eq!(recent(&conn, "a").unwrap(), vec!["2"]);
        apply(
            &conn,
            Msg::Delete {
                id: None,
                app: Some("a".into()),
            },
        )
        .unwrap();
        assert!(recent(&conn, "a").unwrap().is_empty());
        apply(
            &conn,
            Msg::Delete {
                id: None,
                app: None,
            },
        )
        .unwrap();
        assert!(recent(&conn, "b").unwrap().is_empty());
    }
}

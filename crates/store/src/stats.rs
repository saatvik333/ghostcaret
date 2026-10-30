//! Usage stats in SQLite: daily counters per app, and the apps seen so far.
//! Nothing here ever holds typed text.

use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use ghostcaret_engine::controller::Stat;
use ghostcaret_engine::io::AppInfo;
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS daily (
    day TEXT NOT NULL,
    app TEXT NOT NULL,
    shown INTEGER NOT NULL DEFAULT 0,
    accepted INTEGER NOT NULL DEFAULT 0,
    accepted_whole INTEGER NOT NULL DEFAULT 0,
    dismissed INTEGER NOT NULL DEFAULT 0,
    chars INTEGER NOT NULL DEFAULT 0,
    keystrokes_saved INTEGER NOT NULL DEFAULT 0,
    drafts INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, app)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS apps (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    last_seen TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS cloud_requests (
    day TEXT PRIMARY KEY NOT NULL,
    count INTEGER NOT NULL
) WITHOUT ROWID;
";

enum Msg {
    Record(Stat),
    Seen(AppInfo),
    CloudRequest,
    Clear,
}

/// Handle to the stats writer thread. Writes never block the caller.
#[derive(Clone, Debug)]
pub struct Stats {
    tx: Sender<Msg>,
    path: PathBuf,
}

impl Stats {
    pub fn open(data_dir: &Path) -> rusqlite::Result<Stats> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let path = data_dir.join("stats.sqlite");
        let conn = Connection::open(&path)?;
        conn.execute_batch(SCHEMA)?;
        let (tx, rx) = unbounded();
        thread::Builder::new()
            .name("ghostcaret-stats".into())
            .spawn(move || writer(conn, rx))
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        Ok(Stats { tx, path })
    }

    pub fn record(&self, stat: Stat) {
        let _ = self.tx.send(Msg::Record(stat));
    }

    pub fn seen(&self, app: AppInfo) {
        let _ = self.tx.send(Msg::Seen(app));
    }

    pub fn clear(&self) {
        let _ = self.tx.send(Msg::Clear);
    }

    /// Counts one request sent to a cloud provider today, for the daily cap.
    pub fn cloud_request(&self) {
        let _ = self.tx.send(Msg::CloudRequest);
    }

    /// Cloud requests sent so far today (local time).
    pub fn cloud_requests_today(&self) -> rusqlite::Result<u32> {
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        query_cloud_today(&conn)
    }

    /// Totals per app and per day over the last `days` days, today included.
    pub fn usage(&self, days: u32) -> rusqlite::Result<Usage> {
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        query_usage(&conn, days)
    }

    /// Apps seen so far, most recent first.
    pub fn apps(&self) -> rusqlite::Result<Vec<SeenApp>> {
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = conn
            .prepare_cached("SELECT id, name, last_seen FROM apps ORDER BY last_seen DESC, name")?;
        stmt.query_map([], |r| {
            Ok(SeenApp {
                id: r.get(0)?,
                name: r.get(1)?,
                last_seen: r.get(2)?,
            })
        })?
        .collect()
    }
}

fn writer(conn: Connection, rx: Receiver<Msg>) {
    for msg in rx {
        if let Err(e) = apply(&conn, msg) {
            log::warn!("usage stats write failed: {e}");
        }
    }
}

fn apply(conn: &Connection, msg: Msg) -> rusqlite::Result<()> {
    let bump = |app: &str,
                shown: u32,
                accepted: u32,
                whole: u32,
                dismissed: u32,
                chars: u32,
                saved: u32,
                drafts: u32| {
        conn.prepare_cached(
            "INSERT INTO daily (day, app, shown, accepted, accepted_whole, dismissed, chars, keystrokes_saved, drafts)
             VALUES (date('now', 'localtime'), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (day, app) DO UPDATE SET
                shown = shown + ?2, accepted = accepted + ?3, accepted_whole = accepted_whole + ?4,
                dismissed = dismissed + ?5, chars = chars + ?6, keystrokes_saved = keystrokes_saved + ?7,
                drafts = drafts + ?8",
        )?
        .execute(params![app, shown, accepted, whole, dismissed, chars, saved, drafts])
        .map(drop)
    };
    match msg {
        Msg::Record(Stat::Shown { app }) => bump(&app, 1, 0, 0, 0, 0, 0, 0),
        Msg::Record(Stat::Accepted { app, chars, whole }) => {
            // One accept key replaces `chars` typed keys.
            bump(
                &app,
                0,
                1,
                u32::from(whole),
                0,
                chars,
                chars.saturating_sub(1),
                0,
            )
        }
        Msg::Record(Stat::Dismissed { app }) => bump(&app, 0, 0, 0, 1, 0, 0, 0),
        Msg::Record(Stat::DraftInserted { app }) => bump(&app, 0, 0, 0, 0, 0, 0, 1),
        Msg::Seen(app) => conn
            .prepare_cached(
                "INSERT INTO apps (id, name, last_seen) VALUES (?1, ?2, date('now', 'localtime'))
                 ON CONFLICT (id) DO UPDATE SET name = ?2, last_seen = date('now', 'localtime')",
            )?
            .execute(params![
                app.id,
                if app.name.is_empty() {
                    &app.id
                } else {
                    &app.name
                }
            ])
            .map(drop),
        Msg::CloudRequest => conn
            .prepare_cached(
                "INSERT INTO cloud_requests (day, count) VALUES (date('now', 'localtime'), 1)
                 ON CONFLICT (day) DO UPDATE SET count = count + 1",
            )?
            .execute([])
            .map(drop),
        // The cloud request count stays: it enforces the daily cap.
        Msg::Clear => conn.execute_batch("DELETE FROM daily; DELETE FROM apps;"),
    }
}

fn query_cloud_today(conn: &Connection) -> rusqlite::Result<u32> {
    conn.prepare_cached(
        "SELECT COALESCE(SUM(count), 0) FROM cloud_requests WHERE day = date('now', 'localtime')",
    )?
    .query_row([], |r| r.get(0))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub shown: u64,
    pub accepted: u64,
    pub accepted_whole: u64,
    pub dismissed: u64,
    pub chars: u64,
    pub keystrokes_saved: u64,
    pub drafts: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppUsage {
    pub app: String,
    pub name: String,
    #[serde(flatten)]
    pub counts: Counts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DayUsage {
    pub day: String,
    #[serde(flatten)]
    pub counts: Counts,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub apps: Vec<AppUsage>,
    pub days: Vec<DayUsage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SeenApp {
    pub id: String,
    pub name: String,
    pub last_seen: String,
}

const SUMS: &str = "SUM(shown), SUM(accepted), SUM(accepted_whole), SUM(dismissed), SUM(chars), SUM(keystrokes_saved), SUM(drafts)";

fn counts(r: &rusqlite::Row, from: usize) -> rusqlite::Result<Counts> {
    Ok(Counts {
        shown: r.get(from)?,
        accepted: r.get(from + 1)?,
        accepted_whole: r.get(from + 2)?,
        dismissed: r.get(from + 3)?,
        chars: r.get(from + 4)?,
        keystrokes_saved: r.get(from + 5)?,
        drafts: r.get(from + 6)?,
    })
}

fn query_usage(conn: &Connection, days: u32) -> rusqlite::Result<Usage> {
    let since = format!("-{} days", days.max(1) - 1);
    let apps = conn
        .prepare_cached(&format!(
            "SELECT d.app, COALESCE(a.name, d.app), {SUMS} FROM daily d LEFT JOIN apps a ON a.id = d.app
             WHERE d.day >= date('now', 'localtime', ?1) GROUP BY d.app ORDER BY SUM(keystrokes_saved) DESC, d.app"
        ))?
        .query_map([&since], |r| Ok(AppUsage { app: r.get(0)?, name: r.get(1)?, counts: counts(r, 2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    let days = conn
        .prepare_cached(&format!(
            "SELECT day, {SUMS} FROM daily WHERE day >= date('now', 'localtime', ?1) GROUP BY day ORDER BY day"
        ))?
        .query_map([&since], |r| Ok(DayUsage { day: r.get(0)?, counts: counts(r, 1)? }))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Usage { apps, days })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    #[test]
    fn counters_accumulate_per_app_and_day() {
        let conn = memory();
        let app = || "org.telegram.desktop".to_owned();
        apply(
            &conn,
            Msg::Seen(AppInfo {
                id: app(),
                name: "Telegram".into(),
                title: "secret chat".into(),
            }),
        )
        .unwrap();
        apply(&conn, Msg::Record(Stat::Shown { app: app() })).unwrap();
        apply(&conn, Msg::Record(Stat::Shown { app: app() })).unwrap();
        apply(
            &conn,
            Msg::Record(Stat::Accepted {
                app: app(),
                chars: 12,
                whole: true,
            }),
        )
        .unwrap();
        apply(
            &conn,
            Msg::Record(Stat::Accepted {
                app: app(),
                chars: 4,
                whole: false,
            }),
        )
        .unwrap();
        apply(
            &conn,
            Msg::Record(Stat::Dismissed {
                app: "firefox".into(),
            }),
        )
        .unwrap();
        let usage = query_usage(&conn, 7).unwrap();
        let tg = usage.apps.iter().find(|a| a.app == app()).unwrap();
        assert_eq!(tg.name, "Telegram");
        assert_eq!(
            tg.counts,
            Counts {
                shown: 2,
                accepted: 2,
                accepted_whole: 1,
                chars: 16,
                keystrokes_saved: 14,
                ..Counts::default()
            }
        );
        assert_eq!(usage.days.len(), 1);
        assert_eq!(usage.days[0].counts.dismissed, 1);
        assert_eq!(usage.days[0].counts.shown, 2);
    }

    #[test]
    fn stores_no_window_titles() {
        let conn = memory();
        apply(
            &conn,
            Msg::Seen(AppInfo {
                id: "a".into(),
                name: "A".into(),
                title: "private title".into(),
            }),
        )
        .unwrap();
        let dump: Vec<String> = conn
            .prepare("SELECT id || name || last_seen FROM apps")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(dump.iter().all(|row| !row.contains("private")));
    }

    #[test]
    fn cloud_requests_count_today_and_survive_clear() {
        let conn = memory();
        assert_eq!(query_cloud_today(&conn).unwrap(), 0);
        for _ in 0..3 {
            apply(&conn, Msg::CloudRequest).unwrap();
        }
        conn.execute(
            "INSERT INTO cloud_requests (day, count) VALUES ('2000-01-01', 50)",
            [],
        )
        .unwrap();
        apply(&conn, Msg::Clear).unwrap();
        assert_eq!(query_cloud_today(&conn).unwrap(), 3);
    }

    #[test]
    fn clear_removes_everything() {
        let conn = memory();
        apply(&conn, Msg::Record(Stat::Shown { app: "a".into() })).unwrap();
        apply(&conn, Msg::Clear).unwrap();
        assert_eq!(query_usage(&conn, 30).unwrap(), Usage::default());
    }
}

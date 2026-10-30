//! Pauses from the tray and settings: everything, or one app, for a while or
//! until resumed. The runtime owns the clock and sends the controller the
//! current set as [`Pause`] whenever it changes.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use oft_engine::controller::Pause;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "minutes")]
pub enum PauseFor {
    Minutes(u32),
    UntilResumed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Scope {
    All,
    App { id: String, name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Until {
    at: Option<Instant>,
    /// Unix milliseconds of `at`, for display.
    at_ms: Option<u64>,
}

/// One active pause as the UI and tray show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PauseInfo {
    /// `None` for the global pause.
    pub app: Option<String>,
    pub name: Option<String>,
    /// Unix milliseconds when it ends; `None` until resumed.
    pub until_ms: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Pauses {
    all: Option<Until>,
    apps: BTreeMap<String, (String, Until)>,
}

impl Pauses {
    pub fn set(&mut self, scope: Scope, length: PauseFor, now: Instant) {
        let until = match length {
            PauseFor::UntilResumed => Until { at: None, at_ms: None },
            PauseFor::Minutes(m) => {
                let d = Duration::from_secs(u64::from(m.max(1)) * 60);
                let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default() + d;
                Until { at: Some(now + d), at_ms: Some(ms.as_millis() as u64) }
            }
        };
        match scope {
            Scope::All => self.all = Some(until),
            Scope::App { id, name } => {
                self.apps.insert(id, (name, until));
            }
        }
    }

    /// Ends the pause for one app, or every pause when `app` is `None`.
    pub fn resume(&mut self, app: Option<&str>) {
        match app {
            None => {
                self.all = None;
                self.apps.clear();
            }
            Some(id) => {
                self.apps.remove(id);
            }
        }
    }

    /// Drops pauses that have ended; returns true if any did.
    pub fn expire(&mut self, now: Instant) -> bool {
        let ended = |u: &Until| u.at.is_some_and(|at| at <= now);
        let before = self.apps.len() + usize::from(self.all.is_some());
        if self.all.as_ref().is_some_and(ended) {
            self.all = None;
        }
        self.apps.retain(|_, (_, u)| !ended(u));
        before != self.apps.len() + usize::from(self.all.is_some())
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.all.iter().chain(self.apps.values().map(|(_, u)| u)).filter_map(|u| u.at).min()
    }

    /// The pause state as the controller takes it.
    pub fn state(&self) -> Pause {
        Pause { all: self.all.is_some(), apps: self.apps.keys().cloned().collect() }
    }

    pub fn list(&self) -> Vec<PauseInfo> {
        let all = self.all.iter().map(|u| PauseInfo { app: None, name: None, until_ms: u.at_ms });
        let apps = self.apps.iter().map(|(id, (name, u))| PauseInfo {
            app: Some(id.clone()),
            name: Some(name.clone()),
            until_ms: u.at_ms,
        });
        all.chain(apps).collect()
    }
}

/// "14:35" in local time for `ms` Unix milliseconds.
pub fn local_clock(ms: u64) -> String {
    let of_day = local_minutes(ms).rem_euclid(24 * 60);
    format!("{:02}:{:02}", of_day / 60, of_day % 60)
}

/// Days since 1970-01-01 in local time, for daily counters.
pub fn local_day(ms: u64) -> i64 {
    local_minutes(ms).div_euclid(24 * 60)
}

fn local_minutes(ms: u64) -> i64 {
    (ms / 60_000) as i64 + utc_offset_minutes(ms)
}

#[cfg(unix)]
fn utc_offset_minutes(ms: u64) -> i64 {
    let t = (ms / 1000) as libc::time_t;
    // SAFETY: all-zero is a valid `tm`, and localtime_r only writes into it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff as i64 / 60
}

#[cfg(windows)]
fn utc_offset_minutes(_ms: u64) -> i64 {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_ID_DAYLIGHT, TIME_ZONE_INFORMATION};
    // SAFETY: all-zero is a valid TIME_ZONE_INFORMATION, and `tz` is a valid out-pointer.
    let mut tz: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    let kind = unsafe { GetTimeZoneInformation(&mut tz) };
    let bias = tz.Bias + if kind == TIME_ZONE_ID_DAYLIGHT { tz.DaylightBias } else { tz.StandardBias };
    -i64::from(bias)
}

#[cfg(not(any(unix, windows)))]
fn utc_offset_minutes(_ms: u64) -> i64 {
    0
}

/// Unix milliseconds now.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> Scope {
        Scope::App { id: id.into(), name: id.to_uppercase() }
    }

    #[test]
    fn global_pause_expires_at_its_deadline() {
        let now = Instant::now();
        let mut p = Pauses::default();
        p.set(Scope::All, PauseFor::Minutes(15), now);
        assert_eq!(p.state(), Pause { all: true, apps: vec![] });
        assert_eq!(p.next_deadline(), Some(now + Duration::from_secs(900)));
        assert!(!p.expire(now + Duration::from_secs(899)));
        assert!(p.expire(now + Duration::from_secs(900)));
        assert_eq!(p.state(), Pause::default());
        assert_eq!(p.next_deadline(), None);
    }

    #[test]
    fn app_pause_until_resumed_never_expires() {
        let now = Instant::now();
        let mut p = Pauses::default();
        p.set(app("slack"), PauseFor::UntilResumed, now);
        assert_eq!(p.state(), Pause { all: false, apps: vec!["slack".into()] });
        assert_eq!(p.next_deadline(), None);
        assert!(!p.expire(now + Duration::from_secs(86_400)));
        p.resume(Some("slack"));
        assert_eq!(p.state(), Pause::default());
    }

    #[test]
    fn resume_all_clears_app_pauses_too() {
        let now = Instant::now();
        let mut p = Pauses::default();
        p.set(Scope::All, PauseFor::UntilResumed, now);
        p.set(app("a"), PauseFor::Minutes(60), now);
        assert_eq!(p.list().len(), 2);
        p.resume(None);
        assert!(p.list().is_empty());
    }

    #[test]
    fn clock_and_day_agree() {
        let ms = 1_700_000_000_000;
        let s = local_clock(ms);
        assert_eq!((s.len(), &s[2..3]), (5, ":"));
        let minutes_into_day: u64 = s[..2].parse::<u64>().unwrap() * 60 + s[3..].parse::<u64>().unwrap();
        // The next local midnight starts the next day.
        let to_midnight = (24 * 60 - minutes_into_day) * 60_000;
        assert_eq!(local_day(ms + to_midnight), local_day(ms) + 1);
        assert_eq!(local_day(ms + to_midnight - 60_000), local_day(ms));
    }
}

//! The machine a run happens on, and its load.

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub cpu: String,
    pub threads: usize,
    pub ram_gb: f64,
    /// GPU name, memory and driver, when `nvidia-smi` reports one.
    pub gpu: String,
    pub os: String,
    pub kernel: String,
}

impl Machine {
    pub fn detect() -> Machine {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let cpu = cpuinfo
            .lines()
            .find_map(|l| l.strip_prefix("model name"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_owned())
            .unwrap_or_else(|| std::env::consts::ARCH.to_owned());
        let ram_kb: f64 = fs::read_to_string("/proc/meminfo")
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
            .unwrap_or(0.0);
        let os = fs::read_to_string("/etc/os-release")
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_owned())
            .unwrap_or_else(|| std::env::consts::OS.to_owned());
        Machine {
            cpu,
            threads: std::thread::available_parallelism().map_or(0, |n| n.get()),
            ram_gb: (ram_kb / 1024.0 / 1024.0 * 10.0).round() / 10.0,
            gpu: nvidia_gpu().unwrap_or_default(),
            os,
            kernel: fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|s| s.trim().to_owned())
                .unwrap_or_default(),
        }
    }
}

fn nvidia_gpu() -> Option<String> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,driver_version",
            "--format=csv,noheader",
        ])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let line = s.lines().next()?.trim();
    let mut f = line.split(',').map(str::trim);
    let (name, mem, driver) = (f.next()?, f.next()?, f.next()?);
    Some(format!("{name}, {mem}, driver {driver}"))
}

/// The 1-minute load average, when the OS reports it.
pub fn load1() -> Option<f64> {
    fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc(secs)
}

fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_known_instants() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_793_318_400 + 3_723), "2026-10-30T01:02:03Z");
    }
}

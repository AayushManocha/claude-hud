//! Plan usage limits (5-hour / 7-day windows).
//!
//! Claude Code only exposes these to its status line command (`rate_limits` in the JSON on
//! stdin), so `cc-hud statusline` sits in that slot: it records the numbers for the dashboard
//! and hands the same input to the user's own status line, if they have one.

use crate::state;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct Window {
    pub label: &'static str,
    /// 0..=100, or above once a spend limit is exceeded.
    pub pct: f64,
    pub resets_at: u64,
}

pub struct Usage {
    pub windows: Vec<Window>,
    /// Unix seconds when a session last reported these numbers.
    pub updated: u64,
}

const WINDOWS: &[(&str, &str)] = &[("five_hour", "5h"), ("seven_day", "7d"), ("spend_limit", "spend")];

fn file() -> PathBuf {
    state::state_dir().join("usage")
}

/// Persist `rate_limits` from a status line payload. Payloads without it (API-key users,
/// or before a session's first response) leave the last known numbers alone.
fn record(input: &str) -> Result<()> {
    let Some(limits) = serde_json::from_str::<Value>(input).ok().and_then(|v| v.get("rate_limits").cloned()) else {
        return Ok(());
    };
    let path = file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(&json!({ "rate_limits": limits, "updated": state::now() }))?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn parse(v: &Value, now: u64) -> Option<Usage> {
    let limits = v.get("rate_limits")?;
    let windows: Vec<Window> = WINDOWS
        .iter()
        .filter_map(|&(key, label)| {
            let w = limits.get(key)?;
            let resets_at = w.get("resets_at")?.as_u64()?;
            // A window that has reset has no usage until the next response reports it.
            (resets_at > now).then_some(Window { label, pct: w.get("used_percentage")?.as_f64()?, resets_at })
        })
        .collect();
    Some(Usage { windows, updated: v.get("updated")?.as_u64()? })
}

pub fn load() -> Option<Usage> {
    parse(&serde_json::from_slice(&std::fs::read(file()).ok()?).ok()?, state::now())
}

/// `cc-hud statusline [--then CMD]`: record usage, then run `CMD` (the user's previous
/// status line) with the same stdin and pass its output through.
pub fn statusline(then: Option<String>) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let _ = record(&input);
    let Some(cmd) = then else { return Ok(()) };
    let mut child = Command::new("sh").arg("-c").arg(&cmd).stdin(Stdio::piped()).spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    child.wait()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_live_windows_only() {
        let v = json!({
            "updated": 100,
            "rate_limits": {
                "five_hour": { "used_percentage": 23.5, "resets_at": 2000 },
                "seven_day": { "used_percentage": 41.0, "resets_at": 500 },
            }
        });
        let u = parse(&v, 1000).unwrap();
        assert_eq!(u.windows.len(), 1);
        assert_eq!(u.windows[0].label, "5h");
        assert_eq!(u.updated, 100);
    }

    #[test]
    fn no_rate_limits_no_usage() {
        assert!(parse(&json!({ "updated": 1 }), 0).is_none());
    }
}

//! Zellij backend. Each worktree/chat is a tab in one zellij session (the one you are
//! in, else `CC_HUD_SESSION`, default `dev`). Zellij's CLI can't move a client between
//! sessions, so unlike tmux there is no session per workspace.
//!
//! Zellij has no per-tab user options, so the tags tmux keeps in `@hud_*` live in
//! `<state dir>/zellij-tags/<session>@<tab id>.json` instead. Pane ids are
//! `<terminal id>@<session>`: zellij numbers panes per session.

use super::{Multiplexer, NewWindow, PaneInfo};
use crate::config;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Zellij;

#[derive(Serialize, Deserialize)]
struct Tags {
    session: String,
    tab: u64,
    kind: String,
    repo: Option<String>,
    topic: Option<String>,
    path: String,
}

fn tags_dir() -> PathBuf {
    crate::state::state_dir().join("zellij-tags")
}

fn tags_file(session: &str, tab: u64) -> PathBuf {
    tags_dir().join(format!("{session}@{tab}.json"))
}

fn all_tags() -> Vec<Tags> {
    let Ok(rd) = std::fs::read_dir(tags_dir()) else { return vec![] };
    rd.flatten()
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect()
}

/// Run `zellij [--session S] <args>`; stdout on success.
fn zj(session: Option<&str>, args: &[&str]) -> Option<String> {
    let mut c = Command::new("zellij");
    if let Some(s) = session {
        c.args(["--session", s]);
    }
    let o = c.args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn action(session: &str, args: &[&str]) -> Option<String> {
    let mut a = vec!["action"];
    a.extend(args);
    zj(Some(session), &a)
}

/// Sessions known to zellij; `alive_only` skips exited (resurrectable) ones.
fn sessions(alive_only: bool) -> Option<Vec<String>> {
    let o = Command::new("zellij").args(["list-sessions", "-n"]).output().ok()?;
    if !o.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !(alive_only && l.contains("EXITED")))
            .filter_map(|l| Some(l.split(" [").next()?.trim().to_string()))
            .filter(|n| !n.is_empty())
            .collect(),
    )
}

/// `N@session` -> (session, N)
fn split_id(pane_id: &str) -> Option<(&str, &str)> {
    let (n, s) = pane_id.split_once('@')?;
    Some((s, n))
}

/// `session:tab` -> (session, tab id)
fn split_window(window_id: &str) -> Option<(&str, &str)> {
    window_id.rsplit_once(':')
}

fn current_session() -> Option<String> {
    std::env::var("ZELLIJ_SESSION_NAME").ok().filter(|s| !s.is_empty())
}

/// Terminal panes of one session as zellij reports them (with tab info).
fn terminal_panes(session: &str) -> Option<Vec<Value>> {
    let out = action(session, &["list-panes", "-t", "-j"])?;
    let v: Vec<Value> = serde_json::from_str(&out).ok()?;
    Some(v.into_iter().filter(|p| p["is_plugin"] == false).collect())
}

fn has_client(session: &str) -> bool {
    action(session, &["list-clients"]).is_some_and(|o| o.lines().count() > 1)
}

/// Tab ids of a session, and which of them is active.
fn tabs(session: &str) -> Option<Vec<(u64, bool)>> {
    let v: Vec<Value> = serde_json::from_str(&action(session, &["list-tabs", "-j"])?).ok()?;
    Some(v.iter().filter_map(|t| Some((t["tab_id"].as_u64()?, t["active"] == true))).collect())
}

/// Bring a tab (and pane) to the front for the client attached to `session`.
fn focus(session: &str, tab: &str, pane: Option<&str>) {
    let _ = action(session, &["go-to-tab-by-id", tab]);
    if let Some(p) = pane {
        let _ = action(session, &["focus-pane-id", &format!("terminal_{p}")]);
    }
}

/// First terminal pane of a tab.
fn tab_pane(session: &str, tab: u64) -> Option<String> {
    let p = terminal_panes(session)?.into_iter().find(|p| p["tab_id"].as_u64() == Some(tab))?;
    Some(p["id"].as_u64()?.to_string())
}

/// Tags whose tab still exists. Tags of closed tabs and dead sessions are dropped.
fn live_tags() -> Vec<Tags> {
    let Some(alive) = sessions(true) else { return vec![] };
    let live: HashMap<String, HashSet<u64>> = alive
        .into_iter()
        .map(|s| {
            let ids = tabs(&s).unwrap_or_default().into_iter().map(|(t, _)| t).collect();
            (s, ids)
        })
        .collect();
    all_tags()
        .into_iter()
        .filter(|t| {
            let ok = live.get(&t.session).is_some_and(|ids| ids.contains(&t.tab));
            if !ok {
                let _ = std::fs::remove_file(tags_file(&t.session, t.tab));
            }
            ok
        })
        .collect()
}

fn dump(pane_id: &str, flags: &[&str], tail: Option<usize>) -> String {
    let Some((s, n)) = split_id(pane_id) else { return String::new() };
    let id = format!("terminal_{n}");
    let mut a = vec!["dump-screen", "-p", &id];
    a.extend(flags);
    let text = action(s, &a).unwrap_or_default();
    match tail {
        Some(k) => {
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(k)..].join("\n") + "\n"
        }
        None => text,
    }
}

/// The session new tabs go to: the current one, else the workspace session (made
/// detached, rooted at `cwd`, if it doesn't exist). Returns (session, first tab if just created).
fn target_session(cwd: &Path) -> Result<(String, Option<(u64, String)>)> {
    if let Some(s) = current_session() {
        return Ok((s, None));
    }
    let name = std::env::var("CC_HUD_SESSION").unwrap_or_else(|_| "dev".into());
    if sessions(true).is_some_and(|s| s.contains(&name)) {
        return Ok((name, None));
    }
    // The server (and so its first pane's shell) starts in our working directory.
    let started = Command::new("zellij")
        .args(["attach", "--create-background", &name])
        .current_dir(cwd)
        .output()
        .is_ok_and(|o| o.status.success());
    if !started {
        bail!("zellij couldn't create session {name}");
    }
    // The server needs a moment before its first tab exists.
    for _ in 0..50 {
        if let Some(p) = terminal_panes(&name).and_then(|ps| ps.into_iter().next()) {
            if let (Some(tab), Some(pane)) = (p["tab_id"].as_u64(), p["id"].as_u64()) {
                return Ok((name, Some((tab, pane.to_string()))));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("zellij session {name} never became ready")
}

impl Multiplexer for Zellij {
    fn live_panes(&self) -> Option<HashMap<String, PaneInfo>> {
        let names = sessions(true)?;
        let tags = live_tags();
        let mut map = HashMap::new();
        std::thread::scope(|sc| {
            let jobs: Vec<_> = names
                .iter()
                .map(|s| sc.spawn(move || (s, terminal_panes(s).unwrap_or_default())))
                .collect();
            for j in jobs {
                let Ok((s, panes)) = j.join() else { continue };
                for p in panes {
                    let (Some(id), Some(tab)) = (p["id"].as_u64(), p["tab_id"].as_u64()) else { continue };
                    let t = tags.iter().find(|t| &t.session == s && t.tab == tab);
                    map.insert(
                        format!("{id}@{s}"),
                        PaneInfo {
                            session: s.clone(),
                            window_id: format!("{s}:{tab}"),
                            window_name: p["tab_name"].as_str().unwrap_or("").to_string(),
                            kind: t.map(|t| t.kind.clone()).unwrap_or_default(),
                            topic: t.and_then(|t| t.topic.clone()).unwrap_or_default(),
                        },
                    );
                }
            }
        });
        Some(map)
    }

    fn current_pane(&self) -> Option<String> {
        let id = std::env::var("ZELLIJ_PANE_ID").ok()?;
        Some(format!("{id}@{}", current_session()?))
    }

    fn is_visible(&self, pane_id: &str) -> bool {
        let Some((s, n)) = split_id(pane_id) else { return false };
        // A session nobody is attached to is not being looked at.
        if !has_client(s) {
            return false;
        }
        let Some(pane) = terminal_panes(s).and_then(|ps| ps.into_iter().find(|p| p["id"].to_string() == n)) else {
            return false;
        };
        let tab = pane["tab_id"].as_u64();
        pane["is_focused"] == true && tabs(s).is_some_and(|ts| ts.iter().any(|&(t, active)| active && Some(t) == tab))
    }

    /// Only works within the session the client is attached to: zellij's CLI can't switch sessions.
    fn jump(&self, _client: Option<&str>, p: &PaneInfo, pane_id: &str) {
        let (Some((s, n)), Some((_, tab))) = (split_id(pane_id), split_window(&p.window_id)) else { return };
        focus(s, tab, Some(n));
    }

    fn capture(&self, pane_id: &str, lines: usize) -> String {
        dump(pane_id, &[], Some(lines))
    }

    fn capture_ansi(&self, pane_id: &str, lines: usize) -> String {
        dump(pane_id, &["-a"], Some(lines))
    }

    fn capture_ansi_all(&self, pane_id: &str) -> String {
        dump(pane_id, &["-a", "-f"], None)
    }

    fn send_text(&self, pane_id: &str, text: &str) -> bool {
        let Some((s, n)) = split_id(pane_id) else { return false };
        action(s, &["write-chars", "-p", &format!("terminal_{n}"), "--", text]).is_some()
    }

    fn send_enter(&self, pane_id: &str) -> bool {
        let Some((s, n)) = split_id(pane_id) else { return false };
        action(s, &["send-keys", "-p", &format!("terminal_{n}"), "Enter"]).is_some()
    }

    fn open_window(&self, _client: Option<&str>, w: &NewWindow) -> Result<()> {
        let cwd = w.cwd.display().to_string();
        let (session, fresh) = target_session(w.cwd)?;
        let (tab, pane) = match fresh {
            // A brand-new session already has an empty tab: use it rather than add a second.
            Some((tab, pane)) => {
                action(&session, &["rename-tab-by-id", &tab.to_string(), w.name]);
                (tab, pane)
            }
            None => {
                // A tab only gets its terminal once a client (with a screen size) is attached.
                if !has_client(&session) {
                    bail!("no zellij client attached to session {session}; attach first (cc-hud open)");
                }
                let id = action(&session, &["new-tab", "--name", w.name, "--cwd", &cwd])
                    .context("zellij new-tab failed")?;
                let tab: u64 = id.trim().parse().context("unexpected zellij new-tab output")?;
                // The tab's pane shows up in the listing a moment after the tab exists.
                let pane = (0..50)
                    .find_map(|_| {
                        let p = tab_pane(&session, tab);
                        if p.is_none() {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        p
                    })
                    .context("new zellij tab has no pane")?;
                (tab, pane)
            }
        };

        std::fs::create_dir_all(tags_dir())?;
        let tags = Tags {
            session: session.clone(),
            tab,
            kind: w.kind.into(),
            repo: w.repo.map(Into::into),
            topic: w.topic.map(Into::into),
            path: cwd,
        };
        std::fs::write(tags_file(&session, tab), serde_json::to_vec(&tags)?)?;

        if w.launch {
            let id = format!("{pane}@{session}");
            self.send_text(&id, &config::claude_cmd());
            self.send_enter(&id);
        }
        focus(&session, &tab.to_string(), Some(&pane));
        Ok(())
    }

    fn focus_worktree(&self, _client: Option<&str>, path: &Path) -> bool {
        let want = path.display().to_string();
        let Some(t) = live_tags().into_iter().find(|t| t.path == want) else { return false };
        let Some(pane) = tab_pane(&t.session, t.tab) else { return false };
        focus(&t.session, &t.tab.to_string(), Some(&pane));
        true
    }

    fn kill_worktree(&self, path: &Path) {
        let want = path.display().to_string();
        for t in live_tags().into_iter().filter(|t| t.path == want) {
            close_tab(&t.session, t.tab);
        }
    }

    fn kill_chat(&self, p: &PaneInfo) -> bool {
        let Some((s, tab)) = split_window(&p.window_id) else { return false };
        tab.parse().is_ok_and(|tab| close_tab(s, tab))
    }

    /// Zellij keeps no tab activity time; the tag file's mtime (creation) stands in.
    fn session_activity(&self) -> HashMap<PathBuf, u64> {
        live_tags()
            .into_iter()
            .filter_map(|t| {
                let secs = std::fs::metadata(tags_file(&t.session, t.tab))
                    .ok()?
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                Some((PathBuf::from(t.path), secs))
            })
            .collect()
    }

    /// Zellij has no user options; settings come from `CC_HUD_*` environment variables.
    fn option(&self, _name: &str) -> Option<String> {
        None
    }

    fn message(&self, _text: &str, _ms: u32) -> bool {
        false
    }

    fn set_window_glyphs(&self, _glyphs: &[(String, Option<&'static str>)]) {}

    fn attach_workspace(&self, session: &str, root: &Path) -> std::io::Error {
        use std::os::unix::process::CommandExt;
        Command::new("zellij").args(["attach", "--create", session]).current_dir(root).exec()
    }
}

fn close_tab(session: &str, tab: u64) -> bool {
    let ok = action(session, &["close-tab-by-id", &tab.to_string()]).is_some();
    let _ = std::fs::remove_file(tags_file(session, tab));
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_pane_and_window_ids() {
        assert_eq!(split_id("3@dev"), Some(("dev", "3")));
        assert_eq!(split_id("3@a@b"), Some(("a@b", "3"))); // first `@` separates the number
        assert_eq!(split_id("%3"), None);
        assert_eq!(split_window("dev:7"), Some(("dev", "7")));
        assert_eq!(split_window("my:session:7"), Some(("my:session", "7")));
    }
}

//! The Rex backend: everything cc-hud needs from a terminal multiplexer, via the `rex` CLI.
//!
//! A Rex pane has no globally unique short id like tmux's `%12`, and targeting a block
//! needs its session, so a pane id here is `<session id>/<block id>`.
//! Rex has no per-session options either; the `@hud_*` tags tmux keeps live in files
//! under `<state dir>/tags`, keyed by session id.

use crate::state;
use crate::tmux::PaneInfo;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The `rex` executable. Rex puts its directory on `PATH` only in the terminals it opens,
/// so a process started by a Rex action or key binding may not find it there.
fn bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>());
        let bin_dir = std::env::var_os("REX_BIN_DIR").map(PathBuf::from);
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let app = home.join("Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS");
        on_path
            .chain(bin_dir)
            .chain([app])
            .map(|d| d.join("rex"))
            .find(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("rex"))
    })
}

/// `--autostart=false`: never launch a Rex server just to ask whether one is running.
fn cmd() -> Command {
    let mut c = Command::new(bin());
    c.args(["--autostart=false", "--timeout", "3s"]).stdin(std::process::Stdio::null());
    c
}

fn out(args: &[&str]) -> Option<String> {
    let o = cmd().args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn json(args: &[&str]) -> Option<Value> {
    serde_json::from_str(&out(args)?).ok()
}

pub fn run(args: &[&str]) -> bool {
    cmd().args(args).output().is_ok_and(|o| o.status.success())
}

/// Is a Rex server reachable?
pub fn available() -> bool {
    run(&["whoami"])
}

pub fn pane_id(session: &str, block: &str) -> String {
    format!("{session}/{block}")
}

/// `(session id, block id)` of a Rex pane id.
pub fn split(pane: &str) -> Option<(&str, &str)> {
    pane.split_once('/').filter(|(s, b)| s.starts_with("session:") && b.starts_with("block:"))
}

pub fn is_pane(pane: &str) -> bool {
    split(pane).is_some()
}

/// The pane this process runs in, from the variables Rex sets in its terminals.
pub fn env_pane() -> Option<String> {
    let get = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
    Some(pane_id(&get("REX_SESSION")?, &get("REX_BLOCK")?))
}

#[derive(Default, Serialize, Deserialize)]
pub struct Tags {
    pub kind: String,
    pub repo: Option<String>,
    pub topic: Option<String>,
    pub path: String,
}

fn tags_dir() -> PathBuf {
    state::state_dir().join("tags")
}

fn tags_file(session: &str) -> PathBuf {
    let safe: String = session.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    tags_dir().join(format!("{safe}.json"))
}

fn read_tags(session: &str) -> Option<Tags> {
    serde_json::from_slice(&std::fs::read(tags_file(session)).ok()?).ok()
}

fn write_tags(session: &str, t: &Tags) -> Result<()> {
    std::fs::create_dir_all(tags_dir())?;
    std::fs::write(tags_file(session), serde_json::to_vec(t)?)?;
    Ok(())
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Terminal blocks of every session, keyed by pane id, or None if Rex isn't reachable.
pub fn live_panes() -> Option<HashMap<String, PaneInfo>> {
    let list = json(&["ls", "--json"])?;
    let sessions = list.get("sessions")?.as_array()?;
    // Each call is a process spawn; make them concurrently rather than one after another.
    let views: Vec<Option<Value>> = std::thread::scope(|scope| {
        let calls: Vec<_> = sessions
            .iter()
            .map(|s| scope.spawn(move || json(&["session", "inspect", str_of(s, "session_id"), "--json"])))
            .collect();
        calls.into_iter().map(|c| c.join().ok().flatten()).collect()
    });
    let mut map = HashMap::new();
    for (s, view) in sessions.iter().zip(views) {
        let sid = str_of(s, "session_id");
        let Some(view) = view else { continue };
        let tags = read_tags(sid).unwrap_or_default();
        let windows = view.get("windows").and_then(Value::as_array).into_iter().flatten();
        for w in windows {
            let layers = w.get("layers").and_then(Value::as_array).into_iter().flatten();
            let blocks = layers.flat_map(|l| l.get("blocks").and_then(Value::as_array).into_iter().flatten());
            for b in blocks.filter(|b| str_of(b, "creator_name") == "com.superlogical.terminal") {
                map.insert(
                    pane_id(sid, str_of(b, "block_id")),
                    PaneInfo {
                        session: str_of(s, "label").to_string(),
                        session_key: sid.to_string(),
                        window_id: str_of(w, "window_id").to_string(),
                        window_name: str_of(w, "label").to_string(),
                        kind: tags.kind.clone(),
                        topic: tags.topic.clone().unwrap_or_default(),
                    },
                );
            }
        }
    }
    Some(map)
}

/// Bring a pane into view: select its session in the app (`client` is a Rex client label or id), then focus it.
/// The pane is focused in its session even if the app can't be asked to show the session;
/// that failure (typically Remote Control being off in the app) is returned.
pub fn jump(client: Option<&str>, p: &PaneInfo, pane: &str) -> Result<(), String> {
    let Some((sid, bid)) = split(pane) else { return Ok(()) };
    let shown = select_session(client, sid, &p.window_id);
    run(&["focus", "-s", sid, bid]);
    shown
}

/// Ask a Rex app to show a session. Needs a connected app that accepts actions from the CLI
/// (Remote Control in its settings); not needed, and skipped, for the session this process runs in.
fn select_session(client: Option<&str>, sid: &str, window_id: &str) -> Result<(), String> {
    if std::env::var("REX_SESSION").is_ok_and(|s| s == sid) {
        return Ok(());
    }
    let mut args = vec!["do".to_string(), "session.select".to_string(), format!("session_id={sid}")];
    if !window_id.is_empty() {
        args.push(format!("window_id={window_id}"));
    }
    client_do(client, &args).map_err(|e| format!("couldn't switch the Rex app to the session: {e}"))
}

/// Run `rex [-C client] do ...`: an action of a Rex app, which must accept them from the CLI.
fn client_do(client: Option<&str>, args: &[String]) -> Result<(), String> {
    let mut c = cmd();
    if let Some(cl) = client {
        c.args(["-C", cl]);
    }
    let o = c.args(args).output().map_err(|e| format!("running rex: {e}"))?;
    if o.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&o.stderr).trim().trim_start_matches("Error: ").to_string())
}

/// A persistent dashboard (a tab) hands the screen back. `action` is the Rex action that toggles
/// the dashboard tab; it switches the session's active window and returns the window to show,
/// `{"session_id": …, "window_id": …}`. Rex.app keeps its own selected tab, so it is asked to show
/// that window too (needs Remote Control in its settings).
pub fn leave_dashboard(client: Option<&str>, action: &str) -> Result<(), String> {
    let result = json(&["do", action, "--json"]).ok_or_else(|| format!("rex action {action} failed"))?;
    let (sid, wid) = (str_of(&result, "session_id"), str_of(&result, "window_id"));
    if sid.is_empty() || wid.is_empty() {
        return Ok(());
    }
    select_session_window(client, sid, wid).map_err(|e| format!("couldn't switch the Rex app to the tab: {e}"))
}

/// Ask the app to show the window this process runs in, if the session still has it active.
/// The `hud` action asks too, but right after creating the window, when the app may not know of
/// it yet; this runs once the dashboard is up, so a few tries are made in case it is still catching up.
pub fn show_own_window(client: Option<&str>) {
    let (Ok(sid), Ok(bid)) = (std::env::var("REX_SESSION"), std::env::var("REX_BLOCK")) else { return };
    for (attempt, delay) in [0, 150, 400, 1000].into_iter().enumerate() {
        std::thread::sleep(Duration::from_millis(delay));
        let Some(block) = json(&["block", "inspect", "-s", &sid, &bid, "--json"]) else { continue };
        let wid = block.pointer("/block/window_id").and_then(Value::as_str).unwrap_or("");
        if wid.is_empty() {
            continue;
        }
        // The first try follows the key press by a moment, so it goes straight out; later ones
        // first check the user hasn't gone elsewhere in the meantime.
        if attempt > 0 {
            let active = json(&["session", "inspect", &sid, "--json"]);
            if active.as_ref().map(|v| str_of(v, "active_window_id")) != Some(wid) {
                return;
            }
        }
        if select_session_window(client, &sid, wid).is_ok() {
            return;
        }
    }
}

fn select_session_window(client: Option<&str>, sid: &str, wid: &str) -> Result<(), String> {
    client_do(client, &["do".into(), "session.select".into(), format!("session_id={sid}"), format!("window_id={wid}")])
}

/// A pane's screen and scrollback as terminal escape sequences, carriage returns removed.
/// The dashboard asks for the same pane as plain text and as colour a moment apart;
/// both are served from one capture if it is recent enough.
fn capture_vt(pane: &str) -> String {
    static CACHE: Mutex<Option<HashMap<String, (Instant, String)>>> = Mutex::new(None);
    const FRESH: Duration = Duration::from_millis(200);
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some((_, text)) = cache.get(pane).filter(|(at, _)| at.elapsed() < FRESH) {
        return text.clone();
    }
    let text = split(pane)
        .and_then(|(sid, bid)| out(&["capture", "-s", sid, "-b", bid, "--format=vt", "--trim"]))
        .unwrap_or_default()
        .replace('\r', "");
    cache.retain(|_, (at, _)| at.elapsed() < FRESH);
    cache.insert(pane.to_string(), (Instant::now(), text.clone()));
    text
}

/// `text` without terminal escape sequences (CSI and OSC).
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A pane's screen and scrollback; the last `lines` lines unless `lines` is None.
/// With `ansi`, colours are kept (SGR sequences).
pub fn capture(pane: &str, lines: Option<usize>, ansi: bool) -> String {
    let vt = capture_vt(pane);
    let text = if ansi { vt } else { strip_escapes(&vt) };
    match lines {
        Some(n) => {
            let all: Vec<&str> = text.lines().collect();
            all[all.len().saturating_sub(n)..].join("\n") + "\n"
        }
        None => text,
    }
}

pub fn send_text(pane: &str, text: &str) -> bool {
    split(pane).is_some_and(|(s, b)| run(&["send", "-s", s, "-b", b, "--", text]))
}

pub fn send_enter(pane: &str) -> bool {
    split(pane).is_some_and(|(s, b)| run(&["send-key", "-s", s, "-b", b, "enter"]))
}

/// Is this the pane the user is looking at right now (as far as the server can tell)?
pub fn visible(pane: &str) -> bool {
    let Some((sid, bid)) = split(pane) else { return false };
    let Some(view) = json(&["session", "inspect", sid, "--json"]) else { return false };
    let active = str_of(&view, "active_window_id");
    let windows = view.get("windows").and_then(Value::as_array).into_iter().flatten();
    windows.filter(|w| str_of(w, "window_id") == active).any(|w| str_of(w, "focused_block_id") == bid)
}

pub fn kill_session(session: &str) -> bool {
    let _ = std::fs::remove_file(tags_file(session));
    run(&["kill", session])
}

/// A new session with one tagged window; starts `launch` in it if given. Returns the session id and first window id.
pub fn open_window(name: &str, cwd: &Path, tags: &Tags, launch: Option<&str>) -> Result<(String, String)> {
    let cwd_s = cwd.display().to_string();
    let created = json(&["new", name, "-c", &cwd_s, "--window", name, "--json"]).context("rex new failed")?;
    let sid = str_of(&created, "session_id").to_string();
    let win = created.pointer("/initial_windows/0").cloned().unwrap_or(Value::Null);
    let wid = str_of(&win, "window_id").to_string();
    let bid = win.pointer("/block_ids/0").and_then(Value::as_str).unwrap_or("").to_string();
    if sid.is_empty() || bid.is_empty() {
        bail!("unexpected rex output");
    }
    write_tags(&sid, tags)?;
    if let Some(cmd) = launch {
        let pane = pane_id(&sid, &bid);
        send_text(&pane, cmd);
        send_enter(&pane);
    }
    Ok((sid, wid))
}

pub fn focus(client: Option<&str>, sid: &str, wid: &str) -> Result<(), String> {
    let shown = select_session(client, sid, wid);
    run(&["focus", "-s", sid, wid]);
    shown
}

/// Session tagged for this worktree path that still exists, with its first window id.
pub fn find_window(path: &Path) -> Option<(String, String)> {
    let want = path.display().to_string();
    let sessions = json(&["ls", "--json"])?;
    sessions.get("sessions")?.as_array()?.iter().find_map(|s| {
        let sid = str_of(s, "session_id");
        (read_tags(sid)?.path == want).then_some(())?;
        let view = json(&["session", "inspect", sid, "--json"])?;
        let wid = view.pointer("/windows/0/window_id")?.as_str()?;
        Some((sid.to_string(), wid.to_string()))
    })
}

/// Attach to (or create) the workspace session, and show it in the app.
pub fn open(session: &str, root: &Path) -> Result<()> {
    let existing = json(&["ls", "--json"])
        .and_then(|l| l.get("sessions")?.as_array().cloned())
        .and_then(|ss| ss.iter().find(|s| str_of(s, "label") == session).map(|s| str_of(s, "session_id").to_string()));
    let sid = match existing {
        Some(s) => s,
        None => {
            let c = json(&["new", session, "-c", &root.display().to_string(), "--json"]).context("rex new failed")?;
            str_of(&c, "session_id").to_string()
        }
    };
    println!("{sid}");
    select_session(None, &sid, "").map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colour_and_title_sequences() {
        assert_eq!(strip_escapes("\x1b[0m\x1b[38;5;2m/tmp\x1b[0m λ"), "/tmp λ");
        assert_eq!(strip_escapes("a\x1b]0;title\x07b\x1b]7;x\x1b\\c"), "abc");
    }

    #[test]
    fn pane_ids_round_trip() {
        let id = pane_id("session:1", "block:2");
        assert_eq!(split(&id), Some(("session:1", "block:2")));
        assert!(!is_pane("%12"));
    }
}

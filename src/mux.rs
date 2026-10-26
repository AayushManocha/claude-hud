//! One front door for the terminal multiplexers cc-hud supports: tmux and Rex.
//!
//! Operations on an existing pane go by the pane's id, which says which backend owns it
//! (`%12` is tmux, `session:…/block:…` is Rex), so both can be in use at once.
//! Operations that create things or read settings use the *active* backend:
//! `CC_HUD_MUX=tmux|rex` if set, else the multiplexer cc-hud runs inside (`$TMUX`, then
//! `$REX_SESSION`), else tmux if a tmux server is up, else Rex if a Rex server is up.

use crate::tmux::{self, PaneInfo};
use crate::rex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    Tmux,
    Rex,
}

fn env_set(k: &str) -> bool {
    std::env::var(k).is_ok_and(|v| !v.is_empty())
}

fn detect() -> Backend {
    match std::env::var("CC_HUD_MUX").as_deref() {
        Ok("tmux") => return Backend::Tmux,
        Ok("rex") => return Backend::Rex,
        _ => {}
    }
    if env_set("TMUX") {
        Backend::Tmux
    } else if env_set("REX_SESSION") {
        Backend::Rex
    } else if tmux::run(&["list-sessions"]) {
        Backend::Tmux
    } else if rex::available() {
        Backend::Rex
    } else {
        Backend::Tmux
    }
}

pub fn active() -> Backend {
    static ACTIVE: OnceLock<Backend> = OnceLock::new();
    *ACTIVE.get_or_init(detect)
}

pub fn is_rex(pane: &str) -> bool {
    rex::is_pane(pane)
}

static TMUX_UP: AtomicBool = AtomicBool::new(false);
static REX_UP: AtomicBool = AtomicBool::new(false);

/// All live panes of both backends, or None if neither is reachable.
pub fn live_panes() -> Option<HashMap<String, PaneInfo>> {
    let t = tmux::live_panes();
    let r = rex::live_panes();
    TMUX_UP.store(t.is_some(), Ordering::Relaxed);
    REX_UP.store(r.is_some(), Ordering::Relaxed);
    if t.is_none() && r.is_none() {
        return None;
    }
    let mut all = t.unwrap_or_default();
    all.extend(r.unwrap_or_default());
    Some(all)
}

/// Did the last `live_panes` reach the backend that owns this pane? A pane missing from
/// the listing only counts as dead if so; otherwise we just can't see it.
pub fn reachable(pane: &str) -> bool {
    if is_rex(pane) { REX_UP.load(Ordering::Relaxed) } else { TMUX_UP.load(Ordering::Relaxed) }
}

pub fn jump(client: Option<&str>, p: &PaneInfo, pane: &str) -> Result<(), String> {
    if is_rex(pane) {
        rex::jump(client, p, pane)
    } else {
        tmux::jump(client, p, pane);
        Ok(())
    }
}

/// The pane the user was just working in (tmux only: Rex has no client to ask).
pub fn current_pane(client: Option<&str>) -> Option<String> {
    match active() {
        Backend::Tmux => tmux::current_pane(client),
        Backend::Rex => None,
    }
}

/// The pane this process runs in.
pub fn env_pane() -> Option<String> {
    std::env::var("TMUX_PANE").ok().filter(|s| !s.is_empty()).or_else(rex::env_pane)
}

/// Last `lines` lines of a pane's visible content.
pub fn capture(pane: &str, lines: usize) -> String {
    if is_rex(pane) { rex::capture(pane, Some(lines), false) } else { tmux::capture(pane, lines) }
}

/// Like `capture`, with ANSI colour codes.
pub fn capture_ansi(pane: &str, lines: usize) -> String {
    if is_rex(pane) { rex::capture(pane, Some(lines), true) } else { tmux::capture_ansi(pane, lines) }
}

/// Like `capture_ansi`, from the very start of the scrollback.
pub fn capture_ansi_all(pane: &str) -> String {
    if is_rex(pane) { rex::capture(pane, None, true) } else { tmux::capture_ansi_all(pane) }
}

/// Type `text` into a pane, literally.
pub fn send_text(pane: &str, text: &str) -> bool {
    if is_rex(pane) { rex::send_text(pane, text) } else { tmux::run(&["send-keys", "-t", pane, "-l", "--", text]) }
}

pub fn send_enter(pane: &str) -> bool {
    if is_rex(pane) { rex::send_enter(pane) } else { tmux::run(&["send-keys", "-t", pane, "Enter"]) }
}

/// A pane id short enough to show: tmux's `%12` as is, a Rex pane's block id suffix.
pub fn short_id(pane: &str) -> String {
    match rex::split(pane) {
        Some((_, b)) => format!("rex:{}", &b[b.len().saturating_sub(6)..]),
        None => pane.to_string(),
    }
}

/// A persistent dashboard hands the screen back to where the user came from (Rex only).
/// This runs the same Rex action that opens it, which toggles when it is already showing.
pub fn leave_dashboard(client: Option<&str>) -> Result<(), String> {
    let action = std::env::var("CC_HUD_REX_ACTION").unwrap_or_else(|_| "hud".into());
    rex::leave_dashboard(client, &action)
}

/// Is this the pane the user is looking at right now?
pub fn visible(pane: &str) -> bool {
    if is_rex(pane) { rex::visible(pane) } else { tmux::visible(pane) }
}

/// Show a short message in the multiplexer's UI. False if the active backend can't.
pub fn flash(text: &str) -> bool {
    match active() {
        Backend::Tmux => tmux::run(&["display-message", "-d", "5000", text]),
        Backend::Rex => false,
    }
}

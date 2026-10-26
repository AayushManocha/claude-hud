//! Everything the HUD needs from a terminal multiplexer, behind one trait.
//! Business logic (dashboard, lifecycle, notifications…) only talks to
//! [`get`]; `tmux.rs` and `zellij.rs` hold the backend-specific commands.
//!
//! "Window" means one workspace per worktree/chat: a tmux session, or a zellij tab.

mod tmux;
mod zellij;

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub session: String,
    pub window_id: String,
    pub window_name: String,
    /// `worktree` | `chat` (tagged on the session at creation; falls back to a `chat:` name prefix)
    pub kind: String,
    /// What a chat is about (tagged at creation); empty for older chats.
    pub topic: String,
}

impl PaneInfo {
    pub fn is_chat(&self) -> bool {
        self.kind == "chat" || self.window_name.starts_with("chat:")
    }
}

/// A tagged workspace with one pane: a detached tmux session, or a tab in a zellij session.
pub struct NewWindow<'a> {
    pub name: &'a str,
    pub cwd: &'a Path,
    pub kind: &'a str,
    pub repo: Option<&'a str>,
    pub topic: Option<&'a str>,
    pub launch: bool,
}

pub trait Multiplexer: Send + Sync {
    /// All live panes keyed by pane id, or None if the multiplexer isn't reachable.
    fn live_panes(&self) -> Option<HashMap<String, PaneInfo>>;
    /// Id of the pane this process runs in (same format as `live_panes` keys).
    fn current_pane(&self) -> Option<String>;
    /// Is this pane the one the user is looking at right now?
    fn is_visible(&self, pane_id: &str) -> bool;

    /// Focus a pane. `client` is the multiplexer client to switch, when known.
    fn jump(&self, client: Option<&str>, p: &PaneInfo, pane_id: &str);

    /// Last `lines` lines of a pane's visible content, for the peek view.
    fn capture(&self, pane_id: &str, lines: usize) -> String;
    /// Like `capture`, but keeps ANSI colour codes so the preview matches the pane.
    fn capture_ansi(&self, pane_id: &str, lines: usize) -> String;
    /// Like `capture_ansi`, but from the very start of the pane's scrollback.
    fn capture_ansi_all(&self, pane_id: &str) -> String;

    /// Type `text` literally into a pane (no Enter).
    fn send_text(&self, pane_id: &str, text: &str) -> bool;
    fn send_enter(&self, pane_id: &str) -> bool;

    /// Create a tagged, detached workspace, start `claude` in it if asked, and switch to it.
    fn open_window(&self, client: Option<&str>, w: &NewWindow) -> Result<()>;
    /// Switch to the workspace tagged for this worktree path; false if there is none.
    fn focus_worktree(&self, client: Option<&str>, path: &Path) -> bool;
    /// Close the workspace tagged for this worktree path, if any.
    fn kill_worktree(&self, path: &Path);
    /// Close a chat's workspace (and the agent in it).
    fn kill_chat(&self, p: &PaneInfo) -> bool;
    /// Unix seconds of last activity per tagged worktree path.
    fn session_activity(&self) -> HashMap<PathBuf, u64>;

    /// A user setting (`@claude-hud-…`) stored in the multiplexer's own config, if any.
    fn option(&self, name: &str) -> Option<String>;
    /// Flash a message in the multiplexer UI. Returns false only if it has no such feature
    /// (not if there was nobody to show it to).
    fn message(&self, text: &str, ms: u32) -> bool;
    /// Show a status glyph (or clear it) on each window, if the multiplexer can.
    fn set_window_glyphs(&self, glyphs: &[(String, Option<&'static str>)]);

    /// Replace this process with a client attached to (or creating) the workspace session.
    fn attach_workspace(&self, session: &str, root: &Path) -> std::io::Error;
}

/// The backend in use: `CC_HUD_MUX=tmux|zellij`, else whichever we are running inside
/// (tmux first), else tmux.
pub fn get() -> &'static dyn Multiplexer {
    static MUX: OnceLock<Box<dyn Multiplexer>> = OnceLock::new();
    MUX.get_or_init(|| {
        let forced = std::env::var("CC_HUD_MUX").unwrap_or_default();
        let in_zellij = std::env::var_os("ZELLIJ").is_some();
        let in_tmux = std::env::var_os("TMUX").is_some();
        match forced.as_str() {
            "zellij" => Box::new(zellij::Zellij),
            "tmux" => Box::new(tmux::Tmux),
            _ if in_zellij && !in_tmux => Box::new(zellij::Zellij),
            _ => Box::new(tmux::Tmux),
        }
    })
    .as_ref()
}

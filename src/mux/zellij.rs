use super::{Multiplexer, NewWindow, PaneInfo};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Zellij;

impl Multiplexer for Zellij {
    fn live_panes(&self) -> Option<HashMap<String, PaneInfo>> { None }
    fn current_pane(&self) -> Option<String> { None }
    fn is_visible(&self, _: &str) -> bool { false }
    fn jump(&self, _: Option<&str>, _: &PaneInfo, _: &str) {}
    fn capture(&self, _: &str, _: usize) -> String { String::new() }
    fn capture_ansi(&self, _: &str, _: usize) -> String { String::new() }
    fn capture_ansi_all(&self, _: &str) -> String { String::new() }
    fn send_text(&self, _: &str, _: &str) -> bool { false }
    fn send_enter(&self, _: &str) -> bool { false }
    fn open_window(&self, _: Option<&str>, _: &NewWindow) -> Result<()> { bail!("zellij: not implemented") }
    fn focus_worktree(&self, _: Option<&str>, _: &Path) -> bool { false }
    fn kill_worktree(&self, _: &Path) {}
    fn kill_session(&self, _: &str) -> bool { false }
    fn session_activity(&self) -> HashMap<PathBuf, u64> { HashMap::new() }
    fn option(&self, _: &str) -> Option<String> { None }
    fn message(&self, _: &str, _: u32) -> bool { false }
    fn set_window_glyphs(&self, _: &[(String, Option<&'static str>)]) {}
    fn attach_workspace(&self, _: &str, _: &Path) -> std::io::Error { std::io::Error::other("not implemented") }
}

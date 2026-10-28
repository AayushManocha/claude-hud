use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    NeedsInput,
    Idle,
}

impl Status {
    pub fn glyph(self) -> &'static str {
        match self {
            Status::NeedsInput => "⚠",
            Status::Working => "●",
            Status::Idle => "○",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::NeedsInput => "needs input",
            Status::Working => "working",
            Status::Idle => "idle",
        }
    }
}

/// One record per tmux or Rex pane running a Claude Code agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub pane_id: String,
    pub session_id: String,
    pub cwd: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub status: Status,
    /// What the agent is working on: first line of the latest prompt.
    pub task: Option<String>,
    /// Current tool call, e.g. "Edit src/foo.ts".
    pub tool: Option<String>,
    /// Pending notification text (permission prompt, question).
    pub message: Option<String>,
    /// Unix seconds when `status` last changed.
    pub since: u64,
    pub updated: u64,
    /// Top of the worktree the agent works in; agents sharing it are shown together.
    #[serde(default)]
    pub root: Option<String>,
    /// Unix seconds when this session's record was created; orders agents in a worktree (#1, #2…).
    #[serde(default)]
    pub started: u64,
    /// Files this session has edited or written, oldest first.
    #[serde(default)]
    pub edits: Vec<String>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CC_HUD_STATE_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(d).join("cc-hud");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/state/cc-hud")
}

fn path_for(pane_id: &str) -> PathBuf {
    // "%12" -> "12.json"; a Rex "session:…/block:…" keeps its characters that are safe in a file name.
    let name: String = pane_id
        .trim_start_matches('%')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    state_dir().join(format!("{name}.json"))
}

pub fn load(pane_id: &str) -> Option<AgentState> {
    let data = std::fs::read(path_for(pane_id)).ok()?;
    serde_json::from_slice(&data).ok()
}

/// Atomic write: temp file in the same dir, then rename.
pub fn save(s: &AgentState) -> Result<()> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).context("creating state dir")?;
    let dest = path_for(&s.pane_id);
    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(s)?)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(())
}

pub fn remove(pane_id: &str) {
    let _ = std::fs::remove_file(path_for(pane_id));
}

/// Roles name a pane's job ("tests", "review"), so they belong to the pane and outlive the
/// session in it. Kept apart from the agent record, which the hooks rewrite.
fn role_path(pane_id: &str) -> PathBuf {
    let file = path_for(pane_id);
    state_dir().join("roles").join(file.file_name().unwrap_or_default()).with_extension("txt")
}

pub fn role(pane_id: &str) -> String {
    std::fs::read_to_string(role_path(pane_id)).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// An empty role clears it.
pub fn set_role(pane_id: &str, role: &str) -> Result<()> {
    let path = role_path(pane_id);
    let role = role.trim();
    if role.is_empty() {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    std::fs::create_dir_all(state_dir().join("roles")).context("creating roles dir")?;
    std::fs::write(path, role)?;
    Ok(())
}

/// The pane is gone: drop its record and its role.
pub fn forget(pane_id: &str) {
    remove(pane_id);
    let _ = std::fs::remove_file(role_path(pane_id));
}

pub fn load_all() -> Vec<AgentState> {
    let Ok(rd) = std::fs::read_dir(state_dir()) else {
        return vec![];
    };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|d| serde_json::from_slice(&d).ok())
        .collect()
}

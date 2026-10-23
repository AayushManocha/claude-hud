use std::path::PathBuf;
use std::process::Command;

/// A tmux global option (`set -g @claude-hud-…`), if the server is reachable and it's set.
pub fn tmux_opt(name: &str) -> Option<String> {
    let out = Command::new("tmux").args(["show-option", "-gqv", name]).output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

fn setting(env: &str, opt: &str) -> Option<String> {
    std::env::var(env).ok().filter(|v| !v.is_empty()).or_else(|| tmux_opt(opt))
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

/// Directory scanned for repos (`@claude-hud-root`, default ~/Developer).
pub fn root() -> PathBuf {
    expand(&setting("CC_HUD_ROOT", "@claude-hud-root").unwrap_or_else(|| "~/Developer".into()))
}

/// Command typed into new windows (`@claude-hud-claude-cmd`).
pub fn claude_cmd() -> String {
    setting("CC_HUD_CLAUDE_CMD", "@claude-hud-claude-cmd").unwrap_or_else(|| "claude".into())
}

/// `off` | `tmux` | `system` | `both` (`@claude-hud-notify`, default tmux).
pub fn notify_mode() -> String {
    setting("CC_HUD_NOTIFY", "@claude-hud-notify").unwrap_or_else(|| "tmux".into())
}

/// Repo names pinned in tmux config (`@claude-hud-projects`, comma-separated).
pub fn config_pins() -> Vec<String> {
    setting("CC_HUD_PROJECTS", "@claude-hud-projects")
        .map(|v| v.split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect())
        .unwrap_or_default()
}

fn pins_file() -> PathBuf {
    crate::state::state_dir().join("pinned")
}

/// Repo names pinned from the dashboard (one per line).
pub fn saved_pins() -> Vec<String> {
    std::fs::read_to_string(pins_file())
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

pub fn save_pins(pins: &[String]) -> std::io::Result<()> {
    let f = pins_file();
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(f, pins.join("\n") + "\n")
}

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

/// Repo names always shown, even without agents (`@claude-hud-projects`, comma-separated).
pub fn projects() -> Vec<String> {
    setting("CC_HUD_PROJECTS", "@claude-hud-projects")
        .map(|v| v.split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect())
        .unwrap_or_default()
}

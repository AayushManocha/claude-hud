use crate::state::AgentState;
use crate::{config, tmux};
use std::process::Command;

/// Is this pane the one the user is looking at right now?
fn visible(pane: &str) -> bool {
    Command::new("tmux")
        .args(["display-message", "-p", "-t", pane, "#{window_active} #{session_attached} #{pane_active}"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1 1 1")
}

fn applescript_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Called once when an agent starts waiting for you, unless you're already looking at it.
pub fn needs_input(s: &AgentState) {
    let mode = config::notify_mode();
    if mode == "off" || visible(&s.pane_id) {
        return;
    }
    let who = match (&s.repo, &s.branch) {
        (Some(r), Some(b)) => format!("{r}/{b}"),
        (Some(r), None) => r.clone(),
        _ => s.cwd.rsplit('/').next().unwrap_or("agent").to_string(),
    };
    let what = s.message.as_deref().unwrap_or("needs input");
    let text = format!("⚠ {who}: {what}");
    if matches!(mode.as_str(), "tmux" | "both") {
        tmux::run(&["display-message", "-d", "5000", &text]);
    }
    if matches!(mode.as_str(), "system" | "both") {
        let script = format!(
            "display notification \"{}\" with title \"cc-hud\"",
            applescript_str(&format!("{who}: {what}"))
        );
        let _ = Command::new("osascript").args(["-e", &script]).output();
    }
}

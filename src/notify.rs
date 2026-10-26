use crate::state::AgentState;
use crate::{config, mux};
use std::process::Command;

fn applescript_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Called once when an agent starts waiting for you, unless you're already looking at it.
pub fn needs_input(s: &AgentState) {
    let mode = config::notify_mode();
    if mode == "off" || mux::get().is_visible(&s.pane_id) {
        return;
    }
    let who = match (&s.repo, &s.branch) {
        (Some(r), Some(b)) => format!("{r}/{b}"),
        (Some(r), None) => r.clone(),
        _ => s.cwd.rsplit('/').next().unwrap_or("agent").to_string(),
    };
    let what = s.message.as_deref().unwrap_or("needs input");
    let text = format!("⚠ {who}: {what}");
    // "tmux" is the historical name for "in the multiplexer"; fall back to a system
    // notification when the multiplexer has no way to flash a message.
    let shown = matches!(mode.as_str(), "tmux" | "both") && mux::get().message(&text, 5000);
    if matches!(mode.as_str(), "system" | "both") || (mode == "tmux" && !shown) {
        let script = format!(
            "display notification \"{}\" with title \"cc-hud\"",
            applescript_str(&format!("{who}: {what}"))
        );
        let _ = Command::new("osascript").args(["-e", &script]).output();
    }
}

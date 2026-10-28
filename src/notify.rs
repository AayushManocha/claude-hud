use crate::state::AgentState;
use crate::{config, mux};
use std::process::Command;

fn applescript_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Called once when an agent starts waiting for you, unless you're already looking at it.
pub fn needs_input(s: &AgentState) {
    let mode = config::notify_mode();
    if mode == "off" || mux::visible(&s.pane_id) {
        return;
    }
    let who = match (&s.repo, &s.branch) {
        (Some(r), Some(b)) => format!("{r}/{b}"),
        (Some(r), None) => r.clone(),
        _ => s.cwd.rsplit('/').next().unwrap_or("agent").to_string(),
    };
    let role = crate::state::role(&s.pane_id);
    let who = if role.is_empty() { who } else { format!("{who} ({role})") };
    let what = s.message.as_deref().unwrap_or("needs input");
    let text = format!("⚠ {who}: {what}");
    // A backend with no message line of its own (Rex) falls back to a system notification.
    let flashed = matches!(mode.as_str(), "tmux" | "both") && mux::flash(&text);
    if matches!(mode.as_str(), "system" | "both") || (mode != "off" && !flashed) {
        let script = format!(
            "display notification \"{}\" with title \"cc-hud\"",
            applescript_str(&format!("{who}: {what}"))
        );
        let _ = Command::new("osascript").args(["-e", &script]).output();
    }
}

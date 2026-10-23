use crate::state::{self, AgentState, Status};
use crate::tmux;
use std::collections::HashMap;

/// needs_input > working > idle, so a window shows its most urgent agent.
fn rank(s: Status) -> u8 {
    match s {
        Status::NeedsInput => 2,
        Status::Working => 1,
        Status::Idle => 0,
    }
}

/// Agents whose pane is still alive; prunes records for dead panes.
pub fn live_agents(live: &HashMap<String, tmux::PaneInfo>) -> Vec<AgentState> {
    state::load_all()
        .into_iter()
        .filter(|a| {
            let alive = live.contains_key(&a.pane_id);
            if !alive {
                state::remove(&a.pane_id);
            }
            alive
        })
        .collect()
}

/// Mirror agent status onto per-window `@hud_glyph` options so
/// `window-status-format` can show it, then redraw the status line.
pub fn run() {
    let Some(live) = tmux::live_panes() else { return };
    let agents = live_agents(&live);

    let mut worst: HashMap<&str, Status> = HashMap::new();
    for a in &agents {
        let w = live[&a.pane_id].window_id.as_str();
        let e = worst.entry(w).or_insert(a.status);
        if rank(a.status) > rank(*e) {
            *e = a.status;
        }
    }

    let mut windows: Vec<&str> = live.values().map(|p| p.window_id.as_str()).collect();
    windows.sort_unstable();
    windows.dedup();

    let mut args: Vec<&str> = vec![];
    for w in windows {
        if !args.is_empty() {
            args.push(";");
        }
        match worst.get(w) {
            Some(s) => args.extend(["set-option", "-w", "-t", w, "@hud_glyph", s.glyph()]),
            None => args.extend(["set-option", "-wu", "-t", w, "@hud_glyph"]),
        }
    }
    if !args.is_empty() {
        tmux::run(&args);
    }
    tmux::run(&["refresh-client", "-S"]);
}

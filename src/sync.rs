use crate::state::{self, AgentState, Status};
use crate::{mux, tmux};
use std::collections::HashMap;

/// needs_input > working > idle, so a window shows its most urgent agent.
fn rank(s: Status) -> u8 {
    match s {
        Status::NeedsInput => 2,
        Status::Working => 1,
        Status::Idle => 0,
    }
}

/// Agents whose pane is still alive; prunes records for dead panes
/// (never those of a multiplexer that wasn't reachable, since they may well be alive).
pub fn live_agents(live: &HashMap<String, tmux::PaneInfo>) -> Vec<AgentState> {
    agents_in(live, true)
}

/// Like `live_agents`, but leaves records alone. For a `live` listing that may be a moment
/// old: a pane that started since isn't in it, and its record must not be mistaken for a dead one's.
pub fn known_agents(live: &HashMap<String, tmux::PaneInfo>) -> Vec<AgentState> {
    agents_in(live, false)
}

fn agents_in(live: &HashMap<String, tmux::PaneInfo>, prune: bool) -> Vec<AgentState> {
    state::load_all()
        .into_iter()
        .filter(|a| {
            let alive = live.contains_key(&a.pane_id) || !mux::reachable(&a.pane_id);
            if !alive && prune {
                state::remove(&a.pane_id);
            }
            alive
        })
        .collect()
}

/// Mirror agent status onto per-window `@hud_glyph` options so
/// `window-status-format` can show it, then redraw the status line.
/// tmux only: Rex has no window format to put a glyph in.
pub fn run() {
    let Some(live) = mux::live_panes() else { return };
    let agents: Vec<_> = live_agents(&live).into_iter().filter(|a| !mux::is_rex(&a.pane_id)).collect();
    let live: HashMap<_, _> = live.into_iter().filter(|(id, _)| !mux::is_rex(id)).collect();
    if live.is_empty() {
        return;
    }

    let mut worst: HashMap<&str, Status> = HashMap::new();
    for a in &agents {
        let Some(p) = live.get(&a.pane_id) else { continue };
        let w = p.window_id.as_str();
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

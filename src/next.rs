use crate::{mux, sync};
use crate::state::Status;
use anyhow::{Context, Result};

/// Jump to the next agent that needs input, longest-waiting first,
/// cycling onward from `from` (the invoking pane) when it is itself waiting.
fn say(msg: &str) {
    if !mux::flash(msg) {
        println!("{msg}");
    }
}

pub fn run(from: Option<String>) -> Result<()> {
    let from = from.or_else(mux::env_pane);
    let live = mux::live_panes().context("no tmux or Rex server reachable")?;
    let mut waiting: Vec<_> = sync::live_agents(&live)
        .into_iter()
        .filter(|a| a.status == Status::NeedsInput && live.contains_key(&a.pane_id))
        .collect();
    waiting.sort_by_key(|a| (a.since, a.pane_id.clone()));

    if waiting.is_empty() {
        say("cc-hud: no agents need input");
        return Ok(());
    }
    let start = from
        .as_ref()
        .and_then(|f| waiting.iter().position(|a| &a.pane_id == f))
        .map_or(0, |i| i + 1);
    let target = &waiting[start % waiting.len()];
    if Some(&target.pane_id) == from.as_ref() {
        say("cc-hud: this is the only agent needing input");
        return Ok(());
    }
    let p = &live[&target.pane_id];
    mux::jump(None, p, &target.pane_id).map_err(anyhow::Error::msg)
}

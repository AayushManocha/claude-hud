use crate::{mux, sync};
use crate::state::Status;
use anyhow::{Context, Result};

/// Jump to the next agent that needs input, longest-waiting first,
/// cycling onward from `from` (the invoking pane) when it is itself waiting.
pub fn run(from: Option<String>) -> Result<()> {
    let live = mux::get().live_panes().context("multiplexer not reachable")?;
    let mut waiting: Vec<_> = sync::live_agents(&live)
        .into_iter()
        .filter(|a| a.status == Status::NeedsInput)
        .collect();
    waiting.sort_by_key(|a| (a.since, a.pane_id.clone()));

    if waiting.is_empty() {
        mux::get().message("cc-hud: no agents need input", 3000);
        return Ok(());
    }
    let start = from
        .as_ref()
        .and_then(|f| waiting.iter().position(|a| &a.pane_id == f))
        .map_or(0, |i| i + 1);
    let target = &waiting[start % waiting.len()];
    if Some(&target.pane_id) == from.as_ref() {
        mux::get().message("cc-hud: this is the only agent needing input", 3000);
        return Ok(());
    }
    let p = &live[&target.pane_id];
    mux::get().jump(None, p, &target.pane_id);
    Ok(())
}

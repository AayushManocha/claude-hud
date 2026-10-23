use crate::state::Status;
use crate::{sync, tmux};

/// One-line summary for `#(cc-hud status)` in the tmux status bar.
/// Also refreshes the per-window glyphs, since tmux polls this every
/// `status-interval` and that covers agents that died without a hook.
pub fn run(color: bool) {
    let Some(live) = tmux::live_panes() else { return };
    sync::run();
    let agents = sync::live_agents(&live);

    let parts: Vec<String> = [
        (Status::NeedsInput, "red"),
        (Status::Working, "yellow"),
        (Status::Idle, "green"),
    ]
    .iter()
    .filter_map(|&(st, c)| {
        let n = agents.iter().filter(|a| a.status == st).count();
        (n > 0).then(|| {
            if color {
                format!("#[fg={c}]{}{n}#[default]", st.glyph())
            } else {
                format!("{}{n}", st.glyph())
            }
        })
    })
    .collect();
    if !parts.is_empty() {
        println!("{}", parts.join(" "));
    }
}

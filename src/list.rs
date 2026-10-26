use crate::{mux, state, sync};
use anyhow::Result;

fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

pub fn run(json: bool) -> Result<()> {
    let live = mux::live_panes();
    let mut agents = state::load_all();

    if let Some(live) = &live {
        agents = sync::live_agents(live);
    }
    agents.sort_by(|a, b| {
        (a.repo.as_deref(), a.branch.as_deref(), &a.pane_id)
            .cmp(&(b.repo.as_deref(), b.branch.as_deref(), &b.pane_id))
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&agents)?);
        return Ok(());
    }
    let now = state::now();
    for a in &agents {
        let loc = live
            .as_ref()
            .and_then(|l| l.get(&a.pane_id))
            .map(|p| format!("{}:{}", p.session, p.window_name))
            .unwrap_or_default();
        let what = a
            .message
            .as_deref()
            .or(a.tool.as_deref())
            .or(a.task.as_deref())
            .unwrap_or("");
        println!(
            "{} {:<11} {:<14} {:<18} {:<8} {:>4}  {}",
            a.status.glyph(),
            a.status.label(),
            a.repo.as_deref().unwrap_or("-"),
            a.branch.as_deref().unwrap_or("-"),
            loc,
            age(now.saturating_sub(a.since)),
            what
        );
    }
    Ok(())
}

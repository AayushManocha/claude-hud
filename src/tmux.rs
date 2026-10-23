use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub session: String,
    pub window_id: String,
    pub window_name: String,
    /// `worktree` | `chat` (option `@hud_kind`, set on the session; falls back to a `chat:` name prefix)
    pub kind: String,
}

/// Run one tmux invocation; extra commands are chained with a `;` argument.
pub fn run(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// All live panes keyed by pane id, or None if tmux isn't reachable.
pub fn live_panes() -> Option<HashMap<String, PaneInfo>> {
    let out = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{session_name}\t#{window_id}\t#{window_name}\t#{@hud_kind}",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let map = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((
                f.next()?.to_string(),
                PaneInfo {
                    session: f.next()?.to_string(),
                    window_id: f.next()?.to_string(),
                    window_name: f.next()?.to_string(),
                    kind: f.next().unwrap_or("").to_string(),
                },
            ))
        })
        .collect();
    Some(map)
}

impl PaneInfo {
    pub fn is_chat(&self) -> bool {
        self.kind == "chat" || self.window_name.starts_with("chat:")
    }
}

/// Focus a pane. Separate invocations because switch-client fails when no
/// client is attached and would otherwise abort a chained command.
pub fn jump(client: Option<&str>, p: &PaneInfo, pane_id: &str) {
    match client {
        Some(c) => run(&["switch-client", "-c", c, "-t", &p.session]),
        None => run(&["switch-client", "-t", &p.session]),
    };
    run(&["select-window", "-t", &p.window_id]);
    run(&["select-pane", "-t", pane_id]);
}

/// Last `lines` lines of a pane's visible content, for the peek view.
pub fn capture(pane_id: &str, lines: usize) -> String {
    let start = format!("-{lines}");
    Command::new("tmux")
        .args(["capture-pane", "-p", "-t", pane_id, "-S", &start])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Like `capture`, but keeps ANSI colour codes (`-e`) so the preview matches the pane.
pub fn capture_ansi(pane_id: &str, lines: usize) -> String {
    let start = format!("-{lines}");
    Command::new("tmux")
        .args(["capture-pane", "-p", "-e", "-t", pane_id, "-S", &start])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

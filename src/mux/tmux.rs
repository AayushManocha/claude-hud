use super::{Multiplexer, NewWindow, PaneInfo};
use crate::config;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Tmux;

/// Run one tmux invocation; extra commands are chained with a `;` argument.
fn run(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn out(args: &[&str]) -> Option<String> {
    let o = Command::new("tmux").args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn capture_raw(args: &[&str]) -> String {
    Command::new("tmux")
        .arg("capture-pane")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// tmux session names can't contain `.` or `:`.
fn session_name(name: &str) -> String {
    name.replace(['.', ':'], "-")
}

/// `base`, or `base-2`, `base-3`… if a session by that name already exists.
fn unique_session(base: &str) -> String {
    let mut name = base.to_string();
    let mut n = 1;
    while run(&["has-session", "-t", &format!("={name}")]) {
        n += 1;
        name = format!("{base}-{n}");
    }
    name
}

/// Focus a session's window. Separate invocations because switch-client fails
/// when no client is attached and would otherwise abort a chained command.
fn focus(client: Option<&str>, session: &str, window_id: &str) {
    match client {
        Some(c) => run(&["switch-client", "-c", c, "-t", session]),
        None => run(&["switch-client", "-t", session]),
    };
    run(&["select-window", "-t", window_id]);
}

/// Session tagged (at session level) for this worktree path, if any.
/// `#{@hud_path}` in a format would also match a legacy window-level tag via the
/// session's active window, so read the session's own option instead.
fn find_session(path: &Path) -> Option<String> {
    let want = path.display().to_string();
    out(&["list-sessions", "-F", "#{session_id}"])?.lines().find_map(|id| {
        let p = out(&["show-options", "-qv", "-t", id, "@hud_path"])?;
        (p == want).then(|| id.to_string())
    })
}

impl Multiplexer for Tmux {
    fn live_panes(&self) -> Option<HashMap<String, PaneInfo>> {
        let out = Command::new("tmux")
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}\t#{session_name}\t#{window_id}\t#{window_name}\t#{@hud_kind}\t#{@hud_topic}",
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
                        topic: f.next().unwrap_or("").to_string(),
                    },
                ))
            })
            .collect();
        Some(map)
    }

    fn current_pane(&self) -> Option<String> {
        std::env::var("TMUX_PANE").ok()
    }

    fn is_visible(&self, pane: &str) -> bool {
        Command::new("tmux")
            .args(["display-message", "-p", "-t", pane, "#{window_active} #{session_attached} #{pane_active}"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1 1 1")
    }

    fn jump(&self, client: Option<&str>, p: &PaneInfo, pane_id: &str) {
        focus(client, &p.session, &p.window_id);
        run(&["select-pane", "-t", pane_id]);
    }

    fn capture(&self, pane_id: &str, lines: usize) -> String {
        capture_raw(&["-p", "-t", pane_id, "-S", &format!("-{lines}")])
    }

    fn capture_ansi(&self, pane_id: &str, lines: usize) -> String {
        capture_raw(&["-p", "-e", "-t", pane_id, "-S", &format!("-{lines}")])
    }

    fn capture_ansi_all(&self, pane_id: &str) -> String {
        capture_raw(&["-p", "-e", "-t", pane_id, "-S", "-"])
    }

    fn send_text(&self, pane_id: &str, text: &str) -> bool {
        run(&["send-keys", "-t", pane_id, "-l", "--", text])
    }

    fn send_enter(&self, pane_id: &str) -> bool {
        run(&["send-keys", "-t", pane_id, "Enter"])
    }

    fn open_window(&self, client: Option<&str>, w: &NewWindow) -> Result<()> {
        let name = unique_session(&session_name(w.name));
        let cwd = w.cwd.display().to_string();
        let created = out(&[
            "new-session", "-d", "-P", "-F", "#{session_id} #{window_id} #{pane_id}",
            "-s", &name, "-n", w.name, "-c", &cwd,
        ])
        .context("tmux new-session failed")?;
        let mut f = created.split(' ');
        let (sess, win, pane) = (f.next().unwrap_or(""), f.next().unwrap_or(""), f.next().unwrap_or(""));
        if pane.is_empty() {
            bail!("unexpected tmux output");
        }

        // Titles come from our own tags; don't let tmux rename the window to the cwd.
        run(&["set-option", "-w", "-t", win, "automatic-rename", "off"]);
        run(&["rename-window", "-t", win, w.name]);
        // Session options: the `#{@hud_*}` formats resolve them for the session's panes.
        for (k, v) in [("@hud_kind", Some(w.kind)), ("@hud_repo", w.repo), ("@hud_topic", w.topic), ("@hud_path", Some(cwd.as_str()))] {
            if let Some(v) = v {
                run(&["set-option", "-t", sess, k, v]);
            }
        }
        if w.launch {
            run(&["send-keys", "-t", pane, &config::claude_cmd(), "Enter"]);
        }
        focus(client, sess, win);
        Ok(())
    }

    fn focus_worktree(&self, client: Option<&str>, path: &Path) -> bool {
        let Some(sess) = find_session(path) else { return false };
        let Some(win) = out(&["list-windows", "-t", &sess, "-F", "#{window_id}"]).and_then(|w| w.lines().next().map(String::from)) else {
            return false;
        };
        focus(client, &sess, &win);
        true
    }

    fn kill_worktree(&self, path: &Path) {
        if let Some(sess) = find_session(path) {
            run(&["kill-session", "-t", &sess]);
        }
    }

    fn kill_session(&self, name: &str) -> bool {
        run(&["kill-session", "-t", &format!("={name}")])
    }

    fn session_activity(&self) -> HashMap<PathBuf, u64> {
        let Some(out) = out(&["list-sessions", "-F", "#{@hud_path}\t#{session_activity}"]) else {
            return HashMap::new();
        };
        out.lines()
            .filter_map(|l| {
                let (p, t) = l.split_once('\t')?;
                Some((PathBuf::from(p), t.parse().ok()?))
            })
            .collect()
    }

    fn option(&self, name: &str) -> Option<String> {
        let out = Command::new("tmux").args(["show-option", "-gqv", name]).output().ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !v.is_empty()).then_some(v)
    }

    fn message(&self, text: &str, ms: u32) -> bool {
        run(&["display-message", "-d", &ms.to_string(), text]);
        true
    }

    fn set_window_glyphs(&self, glyphs: &[(String, Option<&'static str>)]) {
        let mut args: Vec<&str> = vec![];
        for (w, glyph) in glyphs {
            if !args.is_empty() {
                args.push(";");
            }
            match glyph {
                Some(g) => args.extend(["set-option", "-w", "-t", w, "@hud_glyph", g]),
                None => args.extend(["set-option", "-wu", "-t", w, "@hud_glyph"]),
            }
        }
        if !args.is_empty() {
            run(&args);
        }
        run(&["refresh-client", "-S"]);
    }

    fn attach_workspace(&self, session: &str, root: &Path) -> std::io::Error {
        use std::os::unix::process::CommandExt;
        Command::new("tmux").args(["new-session", "-A", "-s", session, "-c"]).arg(root).exec()
    }
}

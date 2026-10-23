use crate::config;
use crate::discover::{Repo, Worktree};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn tmux_out(args: &[&str]) -> Option<String> {
    let o = Command::new("tmux").args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().context("running git")?;
    if !o.status.success() {
        bail!("{}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub struct NewWindow<'a> {
    pub name: &'a str,
    pub cwd: &'a Path,
    pub kind: &'a str,
    pub repo: Option<&'a str>,
    pub topic: Option<&'a str>,
    pub launch: bool,
}

/// tmux session names can't contain `.` or `:`.
fn session_name(name: &str) -> String {
    name.replace(['.', ':'], "-")
}

/// `base`, or `base-2`, `base-3`… if a session by that name already exists.
fn unique_session(base: &str) -> String {
    let mut name = base.to_string();
    let mut n = 1;
    while crate::tmux::run(&["has-session", "-t", &format!("={name}")]) {
        n += 1;
        name = format!("{base}-{n}");
    }
    name
}

/// Create a tagged, detached session with one window, start `claude` in it
/// if asked, and switch the client to it.
pub fn open_window(client: Option<&str>, w: &NewWindow) -> Result<()> {
    let name = unique_session(&session_name(w.name));
    let cwd = w.cwd.display().to_string();
    let created = tmux_out(&[
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
    crate::tmux::run(&["set-option", "-w", "-t", win, "automatic-rename", "off"]);
    crate::tmux::run(&["rename-window", "-t", win, w.name]);
    // Session options: the `#{@hud_*}` formats resolve them for the session's panes.
    for (k, v) in [("@hud_kind", Some(w.kind)), ("@hud_repo", w.repo), ("@hud_topic", w.topic), ("@hud_path", Some(cwd.as_str()))] {
        if let Some(v) = v {
            crate::tmux::run(&["set-option", "-t", sess, k, v]);
        }
    }
    if w.launch {
        crate::tmux::run(&["send-keys", "-t", pane, &config::claude_cmd(), "Enter"]);
    }
    focus(client, sess, win);
    Ok(())
}

fn focus(client: Option<&str>, session: &str, window_id: &str) {
    match client {
        Some(c) => crate::tmux::run(&["switch-client", "-c", c, "-t", session]),
        None => crate::tmux::run(&["switch-client", "-t", session]),
    };
    crate::tmux::run(&["select-window", "-t", window_id]);
}

/// Session tagged (at session level) for this worktree path, if any.
/// `#{@hud_path}` in a format would also match a legacy window-level tag via the
/// session's active window, so read the session's own option instead.
fn find_session(path: &Path) -> Option<String> {
    let want = path.display().to_string();
    tmux_out(&["list-sessions", "-F", "#{session_id}"])?.lines().find_map(|id| {
        let p = tmux_out(&["show-options", "-qv", "-t", id, "@hud_path"])?;
        (p == want).then(|| id.to_string())
    })
}

/// The tagged session for this worktree path and its first window id.
fn find_window(path: &Path) -> Option<(String, String)> {
    let sess = find_session(path)?;
    let win = tmux_out(&["list-windows", "-t", &sess, "-F", "#{window_id}"])?.lines().next()?.to_string();
    Some((sess, win))
}

pub fn window_name(repo: &str, wt: &Worktree) -> String {
    format!("{repo}/{}", wt.branch.as_deref().unwrap_or("detached"))
}

/// Open (or switch to the existing session for) a worktree, as a plain shell.
pub fn open_worktree(client: Option<&str>, repo: &Repo, wt: &Worktree) -> Result<()> {
    if let Some((s, w)) = find_window(&wt.path) {
        focus(client, &s, &w);
        return Ok(());
    }
    open_window(client, &NewWindow {
        name: &window_name(&repo.name, wt),
        cwd: &wt.path,
        kind: "worktree",
        repo: Some(&repo.name),
        topic: None,
        launch: false,
    })
}

/// `<repo parent>/<repo>-wt/<branch with / → ->`
pub fn worktree_dir(repo: &Repo, branch: &str) -> PathBuf {
    let parent = repo.path.parent().unwrap_or(&repo.path);
    parent.join(format!("{}-wt", repo.name)).join(branch.replace('/', "-"))
}

pub fn new_worktree(client: Option<&str>, repo: &Repo, branch: &str, launch: bool) -> Result<String> {
    let branch = branch.trim();
    if branch.is_empty() || branch.contains(char::is_whitespace) {
        bail!("branch name must be non-empty and contain no spaces");
    }
    let dir = worktree_dir(repo, branch);
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    let dirs = dir.display().to_string();
    let exists = git(&repo.path, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
    if exists {
        git(&repo.path, &["worktree", "add", &dirs, branch])?;
    } else {
        git(&repo.path, &["worktree", "add", "-b", branch, &dirs])?;
    }
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    open_window(client, &NewWindow {
        name: &format!("{}/{branch}", repo.name),
        cwd: &dir,
        kind: "worktree",
        repo: Some(&repo.name),
        topic: None,
        launch,
    })?;
    Ok(format!("worktree {} on {branch}", dir.display()))
}

/// Remove a linked worktree's checkout and close its tmux session. The branch is kept.
/// Git goes first: it refuses a dirty or locked worktree, and then nothing is closed.
pub fn remove_worktree(repo: &Repo, wt: &Worktree) -> Result<String> {
    if wt.path == repo.path {
        bail!("refusing to remove the main checkout of {}", repo.name);
    }
    git(&repo.path, &["worktree", "remove", &wt.path.display().to_string()])?;
    if let Some(sess) = find_session(&wt.path) {
        crate::tmux::run(&["kill-session", "-t", &sess]);
    }
    Ok(format!("removed {}", wt.path.display()))
}

/// Kill a chat's tmux session (and the agent in it).
pub fn kill_chat(session: &str) -> Result<String> {
    if !crate::tmux::run(&["kill-session", "-t", &format!("={session}")]) {
        bail!("couldn't kill session {session}");
    }
    Ok(format!("killed {session}"))
}

pub fn new_chat(client: Option<&str>, topic: &str, cwd: &Path) -> Result<String> {
    let topic = topic.trim();
    if topic.is_empty() {
        bail!("chat needs a topic");
    }
    open_window(client, &NewWindow {
        name: &format!("chat:{topic}"),
        cwd,
        kind: "chat",
        repo: None,
        topic: Some(topic),
        launch: true,
    })?;
    Ok(format!("chat:{topic}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
    }

    #[test]
    fn removes_linked_worktree_but_not_main() {
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("cchud-rm-{}", std::process::id()));
        let main = tmp.join("proj");
        std::fs::create_dir_all(&main).unwrap();
        sh(&main, &["init", "-q"]);
        sh(&main, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "i"]);
        let repo = Repo { name: "proj".into(), path: main.clone(), worktrees: vec![] };
        let dir = worktree_dir(&repo, "feat");
        sh(&main, &["worktree", "add", "-q", "-b", "feat", &dir.display().to_string()]);
        let wt = Worktree { path: std::fs::canonicalize(&dir).unwrap(), branch: Some("feat".into()) };

        // Dirty worktrees are refused and left intact.
        std::fs::write(dir.join("junk"), "x").unwrap();
        assert!(remove_worktree(&repo, &wt).is_err());
        assert!(dir.exists());
        std::fs::remove_file(dir.join("junk")).unwrap();

        remove_worktree(&repo, &wt).unwrap();
        assert!(!dir.exists());
        let main_wt = Worktree { path: main.clone(), branch: None };
        assert!(remove_worktree(&repo, &main_wt).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

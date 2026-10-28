use crate::{config, mux, rex};
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
/// if asked, and switch the client to it. Returns the window's pane id.
pub fn open_window(client: Option<&str>, w: &NewWindow) -> Result<String> {
    match mux::active() {
        mux::Backend::Tmux => open_tmux_window(client, w),
        mux::Backend::Rex => {
            let tags = rex::Tags {
                kind: w.kind.into(),
                repo: w.repo.map(String::from),
                topic: w.topic.map(String::from),
                path: w.cwd.display().to_string(),
            };
            let launch = w.launch.then(config::claude_cmd);
            let (sid, wid, pane) = rex::open_window(w.name, w.cwd, &tags, launch.as_deref())?;
            rex::focus(client, &sid, &wid).map_err(|e| anyhow::anyhow!("opened {}, but {e}", w.name))?;
            Ok(pane)
        }
    }
}

fn open_tmux_window(client: Option<&str>, w: &NewWindow) -> Result<String> {
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
    Ok(pane.to_string())
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

/// Session ids are `session:…` in Rex, `$N` in tmux, so they say which backend they belong to.
fn is_rex_session(id: &str) -> bool {
    id.starts_with("session:")
}

/// An existing session for this worktree, in either backend.
fn find_any(path: &Path) -> Option<(String, String)> {
    find_window(path).or_else(|| rex::find_window(path))
}

pub fn window_name(repo: &str, wt: &Worktree) -> String {
    format!("{repo}/{}", wt.branch.as_deref().unwrap_or("detached"))
}

/// Open (or switch to the existing session for) a worktree, as a plain shell.
pub fn open_worktree(client: Option<&str>, repo: &Repo, wt: &Worktree) -> Result<()> {
    if let Some((s, w)) = find_any(&wt.path) {
        if is_rex_session(&s) {
            return rex::focus(client, &s, &w).map_err(anyhow::Error::msg);
        }
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
    })?;
    Ok(())
}

/// Where another agent goes: beside an agent's pane, or into a worktree (its session's
/// window if it has one, else a new session).
pub enum Spot<'a> {
    Pane(&'a str, &'a crate::tmux::PaneInfo),
    Worktree(&'a Repo, &'a Worktree),
}

/// Start another `claude` at `spot`, working in `cwd`, give it `role`, and show it.
pub fn add_agent(client: Option<&str>, spot: Spot, cwd: &Path, role: &str) -> Result<String> {
    let pane = match spot {
        Spot::Pane(anchor, p) => {
            let pane = split_terminal(Some(anchor), None, cwd)?;
            launch_in(&pane);
            mux::jump(client, p, &pane).map_err(anyhow::Error::msg)?;
            pane
        }
        Spot::Worktree(repo, wt) => match find_any(&wt.path) {
            Some((s, w)) => {
                let pane = split_terminal(None, Some((&s, &w)), cwd)?;
                launch_in(&pane);
                if is_rex_session(&s) {
                    rex::focus(client, &s, &w).map_err(anyhow::Error::msg)?;
                } else {
                    focus(client, &s, &w);
                }
                pane
            }
            None => open_window(client, &NewWindow {
                name: &window_name(&repo.name, wt),
                cwd: &wt.path,
                kind: "worktree",
                repo: Some(&repo.name),
                topic: None,
                launch: true,
            })?,
        },
    };
    crate::state::set_role(&pane, role)?;
    let role = role.trim();
    Ok(if role.is_empty() { format!("new agent in {}", cwd.display()) } else { format!("new {role} agent in {}", cwd.display()) })
}

/// Split a terminal off `anchor` (a pane id), else off `window` (a session and its window).
fn split_terminal(anchor: Option<&str>, window: Option<(&str, &str)>, cwd: &Path) -> Result<String> {
    if let Some((sid, bid)) = anchor.and_then(rex::split) {
        return rex::split_terminal(sid, Some(bid), None, cwd);
    }
    if let Some((sid, wid)) = window.filter(|(s, _)| is_rex_session(s)) {
        return rex::split_terminal(sid, None, Some(wid), cwd);
    }
    let target = anchor.or(window.map(|(_, w)| w)).context("nowhere to split")?;
    let cwd_s = cwd.display().to_string();
    let pane = tmux_out(&["split-window", "-t", target, "-c", &cwd_s, "-P", "-F", "#{pane_id}"]).context("tmux split-window failed")?;
    // Agents get an equal share of the window, however many there are.
    crate::tmux::run(&["select-layout", "-t", &pane, "tiled"]);
    Ok(pane)
}

fn launch_in(pane: &str) {
    mux::send_text(pane, &config::claude_cmd());
    mux::send_enter(pane);
}

/// Close one agent's pane, leaving the rest of its window alone.
pub fn kill_agent(pane: &str) -> Result<String> {
    let closed = if rex::is_pane(pane) { rex::close_pane(pane) } else { crate::tmux::run(&["kill-pane", "-t", pane]) };
    if !closed {
        bail!("couldn't close pane {}", mux::short_id(pane));
    }
    crate::state::forget(pane);
    Ok(format!("closed {}", mux::short_id(pane)))
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

/// Remove a linked worktree and close its tmux session. The branch is kept.
/// Checks go first (git refuses dirty or locked worktrees; then nothing is closed or moved).
/// The checkout is then renamed out of the way and git's record of it pruned, so the path
/// is free at once; deleting the files (`target/`, `node_modules/`…) is left to a detached
/// `rm -rf` that outlives the dashboard.
/// With `force`, uncommitted and untracked files in it are discarded.
pub fn remove_worktree(client: Option<&str>, repo: &Repo, wt: &Worktree, force: bool) -> Result<String> {
    if wt.path == repo.path {
        bail!("refusing to remove the main checkout of {}", repo.name);
    }
    let gitdir = git(&wt.path, &["rev-parse", "--absolute-git-dir"])?;
    if Path::new(&gitdir).join("locked").exists() {
        bail!("{} is locked; unlock it with git worktree unlock", wt.path.display());
    }
    if !force && !git(&wt.path, &["status", "--porcelain"])?.is_empty() {
        bail!("{} contains modified or untracked files, use --force to delete it", wt.path.display());
    }
    let name = wt.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let trash = wt.path.with_file_name(format!(".trash-{name}-{stamp}"));
    if let Some(sess) = find_session(&wt.path) {
        // Killing the session a client is attached to would detach it; move it elsewhere first.
        leave_session(client, &sess);
        crate::tmux::run(&["kill-session", "-t", &sess]);
    }
    if let Some((sess, _)) = rex::find_window(&wt.path) {
        rex::kill_session(&sess);
    }
    std::fs::rename(&wt.path, &trash).with_context(|| format!("moving {} aside", wt.path.display()))?;
    let pruned = git(&repo.path, &["worktree", "prune"]);
    purge(&trash);
    pruned?;
    Ok(format!("removed {}", wt.path.display()))
}

/// If `client` is attached to `session`, switch it to the most recently attached other
/// worktree session (else any other session). The dashboard popup belongs to the client, so
/// it stays open across the switch.
fn leave_session(client: Option<&str>, session: &str) {
    let mut cur = vec!["display-message", "-p"];
    if let Some(c) = client {
        cur.extend(["-c", c]);
    }
    cur.push("#{session_id}");
    if tmux_out(&cur).as_deref() != Some(session) {
        return;
    }
    let Some(list) = tmux_out(&["list-sessions", "-F", "#{session_id}\t#{session_last_attached}\t#{session_activity}\t#{@hud_kind}"]) else { return };
    let target = list
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let id = f.next()?;
            let last: u64 = f.next()?.parse().unwrap_or(0);
            let act: u64 = f.next()?.parse().unwrap_or(0);
            (id != session).then(|| (f.next() == Some("worktree"), last.max(act), id.to_string()))
        })
        .max();
    if let Some((_, _, id)) = target {
        match client {
            Some(c) => crate::tmux::run(&["switch-client", "-c", c, "-t", &id]),
            None => crate::tmux::run(&["switch-client", "-t", &id]),
        };
    }
}

/// Delete `dir` in a detached process, so it finishes even if the dashboard closes first.
fn purge(dir: &Path) {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let spawned = Command::new("nohup")
        .arg("rm")
        .arg("-rf")
        .arg(dir)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if spawned.is_err() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Kill a chat's session (and the agent in it); `session` is `PaneInfo::session_key`.
pub fn kill_chat(session: &str) -> Result<String> {
    let killed = if is_rex_session(session) {
        rex::kill_session(session)
    } else {
        crate::tmux::run(&["kill-session", "-t", &format!("={session}")])
    };
    if !killed {
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
        let wt = Worktree { path: std::fs::canonicalize(&dir).unwrap(), branch: Some("feat".into()), last_used: 0 };

        // Dirty worktrees are refused and left intact.
        std::fs::write(dir.join("junk"), "x").unwrap();
        let err = remove_worktree(None, &repo, &wt, false).unwrap_err().to_string();
        assert!(err.contains("--force"), "{err}");
        assert!(dir.exists());
        remove_worktree(None, &repo, &wt, true).unwrap();
        assert!(!dir.exists());
        let trash_left = |d: &Path| std::fs::read_dir(d).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".trash-"));
        let parent = dir.parent().unwrap();
        for _ in 0..100 {
            if !trash_left(parent) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!trash_left(parent));
        assert!(!git(&main, &["worktree", "list"]).unwrap().contains("/feat"));
        let main_wt = Worktree { path: main.clone(), branch: None, last_used: 0 };
        assert!(remove_worktree(None, &repo, &main_wt, true).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

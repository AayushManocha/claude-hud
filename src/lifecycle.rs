use crate::mux::{self, NewWindow, PaneInfo};
use crate::discover::{Repo, Worktree};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().context("running git")?;
    if !o.status.success() {
        bail!("{}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub fn window_name(repo: &str, wt: &Worktree) -> String {
    format!("{repo}/{}", wt.branch.as_deref().unwrap_or("detached"))
}

/// Open (or switch to the existing session for) a worktree, as a plain shell.
pub fn open_worktree(client: Option<&str>, repo: &Repo, wt: &Worktree) -> Result<()> {
    if mux::get().focus_worktree(client, &wt.path) {
        return Ok(());
    }
    mux::get().open_window(client, &NewWindow {
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
    mux::get().open_window(client, &NewWindow {
        name: &format!("{}/{branch}", repo.name),
        cwd: &dir,
        kind: "worktree",
        repo: Some(&repo.name),
        topic: None,
        launch,
    })?;
    Ok(format!("worktree {} on {branch}", dir.display()))
}

/// Remove a linked worktree and close its session. The branch is kept.
/// Checks go first (git refuses dirty or locked worktrees; then nothing is closed or moved).
/// The checkout is then renamed out of the way and git's record of it pruned, so the path
/// is free at once; deleting the files (`target/`, `node_modules/`…) is left to a detached
/// `rm -rf` that outlives the dashboard.
/// With `force`, uncommitted and untracked files in it are discarded.
pub fn remove_worktree(repo: &Repo, wt: &Worktree, force: bool) -> Result<String> {
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
    mux::get().kill_worktree(&wt.path);
    std::fs::rename(&wt.path, &trash).with_context(|| format!("moving {} aside", wt.path.display()))?;
    let pruned = git(&repo.path, &["worktree", "prune"]);
    purge(&trash);
    pruned?;
    Ok(format!("removed {}", wt.path.display()))
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

/// Kill a chat's session (and the agent in it).
pub fn kill_chat(p: &PaneInfo) -> Result<String> {
    if !mux::get().kill_chat(p) {
        bail!("couldn't kill chat {}", p.window_name);
    }
    Ok(format!("killed {}", p.window_name))
}

pub fn new_chat(client: Option<&str>, topic: &str, cwd: &Path) -> Result<String> {
    let topic = topic.trim();
    if topic.is_empty() {
        bail!("chat needs a topic");
    }
    mux::get().open_window(client, &NewWindow {
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
        let err = remove_worktree(&repo, &wt, false).unwrap_err().to_string();
        assert!(err.contains("--force"), "{err}");
        assert!(dir.exists());
        remove_worktree(&repo, &wt, true).unwrap();
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
        assert!(remove_worktree(&repo, &main_wt, true).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

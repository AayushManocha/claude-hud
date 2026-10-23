use std::path::Path;
use std::process::Command;

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn branch(cwd: &str) -> Option<String> {
    git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Repo name shared by a main checkout and all its worktrees: the parent
/// directory of the common git dir (`<repo>/.git`).
pub fn repo_name(cwd: &str) -> Option<String> {
    let common = git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Path::new(&common)
        .parent()?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

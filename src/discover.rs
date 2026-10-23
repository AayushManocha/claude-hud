use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub worktrees: Vec<Worktree>,
}

const SKIP: &[&str] = &["node_modules", "target", "vendor", "dist", "build"];

/// Main checkouts have a `.git` *directory*; worktree checkouts have a `.git`
/// *file* and are found through their main repo's `git worktree list`.
fn is_main_checkout(p: &Path) -> bool {
    p.join(".git").is_dir()
}

fn subdirs(p: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(p) else { return vec![] };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            !n.starts_with('.') && !SKIP.contains(&n.as_str())
        })
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

pub fn parse_worktrees(porcelain: &str) -> Vec<Worktree> {
    let mut out = vec![];
    for block in porcelain.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        let mut skip = false;
        for l in block.lines() {
            if let Some(p) = l.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = l.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if l == "bare" || l.starts_with("prunable") {
                skip = true;
            }
        }
        if let (Some(path), false) = (path, skip) {
            if path.exists() {
                out.push(Worktree { path: std::fs::canonicalize(&path).unwrap_or(path), branch });
            }
        }
    }
    out
}

fn load_repo(path: PathBuf) -> Option<Repo> {
    let out = Command::new("git").arg("-C").arg(&path).args(["worktree", "list", "--porcelain"]).output().ok()?;
    let worktrees = parse_worktrees(&String::from_utf8_lossy(&out.stdout));
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    Some(Repo { name: path.file_name()?.to_string_lossy().into_owned(), path, worktrees })
}

/// Repos up to two levels under `root` (`root/repo` and `root/group/repo`).
pub fn scan(root: &Path) -> Vec<Repo> {
    let mut found = vec![];
    for d in subdirs(root) {
        if is_main_checkout(&d) {
            found.extend(load_repo(d));
        } else {
            for d2 in subdirs(&d) {
                if is_main_checkout(&d2) {
                    found.extend(load_repo(d2));
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain() {
        let tmp = std::env::temp_dir();
        let t = tmp.display();
        let s = format!(
            "worktree {t}\nHEAD abc\nbranch refs/heads/main\n\nworktree {t}\nHEAD def\ndetached\n\nworktree /nonexistent/gone\nHEAD 1\nbranch refs/heads/x\nprunable gitdir file points to non-existent location\n"
        );
        let w = parse_worktrees(&s);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert_eq!(w[1].branch, None);
    }
}

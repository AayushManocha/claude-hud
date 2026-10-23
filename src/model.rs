use crate::discover::Repo;
use crate::state::{AgentState, Status};
use crate::tmux::PaneInfo;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Entry {
    pub a: AgentState,
    pub p: PaneInfo,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Key {
    Group(String),
    Agent(String),
    /// A discovered worktree with no agent, by path.
    Worktree(String),
}

pub enum Kind {
    Group { label: String, collapsed: bool, counts: [usize; 3] },
    /// `entry` indexes the entries slice; the label is shown once per window.
    Agent { entry: usize, show_label: bool },
    /// Discovered worktree nobody is running an agent in.
    Worktree { repo: usize, wt: usize },
}

pub struct Row {
    pub key: Key,
    pub kind: Kind,
}

pub const CHATS: &str = "chats";

pub fn group_of(e: &Entry) -> String {
    if e.p.is_chat() {
        CHATS.into()
    } else {
        e.a.repo.clone().unwrap_or_else(|| "(no repo)".into())
    }
}

fn urgency(s: Status) -> u8 {
    match s {
        Status::NeedsInput => 2,
        Status::Working => 1,
        Status::Idle => 0,
    }
}

/// Case-insensitive subsequence match: "authx" matches "api/auth-refactor".
pub fn fuzzy(query: &str, hay: &str) -> bool {
    let mut h = hay.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .all(|q| h.any(|c| c == q))
}

fn haystack(e: &Entry) -> String {
    format!(
        "{} {} {} {}",
        group_of(e),
        e.a.branch.as_deref().unwrap_or(""),
        e.p.window_name,
        e.a.task.as_deref().unwrap_or("")
    )
}

fn canon(p: &str) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p))
}

/// Is any agent working inside this worktree directory?
fn covered(wt: &Path, agent_dirs: &[PathBuf]) -> bool {
    agent_dirs.iter().any(|d| d.starts_with(wt))
}

#[derive(Default)]
struct Group {
    agents: Vec<usize>,
    repo: Option<usize>,
}

/// Rows for the dashboard. Repos appear only if they have an agent or are in
/// `pinned`; pinned repos with no agents start folded. `toggled` flips a group's default fold state.
/// Groups with a needs-input agent, and any active filter, force expansion.
/// Order: most urgent first, agent-less repos next, chats last.
pub fn build(entries: &[Entry], repos: &[Repo], pinned: &[String], toggled: &HashSet<String>, filter: &str) -> Vec<Row> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        if filter.is_empty() || fuzzy(filter, &haystack(e)) {
            groups.entry(group_of(e)).or_default().agents.push(i);
        }
    }
    let agent_dirs: Vec<PathBuf> = entries.iter().map(|e| canon(&e.a.cwd)).collect();
    let with_agents: HashSet<String> = entries.iter().map(group_of).collect();
    for (ri, r) in repos.iter().enumerate() {
        if !with_agents.contains(&r.name) && !pinned.contains(&r.name) {
            continue;
        }
        let matches = filter.is_empty()
            || fuzzy(filter, &r.name)
            || r.worktrees.iter().any(|w| fuzzy(filter, &format!("{} {}", r.name, w.branch.as_deref().unwrap_or(""))));
        if matches {
            groups.entry(r.name.clone()).or_default().repo = Some(ri);
        }
    }

    let top = |g: &Group| g.agents.iter().map(|&i| urgency(entries[i].a.status) as i8).max().unwrap_or(-1);
    let mut order: Vec<_> = groups.into_iter().collect();
    order.sort_by(|(an, a), (bn, b)| {
        (an == CHATS, std::cmp::Reverse(top(a)), an).cmp(&(bn == CHATS, std::cmp::Reverse(top(b)), bn))
    });

    let mut rows = vec![];
    for (name, g) in order {
        let mut counts = [0; 3];
        for &i in &g.agents {
            counts[2 - urgency(entries[i].a.status) as usize] += 1; // [needs, working, idle]
        }
        let default_folded = g.agents.is_empty();
        let collapsed = (default_folded != toggled.contains(&name)) && counts[0] == 0 && filter.is_empty();
        rows.push(Row {
            key: Key::Group(name.clone()),
            kind: Kind::Group { label: name, collapsed, counts },
        });
        if collapsed {
            continue;
        }
        let mut idx = g.agents.clone();
        idx.sort_by_key(|&i| {
            let e = &entries[i];
            (std::cmp::Reverse(urgency(e.a.status)), e.p.window_id.clone(), e.a.pane_id.clone())
        });
        // Keep a window's agents adjacent, ordered by its most urgent agent.
        let mut windows: Vec<&str> = vec![];
        for &i in &idx {
            let w = entries[i].p.window_id.as_str();
            if !windows.contains(&w) {
                windows.push(w);
            }
        }
        let mut last_window: Option<&str> = None;
        for w in windows {
            for &i in idx.iter().filter(|&&i| entries[i].p.window_id == w) {
                rows.push(Row {
                    key: Key::Agent(entries[i].a.pane_id.clone()),
                    kind: Kind::Agent { entry: i, show_label: last_window != Some(w) },
                });
                last_window = Some(w);
            }
        }
        if let Some(ri) = g.repo {
            for (wi, wt) in repos[ri].worktrees.iter().enumerate() {
                let listed = filter.is_empty()
                    || fuzzy(filter, &format!("{} {}", repos[ri].name, wt.branch.as_deref().unwrap_or("")));
                if listed && !covered(&wt.path, &agent_dirs) {
                    rows.push(Row {
                        key: Key::Worktree(wt.path.display().to_string()),
                        kind: Kind::Worktree { repo: ri, wt: wi },
                    });
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pane: &str, win: &str, repo: &str, branch: &str, st: Status, chat: bool) -> Entry {
        Entry {
            a: AgentState {
                pane_id: pane.into(), session_id: "s".into(), cwd: "/".into(),
                repo: Some(repo.into()), branch: Some(branch.into()), status: st,
                task: Some("t".into()), tool: None, message: None, since: 0, updated: 0,
            },
            p: PaneInfo {
                session: "dev".into(), window_id: win.into(), window_name: branch.into(),
                kind: if chat { "chat".into() } else { "".into() },
            },
        }
    }

    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.kind {
                Kind::Group { label, collapsed, .. } => format!("G:{label}{}", if *collapsed { "(folded)" } else { "" }),
                Kind::Agent { entry, .. } => format!("A:{entry}"),
                Kind::Worktree { wt, .. } => format!("W:{wt}"),
            })
            .collect()
    }

    #[test]
    fn fuzzy_subsequence() {
        assert!(fuzzy("authx", "api Auth-refactor x"));
        assert!(fuzzy("", "anything"));
        assert!(!fuzzy("zz", "api auth"));
    }

    #[test]
    fn urgent_groups_first_chats_last() {
        let es = vec![
            entry("%0", "@0", "aaa", "m", Status::Idle, false),
            entry("%1", "@1", "zzz", "m", Status::NeedsInput, false),
            entry("%2", "@2", "x", "c", Status::NeedsInput, true),
        ];
        let rows = build(&es, &[], &[], &HashSet::new(), "");
        assert_eq!(labels(&rows), ["G:zzz", "A:1", "G:aaa", "A:0", "G:chats", "A:2"]);
    }

    #[test]
    fn fold_unless_needs_input() {
        let es = vec![
            entry("%0", "@0", "a", "m", Status::Idle, false),
            entry("%1", "@1", "b", "m", Status::NeedsInput, false),
        ];
        let folded: HashSet<String> = ["a", "b"].map(String::from).into();
        let rows = build(&es, &[], &[], &folded, "");
        assert_eq!(labels(&rows), ["G:b", "A:1", "G:a(folded)"]);
    }

    #[test]
    fn filter_drops_groups_and_ignores_folds() {
        let es = vec![
            entry("%0", "@0", "api", "auth", Status::Idle, false),
            entry("%1", "@1", "web", "dark", Status::Idle, false),
        ];
        let folded: HashSet<String> = ["api"].map(String::from).into();
        assert_eq!(labels(&build(&es, &[], &[], &folded, "auth")), ["G:api", "A:0"]);
    }

    #[test]
    fn label_shown_once_per_window() {
        let es = vec![
            entry("%0", "@0", "a", "m", Status::Working, false),
            entry("%1", "@0", "a", "m", Status::Working, false),
        ];
        let rows = build(&es, &[], &[], &HashSet::new(), "");
        let shown: Vec<bool> = rows.iter().filter_map(|r| match r.kind {
            Kind::Agent { show_label, .. } => Some(show_label), _ => None }).collect();
        assert_eq!(shown, [true, false]);
    }

    fn repo(name: &str, paths: &[(&str, &str)]) -> Repo {
        Repo {
            name: name.into(),
            path: paths[0].0.into(),
            worktrees: paths
                .iter()
                .map(|(p, b)| crate::discover::Worktree { path: (*p).into(), branch: Some((*b).into()) })
                .collect(),
        }
    }

    #[test]
    fn agentless_repo_folded_until_toggled() {
        let repos = vec![repo("dots", &[("/nope/dots", "main")])];
        let pinned = vec!["dots".to_string()];
        let rows = build(&[], &repos, &pinned, &HashSet::new(), "");
        assert_eq!(labels(&rows), ["G:dots(folded)"]);
        let open: HashSet<String> = ["dots"].map(String::from).into();
        assert_eq!(labels(&build(&[], &repos, &pinned, &open, "")), ["G:dots", "W:0"]);
    }

    #[test]
    fn unpinned_agentless_repo_hidden() {
        let repos = vec![repo("dots", &[("/nope/dots", "main")])];
        assert!(build(&[], &repos, &[], &HashSet::new(), "").is_empty());
    }

    #[test]
    fn worktree_row_only_without_agent() {
        let mut e = entry("%0", "@0", "r", "a", Status::Idle, false);
        e.a.cwd = "/nope/r-wt/a/src".into(); // agent in a subdir of worktree a
        let repos = vec![repo("r", &[("/nope/r", "main"), ("/nope/r-wt/a", "a")])];
        let rows = build(&[e], &repos, &[], &HashSet::new(), "");
        assert_eq!(labels(&rows), ["G:r", "A:0", "W:0"]); // main has no agent, a is covered
    }
}


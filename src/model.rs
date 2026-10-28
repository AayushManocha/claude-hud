use crate::discover::Repo;
use crate::state::{AgentState, Status};
use crate::tmux::PaneInfo;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Entry {
    pub a: AgentState,
    pub p: PaneInfo,
    /// The pane's job ("tests", "review"), if the user gave it one.
    pub role: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Key {
    Group(String),
    /// A worktree or chat with more than one agent, by `unit_of`.
    Unit(String),
    Agent(String),
    /// A discovered worktree with no agent, by path.
    Worktree(String),
}

pub enum Kind {
    Group { label: String, collapsed: bool, counts: [usize; 3] },
    /// Header of a worktree or chat that has more than one agent. `members` index the
    /// entries slice in handle order (#1 first); `shared` are files more than one of them edited.
    Unit { label: String, members: Vec<usize>, collapsed: bool, shared: Vec<Touched> },
    /// `entry` indexes the entries slice; `slot` is set for an agent under a `Unit` header.
    Agent { entry: usize, slot: Option<Slot> },
    /// Discovered worktree nobody is running an agent in.
    Worktree { repo: usize, wt: usize },
}

pub struct Row {
    pub key: Key,
    pub kind: Kind,
}

/// An agent's place under its `Unit` header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot {
    /// Its handle: #1 is the agent that started first in the worktree.
    pub n: usize,
    /// Last row under the header (drawn with `╰`).
    pub last: bool,
}

/// A file edited by agents of one unit, shown relative to the worktree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Touched {
    pub file: String,
    /// Handles of the agents that edited it.
    pub by: Vec<usize>,
}

pub const CHATS: &str = "chats";

pub fn group_of(e: &Entry) -> String {
    if e.p.is_chat() {
        CHATS.into()
    } else {
        e.a.repo.clone().unwrap_or_else(|| "(no repo)".into())
    }
}

/// What a chat is about: the topic it was created with, else the agent's task,
/// else the session name. Never the window name, which tmux sets from the cwd.
pub fn chat_title(e: &Entry) -> String {
    let first = |s: &str| s.lines().next().unwrap_or("").trim().to_string();
    [first(&e.p.topic), e.a.task.as_deref().map(first).unwrap_or_default()]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| e.p.session.trim_start_matches("chat-").to_string())
}

/// Short row label: a chat's title, or a worktree's branch.
pub fn label(e: &Entry) -> String {
    if e.p.is_chat() {
        chat_title(e)
    } else {
        e.a.branch.clone().unwrap_or_else(|| e.p.window_name.clone())
    }
}

/// What agents share: a chat's session, else the worktree they work in. Records from before
/// worktree roots were kept fall back to the window.
pub fn unit_of(e: &Entry) -> String {
    if e.p.is_chat() {
        format!("chat:{}", e.p.session_key)
    } else if let Some(root) = &e.a.root {
        format!("wt:{root}")
    } else {
        format!("win:{}", e.p.window_id)
    }
}

/// The agents of every unit, in handle order: whoever started first is #1.
pub fn units(entries: &[Entry]) -> BTreeMap<String, Vec<usize>> {
    let mut m: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        m.entry(unit_of(e)).or_default().push(i);
    }
    for v in m.values_mut() {
        v.sort_by(|&a, &b| (entries[a].a.started, &entries[a].a.pane_id).cmp(&(entries[b].a.started, &entries[b].a.pane_id)));
    }
    m
}

/// "#2 tests", or "#2" with no role.
pub fn handle(n: usize, role: &str) -> String {
    if role.is_empty() { format!("#{n}") } else { format!("#{n} {role}") }
}

/// Files the unit's agents edited, those edited by more than one agent first, then by name.
/// Paths are shown relative to the worktree when inside it.
pub fn touched(entries: &[Entry], members: &[usize]) -> Vec<Touched> {
    let mut by_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (h, &i) in members.iter().enumerate() {
        let e = &entries[i];
        let root = e.a.root.as_deref().unwrap_or(&e.a.cwd);
        for f in &e.a.edits {
            let shown = Path::new(f).strip_prefix(root).map(|p| p.display().to_string()).unwrap_or_else(|_| f.clone());
            let by = by_file.entry(shown).or_default();
            if !by.contains(&(h + 1)) {
                by.push(h + 1);
            }
        }
    }
    let mut v: Vec<Touched> = by_file.into_iter().map(|(file, by)| Touched { file, by }).collect();
    v.sort_by_key(|t| t.by.len() < 2);
    v
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
        if e.p.is_chat() { chat_title(e) } else { e.p.window_name.clone() },
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
/// Groups with a needs-input agent, and any active filter, force expansion; the same goes for
/// units (a worktree or chat with several agents), whose header rows `toggled` also folds.
/// Order: most urgent first, agent-less repos next, chats last.
pub fn build(entries: &[Entry], repos: &[Repo], pinned: &[String], toggled: &HashSet<String>, filter: &str) -> Vec<Row> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        if filter.is_empty() || fuzzy(filter, &haystack(e)) {
            groups.entry(group_of(e)).or_default().agents.push(i);
        }
    }
    let agent_dirs: Vec<PathBuf> = entries.iter().map(|e| canon(&e.a.cwd)).collect();
    let all_units = units(entries);
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
            (std::cmp::Reverse(urgency(e.a.status)), std::cmp::Reverse(e.a.updated), e.a.pane_id.clone())
        });
        // A unit's agents stay together, placed by its most urgent agent.
        let mut order: Vec<String> = vec![];
        for &i in &idx {
            let u = unit_of(&entries[i]);
            if !order.contains(&u) {
                order.push(u);
            }
        }
        for u in order {
            let members = &all_units[&u];
            if members.len() < 2 {
                let i = members[0];
                rows.push(Row { key: Key::Agent(entries[i].a.pane_id.clone()), kind: Kind::Agent { entry: i, slot: None } });
                continue;
            }
            let needs = members.iter().any(|&i| entries[i].a.status == Status::NeedsInput);
            let collapsed = toggled.contains(&u) && !needs && filter.is_empty();
            let shared = touched(entries, members).into_iter().filter(|t| t.by.len() > 1).collect();
            rows.push(Row {
                key: Key::Unit(u.clone()),
                kind: Kind::Unit { label: label(&entries[members[0]]), members: members.clone(), collapsed, shared },
            });
            if collapsed {
                continue;
            }
            let shown: Vec<(usize, usize)> =
                members.iter().enumerate().filter(|(_, i)| g.agents.contains(i)).map(|(h, &i)| (h + 1, i)).collect();
            for (k, &(n, i)) in shown.iter().enumerate() {
                rows.push(Row {
                    key: Key::Agent(entries[i].a.pane_id.clone()),
                    kind: Kind::Agent { entry: i, slot: Some(Slot { n, last: k + 1 == shown.len() }) },
                });
            }
        }
        if let Some(ri) = g.repo {
            // Most recently used first; ties keep git's order.
            let mut order: Vec<usize> = (0..repos[ri].worktrees.len()).collect();
            order.sort_by_key(|&wi| std::cmp::Reverse(repos[ri].worktrees[wi].last_used));
            for wi in order {
                let wt = &repos[ri].worktrees[wi];
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
                root: None, started: 0, edits: vec![],
            },
            p: PaneInfo {
                session: "dev".into(), session_key: "dev".into(), window_id: win.into(), window_name: branch.into(),
                kind: if chat { "chat".into() } else { "".into() },
                topic: String::new(),
            },
            role: String::new(),
        }
    }

    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.kind {
                Kind::Group { label, collapsed, .. } => format!("G:{label}{}", if *collapsed { "(folded)" } else { "" }),
                Kind::Unit { members, .. } => format!("U:{}", members.len()),
                Kind::Agent { entry, slot: None } => format!("A:{entry}"),
                Kind::Agent { entry, slot: Some(s) } => format!("A:{entry}#{}", s.n),
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
    fn agents_sharing_a_worktree_get_a_header_and_handles() {
        let mut es = vec![
            entry("%0", "@0", "a", "m", Status::Idle, false),
            entry("%1", "@0", "a", "m", Status::NeedsInput, false),
            entry("%2", "@1", "a", "m", Status::Working, false),
            entry("%3", "@2", "a", "other", Status::Working, false),
        ];
        // %0 and %2 share a worktree from different windows; %1 started first, so it is #1.
        for (e, (root, t)) in es.iter_mut().zip([("/w/m", 5), ("/w/m", 1), ("/w/m", 9), ("/w/o", 0)]) {
            e.a.root = Some(root.into());
            e.a.started = t;
        }
        let rows = build(&es, &[], &[], &HashSet::new(), "");
        assert_eq!(labels(&rows), ["G:a", "U:3", "A:1#1", "A:0#2", "A:2#3", "A:3"]);
        assert!(matches!(rows[4].kind, Kind::Agent { slot: Some(Slot { last: true, .. }), .. }));

        // A unit with a waiting agent can't be folded; otherwise it folds to its header.
        let fold: HashSet<String> = ["wt:/w/m".to_string()].into();
        assert_eq!(labels(&build(&es, &[], &[], &fold, ""))[..3], ["G:a", "U:3", "A:1#1"]);
        es[1].a.status = Status::Idle;
        assert_eq!(labels(&build(&es, &[], &[], &fold, "")), ["G:a", "U:3", "A:3"]);
    }

    #[test]
    fn touched_files_shared_first_relative_to_worktree() {
        let mut es = vec![
            entry("%0", "@0", "a", "m", Status::Idle, false),
            entry("%1", "@0", "a", "m", Status::Idle, false),
        ];
        es[0].a.root = Some("/w".into());
        es[0].a.edits = vec!["/w/b.rs".into(), "/w/a.rs".into()];
        es[1].a.root = Some("/w".into());
        es[1].a.edits = vec!["/w/a.rs".into(), "/elsewhere/c.rs".into()];
        let t = touched(&es, &[0, 1]);
        let flat: Vec<(&str, Vec<usize>)> = t.iter().map(|t| (t.file.as_str(), t.by.clone())).collect();
        assert_eq!(flat, [("a.rs", vec![1, 2]), ("/elsewhere/c.rs", vec![2]), ("b.rs", vec![1])]);
    }

    fn repo(name: &str, paths: &[(&str, &str)]) -> Repo {
        Repo {
            name: name.into(),
            path: paths[0].0.into(),
            worktrees: paths
                .iter()
                .map(|(p, b)| crate::discover::Worktree { path: (*p).into(), branch: Some((*b).into()), last_used: 0 })
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
    fn chat_title_prefers_topic_then_task_then_session() {
        let mut e = entry("%0", "@0", "r", "b", Status::Idle, true);
        e.p.window_name = "Developer".into(); // tmux renamed it to the cwd
        e.p.session = "chat-ideas".into();
        e.a.task = Some("brainstorm onboarding\nmore".into());
        assert_eq!(chat_title(&e), "brainstorm onboarding");
        e.p.topic = "launch plan".into();
        assert_eq!(chat_title(&e), "launch plan");
        e.p.topic.clear();
        e.a.task = None;
        assert_eq!(chat_title(&e), "ideas");
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

    #[test]
    fn worktrees_most_recently_used_first() {
        let mut repos = vec![repo("r", &[("/nope/r", "main"), ("/nope/r-wt/a", "a"), ("/nope/r-wt/b", "b")])];
        for (w, t) in repos[0].worktrees.iter_mut().zip([5, 1, 9]) {
            w.last_used = t;
        }
        let rows = build(&[], &repos, &["r".to_string()], &HashSet::from(["r".to_string()]), "");
        assert_eq!(labels(&rows), ["G:r", "W:2", "W:0", "W:1"]);
    }
}

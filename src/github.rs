//! Pull requests the user authored in the pinned repos, fetched through the `gh` CLI.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Merged PRs stay listed this long after the merge.
const MERGED_KEEP: u64 = 24 * 3600;

/// One request for every pinned repo: the login, open PRs, and PRs merged since `$merged`'s cutoff.
const QUERY: &str = "query($open:String!,$merged:String!){viewer{login} \
open:search(query:$open,type:ISSUE,first:100){...prs} merged:search(query:$merged,type:ISSUE,first:100){...prs}} \
fragment prs on SearchResultItemConnection{nodes{... on PullRequest{\
repository{nameWithOwner} number title body url isDraft state mergedAt \
commits(last:1){nodes{commit{statusCheckRollup{state}}}} \
reviews(first:50){nodes{body author{login __typename}}} \
comments(first:50){nodes{author{login __typename}}} \
reviewThreads(first:50){nodes{isResolved comments(first:20){nodes{author{login __typename}}}}}}}}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ci {
    /// No checks configured.
    None,
    Running,
    Failed,
    Passed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pr {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub body: String,
    pub draft: bool,
    pub ci: Ci,
    /// Comments from people other than the author: no bots, and nothing the user's own agents wrote.
    pub human_comments: usize,
    /// Unix seconds of the merge; `None` while open.
    pub merged: Option<u64>,
}

/// Seconds since the epoch for `2026-10-26T18:29:28Z`.
fn parse_time(s: &str) -> Option<u64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from civil date (Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + sec).ok()
}

/// `YYYY-MM-DD` of a Unix time, for search qualifiers.
fn date_of(t: u64) -> String {
    // Civil from days (Hinnant).
    let z = (t / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Whether a comment author counts as a person reviewing: not a bot, and not the PR's own author
/// (agents post under the user's account).
fn is_reviewer(author: &Value, me: &str) -> bool {
    let login = author["login"].as_str().unwrap_or("");
    author["__typename"].as_str() == Some("User") && !login.eq_ignore_ascii_case(me)
}

fn human_comments(pr: &Value, me: &str) -> usize {
    let nodes = |v: &Value| v["nodes"].as_array().cloned().unwrap_or_default();
    let reviews = nodes(&pr["reviews"]).iter().filter(|r| is_reviewer(&r["author"], me) && !r["body"].as_str().unwrap_or("").trim().is_empty()).count();
    let comments = nodes(&pr["comments"]).iter().filter(|c| is_reviewer(&c["author"], me)).count();
    let threads: usize = nodes(&pr["reviewThreads"])
        .iter()
        .filter(|t| t["isResolved"].as_bool() != Some(true))
        .map(|t| nodes(&t["comments"]).iter().filter(|c| is_reviewer(&c["author"], me)).count())
        .sum();
    reviews + comments + threads
}

fn ci_of(pr: &Value) -> Ci {
    match pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"].as_str() {
        Some("SUCCESS") => Ci::Passed,
        Some("FAILURE" | "ERROR") => Ci::Failed,
        Some("PENDING" | "EXPECTED") => Ci::Running,
        _ => Ci::None,
    }
}

/// The `gh` to run. A tmux popup or Rex tab may not have Homebrew on its PATH, so look in the
/// usual places before falling back to PATH (`CC_HUD_GH` overrides).
fn gh_bin() -> String {
    if let Some(p) = std::env::var("CC_HUD_GH").ok().filter(|p| !p.is_empty()) {
        return p;
    }
    ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .unwrap_or("gh")
        .to_string()
}

fn gh(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(gh_bin()).current_dir(cwd).args(args).output().map_err(|e| format!("gh ({}): {e}; set CC_HUD_GH to its path", gh_bin()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().next().unwrap_or("gh failed").to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `owner/name` from a GitHub remote URL (`https://github.com/o/n.git`, `git@github.com:o/n.git`).
fn nwo_of_url(url: &str) -> Option<String> {
    let rest = url.trim().split_once("github.com")?.1.trim_start_matches([':', '/']);
    let rest = rest.strip_suffix(".git").unwrap_or(rest).trim_end_matches('/');
    let (owner, name) = rest.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/')).then(|| format!("{owner}/{name}"))
}

/// The GitHub repo checked out at `path`, read from its remotes (no network): `origin` if it
/// is on GitHub, else the first one that is.
fn nwo(path: &Path) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(path).args(["remote", "-v"]).output().map_err(|e| format!("git: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let found = |origin: bool| {
        text.lines()
            .filter_map(|l| l.split_once(char::is_whitespace))
            .filter(|(r, _)| !origin || *r == "origin")
            .find_map(|(_, url)| nwo_of_url(url.split_whitespace().next()?))
    };
    found(true).or_else(|| found(false)).ok_or_else(|| "no GitHub remote".to_string())
}

/// Open PRs first, merged ones last (newest merge first), grouped by repo order given.
pub fn fetch(repos: &[(String, PathBuf)], now: u64) -> Result<Vec<Pr>, String> {
    let Some((_, first)) = repos.first() else { return Ok(vec![]) };
    let mut names: Vec<(String, &str)> = vec![];
    let mut err = None;
    for (name, path) in repos {
        match nwo(path) {
            Ok(n) => names.push((n, name)),
            Err(e) => err = Some(format!("{name}: {e}")),
        }
    }
    if names.is_empty() {
        return Err(err.unwrap_or_default());
    }
    let scope: String = names.iter().map(|(n, _)| format!("repo:{n} ")).collect();
    // Search only has day precision; the exact cutoff is applied below.
    let open = format!("open={scope}is:pr author:@me is:open");
    let merged = format!("merged={scope}is:pr author:@me is:merged merged:>={}", date_of(now.saturating_sub(MERGED_KEEP)));
    let json = gh(first, &["api", "graphql", "-f", &format!("query={QUERY}"), "-f", &open, "-f", &merged])?;
    let v: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let me = v["data"]["viewer"]["login"].as_str().unwrap_or("");
    let nodes = |k: &str| v["data"][k]["nodes"].as_array().cloned().unwrap_or_default();
    let mut all: Vec<Pr> = nodes("open")
        .iter()
        .chain(nodes("merged").iter())
        .filter_map(|p| {
            let nwo = p["repository"]["nameWithOwner"].as_str()?;
            let repo = names.iter().find(|(n, _)| n.eq_ignore_ascii_case(nwo))?.1;
            let merged = p["mergedAt"].as_str().and_then(parse_time);
            match p["state"].as_str()? {
                "OPEN" => {}
                "MERGED" if merged.is_some_and(|t| now.saturating_sub(t) < MERGED_KEEP) => {}
                _ => return None,
            }
            Some(Pr {
                repo: repo.to_string(),
                number: p["number"].as_u64()?,
                title: p["title"].as_str()?.to_string(),
                url: p["url"].as_str()?.to_string(),
                body: p["body"].as_str().unwrap_or("").replace('\r', ""),
                draft: p["isDraft"].as_bool().unwrap_or(false),
                ci: if merged.is_some() { Ci::None } else { ci_of(p) },
                human_comments: human_comments(p, me),
                merged,
            })
        })
        .collect();
    // Stable: open PRs keep their repo/number order, merged sink below them.
    all.sort_by_key(|p| (p.merged.is_some(), std::cmp::Reverse(p.merged), p.repo.clone(), std::cmp::Reverse(p.number)));
    Ok(all)
}

/// The last fetch, kept on disk so the next dashboard shows PRs before its own fetch returns.
#[derive(Serialize, Deserialize)]
pub struct Cache {
    /// The pinned repos (name, checkout) it was fetched for.
    pub targets: Vec<(String, PathBuf)>,
    pub prs: Vec<Pr>,
}

fn cache_file() -> PathBuf {
    crate::state::state_dir().join("prs.json")
}

pub fn load_cache() -> Option<Cache> {
    serde_json::from_slice(&std::fs::read(cache_file()).ok()?).ok()
}

pub fn save_cache(targets: &[(String, PathBuf)], prs: &[Pr]) {
    let c = Cache { targets: targets.to_vec(), prs: prs.to_vec() };
    if let Ok(text) = serde_json::to_vec(&c) {
        let _ = std::fs::write(cache_file(), text);
    }
}

/// Open a URL in the default browser.
pub fn open_url(url: &str) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{opener}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamps() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-10-26T18:29:28Z"), Some(1793039368));
    }

    #[test]
    fn formats_dates() {
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(1793039368), "2026-10-26");
        assert_eq!(date_of(951782400), "2000-02-29");
    }

    #[test]
    fn reads_remote_urls() {
        assert_eq!(nwo_of_url("https://github.com/o/n.git\n").as_deref(), Some("o/n"));
        assert_eq!(nwo_of_url("git@github.com:o/n.git").as_deref(), Some("o/n"));
        assert_eq!(nwo_of_url("ssh://git@github.com/o/n").as_deref(), Some("o/n"));
        assert_eq!(nwo_of_url("https://gitlab.com/o/n.git"), None);
    }

    #[test]
    fn counts_only_human_reviewers() {
        let pr: Value = serde_json::json!({
            "reviews": {"nodes": [
                {"body": "needs work", "author": {"login": "bob", "__typename": "User"}},
                {"body": "", "author": {"login": "carol", "__typename": "User"}},
                {"body": "lgtm", "author": {"login": "copilot", "__typename": "Bot"}}
            ]},
            "comments": {"nodes": [
                {"author": {"login": "me", "__typename": "User"}},
                {"author": {"login": "dave", "__typename": "User"}}
            ]},
            "reviewThreads": {"nodes": [
                {"isResolved": true, "comments": {"nodes": [{"author": {"login": "bob", "__typename": "User"}}]}},
                {"isResolved": false, "comments": {"nodes": [{"author": {"login": "bob", "__typename": "User"}}]}}
            ]}
        });
        assert_eq!(human_comments(&pr, "me"), 3);
    }
}

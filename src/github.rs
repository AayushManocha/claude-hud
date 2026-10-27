//! Pull requests the user authored in the pinned repos, fetched through the `gh` CLI.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Merged PRs stay listed this long after the merge.
const MERGED_KEEP: u64 = 24 * 3600;

const QUERY: &str = "query($q:String!){search(query:$q,type:ISSUE,first:30){nodes{... on PullRequest{\
number title url isDraft state mergedAt \
commits(last:1){nodes{commit{statusCheckRollup{state}}}} \
reviews(first:50){nodes{body author{login __typename}}} \
comments(first:50){nodes{author{login __typename}}} \
reviewThreads(first:50){nodes{isResolved comments(first:20){nodes{author{login __typename}}}}}}}}}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ci {
    /// No checks configured.
    None,
    Running,
    Failed,
    Passed,
}

#[derive(Debug, Clone)]
pub struct Pr {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
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

fn gh(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("gh").current_dir(cwd).args(args).output().map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().next().unwrap_or("gh failed").to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The user's open PRs and those merged within the last day in the repo checked out at `path`.
fn fetch_repo(name: &str, path: &Path, me: &str, now: u64) -> Result<Vec<Pr>, String> {
    let nwo = gh(path, &["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"])?;
    let q = format!("q=repo:{nwo} is:pr author:@me sort:updated-desc");
    let json = gh(path, &["api", "graphql", "-f", &format!("query={QUERY}"), "-f", &q])?;
    let v: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let nodes = v["data"]["search"]["nodes"].as_array().cloned().unwrap_or_default();
    Ok(nodes
        .iter()
        .filter_map(|p| {
            let merged = p["mergedAt"].as_str().and_then(parse_time);
            match p["state"].as_str()? {
                "OPEN" => {}
                "MERGED" if merged.is_some_and(|t| now.saturating_sub(t) < MERGED_KEEP) => {}
                _ => return None,
            }
            Some(Pr {
                repo: name.to_string(),
                number: p["number"].as_u64()?,
                title: p["title"].as_str()?.to_string(),
                url: p["url"].as_str()?.to_string(),
                draft: p["isDraft"].as_bool().unwrap_or(false),
                ci: if merged.is_some() { Ci::None } else { ci_of(p) },
                human_comments: human_comments(p, me),
                merged,
            })
        })
        .collect())
}

/// Open PRs first, merged ones last (newest merge first), grouped by repo order given.
pub fn fetch(repos: &[(String, std::path::PathBuf)], now: u64) -> Result<Vec<Pr>, String> {
    let Some((_, first)) = repos.first() else { return Ok(vec![]) };
    let me = gh(first, &["api", "user", "-q", ".login"])?;
    let mut all = vec![];
    let mut err = None;
    for (name, path) in repos {
        match fetch_repo(name, path, &me, now) {
            Ok(mut v) => all.append(&mut v),
            Err(e) => err = Some(format!("{name}: {e}")),
        }
    }
    match (all.is_empty(), err) {
        (true, Some(e)) => Err(e),
        _ => {
            // Stable: open PRs keep their repo/number order, merged sink below them.
            all.sort_by_key(|p| (p.merged.is_some(), std::cmp::Reverse(p.merged), p.repo.clone(), std::cmp::Reverse(p.number)));
            Ok(all)
        }
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

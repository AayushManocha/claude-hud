//! Answering an agent's prompt from outside its pane, via `tmux send-keys`.
//! Everything here is conservative: we only type into a pane after parsing
//! its current screen and confirming it shows what we expect.

use crate::state::{self, Status};
use crate::tmux;

pub const CAPTURE_LINES: usize = 60;
pub const MAX_REPLY: usize = 500;

#[derive(Debug, PartialEq, Eq)]
pub struct Prompt {
    pub options: Vec<(u32, String)>,
    /// The dialog's own lines (context above the options plus the options),
    /// excluding status lines that tick, so it can detect "the prompt changed".
    pub fingerprint: String,
}

#[derive(Clone, Copy)]
pub enum Decision {
    Allow,
    Deny,
}

impl Prompt {
    /// Option number for a one-time "Yes". Broader grants ("Yes, and always
    /// allow…", "…switch to auto mode") are never chosen from the HUD.
    pub fn allow_once(&self) -> Option<u32> {
        self.options.iter().find(|(_, t)| t == "Yes").map(|(n, _)| *n)
    }

    pub fn deny(&self) -> Option<u32> {
        self.options.iter().find(|(_, t)| t.starts_with("No")).map(|(n, _)| *n)
    }

    fn is_permission(&self) -> bool {
        self.allow_once().is_some() && self.deny().is_some()
    }
}

fn strip_marker(line: &str) -> &str {
    line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '❯' | '›' | '>'))
}

fn numbered(line: &str) -> Option<(u32, String)> {
    let l = strip_marker(line);
    let (n, rest) = l.split_once(". ")?;
    Some((n.parse().ok()?, rest.trim().trim_end_matches('│').trim().to_string()))
}

/// Parse a numbered-choice dialog (`❯ 1. Yes … 4. No`, then an "Esc to cancel"
/// footer) from the bottom of a pane capture.
pub fn parse(capture: &str) -> Option<Prompt> {
    let lines: Vec<&str> = capture.lines().collect();
    let mut opts: Vec<(u32, String)> = vec![];
    let (mut first, mut last) = (0, 0);
    for (i, l) in lines.iter().enumerate() {
        if let Some((n, t)) = numbered(l) {
            if n == 1 {
                opts.clear();
                first = i;
            }
            if n as usize == opts.len() + 1 && !opts.is_empty() || n == 1 {
                opts.push((n, t));
                last = i;
            }
        }
    }
    let footer = lines.iter().skip(last + 1).any(|l| l.contains("Esc to cancel"));
    if opts.is_empty() || !footer {
        return None;
    }
    let from = first.saturating_sub(8);
    let fingerprint = lines[from..=last]
        .iter()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    Some(Prompt { options: opts, fingerprint })
}

/// Result messages are shown in the dashboard footer.
pub type Outcome = Result<String, String>;

/// Send a one-time allow/deny. `seen` is the fingerprint the user was looking
/// at; if the screen moved on, nothing is sent.
pub fn decide(pane_id: &str, d: Decision, seen: &str) -> Outcome {
    let st = state::load(pane_id).ok_or("agent state is gone")?;
    if st.status != Status::NeedsInput {
        return Err("agent no longer needs input".into());
    }
    let prompt = parse(&tmux::capture(pane_id, CAPTURE_LINES)).ok_or("no permission dialog found on screen")?;
    if prompt.fingerprint != seen {
        return Err("prompt changed; review it and try again".into());
    }
    let (n, verb) = match d {
        Decision::Allow => (prompt.allow_once(), "allowed once"),
        Decision::Deny => (prompt.deny(), "denied"),
    };
    let n = n.ok_or("dialog has no plain Yes/No option; answer in the pane")?;
    // A number key selects the option immediately; no Enter (it could land on a follow-up dialog).
    tmux::run(&["send-keys", "-t", pane_id, &n.to_string()]);
    Ok(format!("{verb}: {}", st.message.or(st.tool).unwrap_or_default()))
}

/// Type a short answer into an idle (or question-asking) agent.
pub fn reply(pane_id: &str, text: &str) -> Outcome {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty reply".into());
    }
    if text.contains('\n') || text.chars().count() > MAX_REPLY {
        return Err(format!("replies are single-line, max {MAX_REPLY} chars"));
    }
    let st = state::load(pane_id).ok_or("agent state is gone")?;
    if st.status == Status::Working {
        return Err("agent is working; wait for it to stop".into());
    }
    if let Some(prompt) = parse(&tmux::capture(pane_id, CAPTURE_LINES)) {
        if prompt.is_permission() {
            return Err("permission prompt: use [a]llow / [d]eny".into());
        }
        // A choice dialog: only a matching option number is safe to send.
        return match text.parse::<u32>() {
            Ok(n) if prompt.options.iter().any(|(o, _)| *o == n) => {
                tmux::run(&["send-keys", "-t", pane_id, text]);
                Ok(format!("chose option {n}"))
            }
            _ => Err("choice dialog: reply with an option number, or answer in the pane".into()),
        };
    }
    tmux::run(&["send-keys", "-t", pane_id, "-l", "--", text]);
    tmux::run(&["send-keys", "-t", pane_id, "Enter"]);
    Ok(format!("sent: {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from a real Claude Code 2.1 permission dialog.
    const PERM: &str = "\
 Bash command
 Write hello-hud to /tmp/hudreal/out.txt
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 echo hello-hud > /tmp/hudreal/out.txt
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /tmp/hudreal from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No

 Esc to cancel · Tab to amend
";

    #[test]
    fn parses_real_permission_dialog() {
        let p = parse(PERM).unwrap();
        assert_eq!(p.options.len(), 4);
        assert_eq!(p.allow_once(), Some(1)); // never 2 or 3
        assert_eq!(p.deny(), Some(4));
        assert!(p.fingerprint.contains("echo hello-hud"));
    }

    #[test]
    fn allow_needs_plain_yes() {
        let p = parse(" ❯ 1. Yes, and always allow\n   2. No\n\n Esc to cancel\n").unwrap();
        assert_eq!(p.allow_once(), None);
    }

    #[test]
    fn requires_dialog_footer() {
        // A numbered list in ordinary conversation is not a dialog.
        assert!(parse("1. Yes\n2. No\nthat was my plan\n").is_none());
    }

    #[test]
    fn last_list_wins() {
        let c = "1. old\n2. list\n\n ❯ 1. Yes\n   2. No\n\n Esc to cancel\n";
        assert_eq!(parse(c).unwrap().options.len(), 2);
    }

    #[test]
    fn fingerprint_ignores_lines_below() {
        let a = format!("{PERM}\n  Haiku | 34s\n");
        let b = format!("{PERM}\n  Haiku | 35s\n");
        assert_eq!(parse(&a).unwrap().fingerprint, parse(&b).unwrap().fingerprint);
    }
}

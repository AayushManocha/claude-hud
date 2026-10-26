use crate::{git, notify, state, sync};
use crate::state::{AgentState, Status};
use anyhow::Result;
use serde_json::Value;
use std::io::Read;

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn truncate(s: &str, n: usize) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > n {
        let t: String = line.chars().take(n - 1).collect();
        format!("{t}…")
    } else {
        line.to_string()
    }
}

/// "Edit src/foo.ts", "Bash npm test", ...
fn describe_tool(input: &Value, name: &str) -> String {
    let ti = input.get("tool_input");
    let detail = ti
        .and_then(|t| {
            ["file_path", "command", "pattern", "url", "description"]
                .iter()
                .find_map(|k| t.get(k).and_then(Value::as_str))
        })
        .unwrap_or("");
    truncate(&format!("{name} {detail}"), 80)
}

/// What a Notification means for agent status. `None` leaves it unchanged.
fn notification_status(input: &Value) -> Option<Status> {
    match str_field(input, "notification_type") {
        Some("permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input") => {
            Some(Status::NeedsInput)
        }
        Some("idle_prompt" | "agent_completed") => Some(Status::Idle),
        Some(_) => None,
        // Older versions send no type: fall back to the message text.
        None => Some(
            if str_field(input, "message")
                .unwrap_or("")
                .to_lowercase()
                .contains("waiting for your input")
            {
                Status::Idle
            } else {
                Status::NeedsInput
            },
        ),
    }
}

/// Hook entrypoint. Must never write to stdout (it can be injected into the
/// agent's context) and must never fail the hook, so errors are swallowed.
pub fn run(hook: &str) {
    let _ = handle(hook);
    sync::run();
}

fn handle(hook: &str) -> Result<()> {
    let Some(pane) = crate::mux::get().current_pane() else {
        return Ok(()); // not running under a multiplexer
    };
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let input: Value = serde_json::from_str(&buf).unwrap_or(Value::Null);

    if hook == "session-end" {
        state::remove(&pane);
        return Ok(());
    }

    let now = state::now();
    let session_id = str_field(&input, "session_id").unwrap_or("").to_string();
    let cwd = str_field(&input, "cwd")
        .map(String::from)
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_default();

    let prev = state::load(&pane);
    // A different session in the same pane (new `claude` run) starts fresh.
    let prev = prev.filter(|p| session_id.is_empty() || p.session_id == session_id);

    let mut s = prev.unwrap_or_else(|| AgentState {
        pane_id: pane.clone(),
        session_id: session_id.clone(),
        cwd: cwd.clone(),
        repo: None,
        branch: None,
        status: Status::Idle,
        task: None,
        tool: None,
        message: None,
        since: now,
        updated: now,
    });

    if s.cwd != cwd || s.repo.is_none() || matches!(hook, "session-start" | "user-prompt-submit") {
        s.cwd = cwd.clone();
        s.repo = git::repo_name(&cwd);
        s.branch = git::branch(&cwd);
    }

    let new_status = match hook {
        "session-start" | "stop" => {
            s.tool = None;
            s.message = None;
            Status::Idle
        }
        "user-prompt-submit" => {
            s.task = Some(truncate(str_field(&input, "prompt").unwrap_or(""), 120));
            s.tool = None;
            s.message = None;
            Status::Working
        }
        "pre-tool-use" => {
            s.tool = Some(describe_tool(&input, str_field(&input, "tool_name").unwrap_or("")));
            s.message = None;
            Status::Working
        }
        "post-tool-use" => {
            s.message = None;
            Status::Working
        }
        "notification" => {
            match notification_status(&input) {
                Some(st) => {
                    // Keep the specific text a PermissionRequest hook already recorded
                    // over the generic "Claude needs your permission".
                    if !(st == Status::NeedsInput && s.status == Status::NeedsInput && s.message.is_some()) {
                        s.message = str_field(&input, "message").map(|m| truncate(m, 160));
                    }
                    st
                }
                None => s.status,
            }
        }
        // Fires the moment a permission dialog appears (Notification lags ~6s).
        "permission-request" => {
            s.message = Some(describe_tool(&input, str_field(&input, "tool_name").unwrap_or("")));
            Status::NeedsInput
        }
        _ => s.status,
    };

    let became_blocked = new_status == Status::NeedsInput && s.status != Status::NeedsInput;
    if new_status != s.status {
        s.status = new_status;
        s.since = now;
    }
    s.updated = now;
    state::save(&s)?;
    if became_blocked {
        notify::needs_input(&s);
    }
    Ok(())
}

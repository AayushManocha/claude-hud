use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

/// (Claude Code event name, `cc-hud event` argument)
const HOOKS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "user-prompt-submit"),
    ("PreToolUse", "pre-tool-use"),
    ("PostToolUse", "post-tool-use"),
    ("PermissionRequest", "permission-request"),
    ("Notification", "notification"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

fn is_ours(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .is_some_and(|hs| hs.iter().any(|h| h["command"].as_str().is_some_and(|c| c.contains("cc-hud event"))))
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Route the status line through `cc-hud statusline` so the dashboard can show plan usage.
/// An existing status line command keeps running unchanged behind it.
fn wrap_statusline(settings: &mut Value, bin: &str) {
    let ours = format!("{bin} statusline");
    let obj = settings.as_object_mut().expect("settings must be an object");
    let cur = obj.get("statusLine").and_then(|s| s["command"].as_str()).map(str::to_string);
    let command = match cur {
        Some(c) if c.contains("cc-hud statusline") => return,
        Some(c) => format!("{ours} --then {}", sh_quote(&c)),
        None => ours,
    };
    let sl = obj.entry("statusLine").or_insert_with(|| json!({ "type": "command", "padding": 0 }));
    sl["command"] = json!(command);
}

pub fn merge(settings: &mut Value, bin: &str) {
    wrap_statusline(settings, bin);
    let hooks = settings
        .as_object_mut()
        .expect("settings must be an object")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    for (event, arg) in HOOKS {
        let groups = hooks
            .as_object_mut()
            .unwrap()
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();
        groups.retain(|g| !is_ours(g)); // idempotent re-install
        groups.push(json!({
            "hooks": [{ "type": "command", "command": format!("{bin} event {arg}"), "timeout": 5 }]
        }));
    }
}

pub fn run(write: bool, settings: Option<PathBuf>) -> Result<()> {
    let bin = std::env::current_exe()?.display().to_string();
    let path = settings.unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/settings.json")
    });
    let mut cur: Value = match std::fs::read(&path) {
        Ok(d) => serde_json::from_slice(&d).with_context(|| format!("parsing {}", path.display()))?,
        Err(_) => json!({}),
    };
    merge(&mut cur, &bin);
    let out = serde_json::to_string_pretty(&cur)?;
    if write {
        if path.exists() {
            std::fs::copy(&path, path.with_extension("json.bak"))?;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, out + "\n")?;
        eprintln!("hooks installed in {}", path.display());
    } else {
        println!("{out}");
        eprintln!("(dry run: pass --write to update {})", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_existing_statusline_once() {
        let mut s = json!({ "statusLine": { "type": "command", "command": "python3 ~/it's.py", "padding": 0 } });
        merge(&mut s, "/x/cc-hud");
        let c = s["statusLine"]["command"].as_str().unwrap().to_string();
        assert_eq!(c, "/x/cc-hud statusline --then 'python3 ~/it'\\''s.py'");
        merge(&mut s, "/x/cc-hud");
        assert_eq!(s["statusLine"]["command"], c.as_str());
        assert_eq!(s["statusLine"]["padding"], 0);
    }

    #[test]
    fn adds_statusline_when_missing() {
        let mut s = json!({});
        merge(&mut s, "/x/cc-hud");
        assert_eq!(s["statusLine"]["command"], "/x/cc-hud statusline");
        assert_eq!(s["statusLine"]["type"], "command");
    }
}

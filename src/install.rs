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

pub fn merge(settings: &mut Value, bin: &str) {
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

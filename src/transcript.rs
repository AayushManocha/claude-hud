//! Scrollback for the preview, read from Claude Code's session transcript.
//!
//! Claude Code runs in the terminal's alternate screen, where tmux keeps no
//! history, so older output is only available from the `.jsonl` transcript.
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

const MAX_RESULT_LINES: usize = 6;

/// Transcript file for a session: `~/.claude/projects/<cwd slug>/<session>.jsonl`.
pub fn find(cwd: &str, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() {
        return None;
    }
    let projects = PathBuf::from(std::env::var("HOME").ok()?).join(".claude/projects");
    let slug: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let direct = projects.join(slug).join(format!("{session_id}.jsonl"));
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|d| d.path().join(format!("{session_id}.jsonl")))
        .find(|p| p.is_file())
}

pub struct Transcript {
    pub session_id: String,
    path: PathBuf,
    offset: u64,
    /// Bytes after the last newline, kept until the writer finishes the line.
    partial: Vec<u8>,
    logical: Vec<(Style, String)>,
    wrapped: Vec<Line<'static>>,
    wrapped_width: u16,
    wrapped_upto: usize,
}

impl Transcript {
    pub fn open(path: PathBuf, session_id: String) -> Self {
        Transcript { session_id, path, offset: 0, partial: vec![], logical: vec![], wrapped: vec![], wrapped_width: 0, wrapped_upto: 0 }
    }

    /// Read whatever was appended since last time.
    pub fn update(&mut self) {
        let Ok(mut f) = File::open(&self.path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            // Truncated or replaced: start over.
            *self = Transcript::open(self.path.clone(), std::mem::take(&mut self.session_id));
        }
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = vec![];
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        self.offset += buf.len() as u64;
        self.partial.extend(buf);
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else { return };
        let rest = self.partial.split_off(end + 1);
        let done = std::mem::replace(&mut self.partial, rest);
        for line in done.split(|&b| b == b'\n') {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                render_entry(&v, &mut self.logical);
            }
        }
    }

    /// Wrapped lines for a pane `width` wide. Only re-wraps what's new unless the width changed.
    pub fn lines(&mut self, width: u16) -> &[Line<'static>] {
        if width != self.wrapped_width {
            self.wrapped.clear();
            self.wrapped_upto = 0;
            self.wrapped_width = width;
        }
        for (st, text) in &self.logical[self.wrapped_upto..] {
            for part in wrap(text, width as usize) {
                self.wrapped.push(Line::from(Span::styled(part, *st)));
            }
        }
        self.wrapped_upto = self.logical.len();
        &self.wrapped
    }
}

/// Hard-wrap on width, preferring to break at a space.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = vec![];
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let mut i = 0;
    while i < chars.len() {
        let mut end = (i + width).min(chars.len());
        if end < chars.len() {
            if let Some(sp) = chars[i..end].iter().rposition(|&c| c == ' ').filter(|&p| p > 0) {
                end = i + sp + 1;
            }
        }
        out.push(chars[i..end].iter().collect::<String>().trim_end().to_string());
        i = end;
    }
    out
}

fn push_text(out: &mut Vec<(Style, String)>, marker: &str, style: Style, text: &str) {
    let pad = " ".repeat(marker.chars().count());
    for (n, l) in text.trim_end().lines().enumerate() {
        out.push((style, format!("{}{l}", if n == 0 { marker } else { &pad })));
    }
}

fn tool_summary(name: &str, input: &Value) -> String {
    let key = ["command", "file_path", "pattern", "path", "url", "description", "query"].iter().find_map(|k| input.get(k).and_then(Value::as_str));
    match key {
        Some(s) => format!("{name}({})", s.lines().next().unwrap_or("")),
        None => name.to_string(),
    }
}

fn result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Append the display lines for one transcript entry.
fn render_entry(v: &Value, out: &mut Vec<(Style, String)>) {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "user" | "assistant") || v.get("isSidechain").and_then(Value::as_bool) == Some(true) || v.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let text = Style::new().fg(Color::Rgb(235, 219, 178));
    let dim = Style::new().fg(Color::Rgb(146, 131, 116));
    let user = Style::new().fg(Color::Rgb(131, 165, 152)).add_modifier(Modifier::BOLD);
    let content = &v["message"]["content"];
    let before = out.len();
    match content {
        Value::String(s) if kind == "user" => {
            // Injected context (system reminders, command wrappers) isn't something the user typed.
            if !s.trim_start().starts_with('<') {
                push_text(out, "❯ ", user, s);
            }
        }
        Value::Array(blocks) => {
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                        if kind == "user" {
                            if !t.trim_start().starts_with('<') {
                                push_text(out, "❯ ", user, t);
                            }
                        } else {
                            push_text(out, "● ", text, t);
                        }
                    }
                    Some("tool_use") => {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                        push_text(out, "● ", Style::new().fg(Color::Rgb(184, 187, 38)), &tool_summary(name, &b["input"]));
                    }
                    Some("tool_result") => {
                        let r = result_text(&b["content"]);
                        let all: Vec<&str> = r.trim_end().lines().collect();
                        for (n, l) in all.iter().take(MAX_RESULT_LINES).enumerate() {
                            out.push((dim, format!("{}{l}", if n == 0 { "  ⎿ " } else { "    " })));
                        }
                        if all.len() > MAX_RESULT_LINES {
                            out.push((dim, format!("    … +{} lines", all.len() - MAX_RESULT_LINES)));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if out.len() > before {
        out.push((text, String::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(json: &str) -> Vec<String> {
        let mut out = vec![];
        render_entry(&serde_json::from_str(json).unwrap(), &mut out);
        out.into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn renders_messages_and_tools() {
        assert_eq!(render(r#"{"type":"user","message":{"content":"hi there"}}"#), ["❯ hi there", ""]);
        assert_eq!(
            render(r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"ok"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#),
            ["● ok", "● Bash(ls)", ""]
        );
        assert_eq!(render(r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"a\nb"}]}}"#), ["  ⎿ a", "    b", ""]);
    }

    #[test]
    fn skips_injected_and_unrelated_entries() {
        assert!(render(r#"{"type":"user","message":{"content":"<system-reminder>x</system-reminder>"}}"#).is_empty());
        assert!(render(r#"{"type":"api-request","message":{"content":"x"}}"#).is_empty());
    }

    #[test]
    fn wraps_at_spaces() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa", "bbb ccc"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }
}

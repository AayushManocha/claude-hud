use crate::discover::{self, Repo};
use crate::interact::{self, Decision};
use crate::model::{self, Entry, Key, Kind, Row};
use crate::state::{self, Status};
use crate::{config, lifecycle, sync, tmux};
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// What the footer input line is collecting.
enum InputKind {
    Reply,
    /// New worktree in repo `repos[i]`.
    Worktree(usize),
    Chat(PathBuf),
}

/// Fuzzy repo picker for pinning (`P`).
struct Picker {
    query: String,
    sel: usize,
}

struct App {
    client: Option<String>,
    entries: Vec<Entry>,
    rows: Vec<Row>,
    repos: Vec<Repo>,
    repo_rx: Receiver<Vec<Repo>>,
    toggled: HashSet<String>,
    grid: bool,
    grid_cols: usize,
    tiles: HashMap<String, String>,
    filter: String,
    typing: bool,
    /// Pins from tmux config (read-only here) and pins saved from the dashboard.
    config_pins: Vec<String>,
    saved_pins: Vec<String>,
    pinned: Vec<String>,
    picker: Option<Picker>,
    peek: bool,
    peek_text: String,
    /// Fingerprint of the dialog the user is currently looking at in the peek.
    seen: Option<(String, String)>,
    /// When `seen` last changed; decisions right after a change are refused.
    seen_at: Instant,
    input: Option<(InputKind, String)>,
    msg: Option<(Result<String, String>, Instant)>,
    list: ListState,
    sel: Option<Key>,
    quit: bool,
}

const TEXT: Color = Color::Rgb(235, 219, 178);
const DIM: Color = Color::Rgb(146, 131, 116);
const FAINT: Color = Color::Rgb(80, 73, 69);
const SURFACE: Color = Color::Rgb(60, 56, 54);
const ACCENT: Color = Color::Rgb(131, 165, 152);
const RED: Color = Color::Rgb(251, 73, 52);
const YELLOW: Color = Color::Rgb(250, 189, 47);
const GREEN: Color = Color::Rgb(184, 187, 38);

fn status_color(s: Status) -> Color {
    match s {
        Status::NeedsInput => RED,
        Status::Working => YELLOW,
        Status::Idle => GREEN,
    }
}

fn status_style(s: Status) -> Style {
    let st = Style::new().fg(status_color(s));
    if s == Status::NeedsInput { st.add_modifier(Modifier::BOLD) } else { st }
}

fn dim() -> Style {
    Style::new().fg(DIM)
}

/// Rounded panel with a muted border.
fn panel<'a>() -> Block<'a> {
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(FAINT))
}

fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

impl App {
    fn new(client: Option<String>, grid: bool) -> Self {
        // Scanning runs git per repo, so keep it off the UI thread.
        let (tx, repo_rx) = mpsc::channel();
        std::thread::spawn(move || loop {
            if tx.send(discover::scan(&config::root())).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_secs(10));
        });
        let mut a = App {
            client,
            entries: vec![],
            rows: vec![],
            repos: vec![],
            repo_rx,
            toggled: HashSet::new(),
            grid,
            grid_cols: 1,
            tiles: HashMap::new(),
            filter: String::new(),
            typing: false,
            pinned: vec![],
            config_pins: config::config_pins(),
            saved_pins: config::saved_pins(),
            picker: None,
            peek: true,
            peek_text: String::new(),
            seen: None,
            seen_at: Instant::now(),
            input: None,
            msg: None,
            list: ListState::default(),
            sel: None,
            quit: false,
        };
        a.pinned = a.all_pins();
        a.refresh();
        a
    }

    fn all_pins(&self) -> Vec<String> {
        let mut v = self.config_pins.clone();
        v.extend(self.saved_pins.iter().cloned());
        v
    }

    /// Discovered repos matching the picker query, by name.
    fn picker_matches(&self) -> Vec<usize> {
        let q = self.picker.as_ref().map(|p| p.query.as_str()).unwrap_or("");
        let mut v: Vec<usize> = (0..self.repos.len()).filter(|&i| model::fuzzy(q, &self.repos[i].name)).collect();
        v.sort_by_key(|&i| self.repos[i].name.to_lowercase());
        v
    }

    fn on_picker_key(&mut self, k: KeyEvent) {
        let Some(p) = self.picker.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Down => p.sel += 1,
            KeyCode::Char('n') if ctrl => p.sel += 1,
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Char('p') if ctrl => p.sel = p.sel.saturating_sub(1),
            KeyCode::Backspace => {
                p.query.pop();
                p.sel = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                p.query.push(c);
                p.sel = 0;
            }
            KeyCode::Enter => self.toggle_pin(),
            _ => {}
        }
    }

    fn toggle_pin(&mut self) {
        let m = self.picker_matches();
        let Some(&ri) = self.picker.as_ref().and_then(|p| m.get(p.sel.min(m.len().saturating_sub(1)))) else { return };
        let name = self.repos[ri].name.clone();
        if self.config_pins.contains(&name) {
            return self.say(Err(format!("{name} is pinned in tmux config (@claude-hud-projects)")));
        }
        let was = self.saved_pins.contains(&name);
        if was {
            self.saved_pins.retain(|n| n != &name);
        } else {
            self.saved_pins.push(name.clone());
        }
        match config::save_pins(&self.saved_pins) {
            Ok(()) => {
                self.pinned = self.all_pins();
                self.say(Ok(format!("{} {name}", if was { "unpinned" } else { "pinned" })));
                self.refresh();
            }
            Err(e) => self.say(Err(format!("saving pins: {e}"))),
        }
    }

    fn render_picker(&self, f: &mut Frame) {
        let Some(p) = &self.picker else { return };
        let a = f.area();
        let (w, h) = (a.width.min(60), a.height.min(20));
        let area = Rect::new(a.x + (a.width - w) / 2, a.y + (a.height - h) / 2, w, h);
        f.render_widget(Clear, area);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(ACCENT))
            .title(Span::styled(" pin repos ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
            .title_bottom(Line::from(Span::styled(" ⏎ toggle · esc close ", dim())).right_aligned());
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [q, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        f.render_widget(
            Line::from(vec![
                Span::styled(" ", Style::new().fg(ACCENT)),
                Span::styled(p.query.clone(), Style::new().fg(TEXT)),
                Span::styled("▏", Style::new().fg(ACCENT)),
            ]),
            q,
        );
        let m = self.picker_matches();
        let items: Vec<ListItem> = m
            .iter()
            .map(|&i| {
                let n = &self.repos[i].name;
                let (mark, note) = if self.config_pins.contains(n) {
                    ("★", "  (config)")
                } else if self.saved_pins.contains(n) {
                    ("★", "")
                } else {
                    (" ", "")
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {mark} "), Style::new().fg(YELLOW)),
                    Span::styled(n.clone(), Style::new().fg(TEXT)),
                    Span::styled(note, dim()),
                ]))
            })
            .collect();
        if items.is_empty() {
            f.render_widget(Paragraph::new("  no matching repos").style(dim()), list);
        } else {
            let mut st = ListState::default().with_selected(Some(p.sel.min(items.len() - 1)));
            f.render_stateful_widget(List::new(items).highlight_style(Style::new().bg(SURFACE).add_modifier(Modifier::BOLD)), list, &mut st);
        }
    }

    /// Reload state from disk/tmux, keeping the selection on the same row.
    fn refresh(&mut self) {
        self.entries = match tmux::live_panes() {
            Some(live) => sync::live_agents(&live)
                .into_iter()
                .map(|a| {
                    let p = live[&a.pane_id].clone();
                    Entry { a, p }
                })
                .collect(),
            None => vec![],
        };
        while let Ok(r) = self.repo_rx.try_recv() {
            self.repos = r;
        }
        if self.grid {
            self.tiles = self.entries.iter().map(|e| (e.a.pane_id.clone(), tmux::capture(&e.a.pane_id, 40))).collect();
        }
        self.rows = model::build(&self.entries, &self.repos, &self.pinned, &self.toggled, &self.filter);
        let idx = self
            .sel
            .as_ref()
            .and_then(|k| self.rows.iter().position(|r| &r.key == k))
            .or_else(|| self.list.selected().map(|i| i.min(self.rows.len().saturating_sub(1))))
            .or(Some(0))
            .filter(|_| !self.rows.is_empty());
        self.select(idx);
        self.refresh_peek();
    }

    fn select(&mut self, idx: Option<usize>) {
        self.list.select(idx);
        self.sel = idx.map(|i| self.rows[i].key.clone());
    }

    fn selected_entry(&self) -> Option<&Entry> {
        match self.rows.get(self.list.selected()?)?.kind {
            Kind::Agent { entry, .. } => Some(&self.entries[entry]),
            _ => None,
        }
    }

    fn move_by(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.list.selected().unwrap_or(0) as isize;
        self.select(Some((cur + d).clamp(0, self.rows.len() as isize - 1) as usize));
        self.refresh_peek();
    }

    fn refresh_peek(&mut self) {
        let pane = self.selected_entry().map(|e| e.a.pane_id.clone());
        self.peek_text = match (self.peek && !self.grid, &pane) {
            (true, Some(p)) => tmux::capture(p, interact::CAPTURE_LINES),
            _ => String::new(),
        };
        let seen = pane.and_then(|p| interact::parse(&self.peek_text).map(|d| (p, d.fingerprint)));
        if seen != self.seen {
            self.seen_at = Instant::now();
        }
        self.seen = seen;
    }

    fn say(&mut self, r: Result<String, String>) {
        self.msg = Some((r, Instant::now()));
    }

    fn decide(&mut self, d: Decision) {
        if self.grid {
            return self.say(Err("press m for the list view to approve or deny".into()));
        }
        let Some(e) = self.selected_entry() else {
            return self.say(Err("select an agent".into()));
        };
        let pane = e.a.pane_id.clone();
        if e.a.status != Status::NeedsInput {
            return self.say(Err("selected agent isn't waiting for input".into()));
        }
        if !self.peek {
            // Never act on a prompt the user hasn't had a chance to read.
            self.peek = true;
            self.refresh_peek();
            return self.say(Err("peek opened: read the prompt, then press again".into()));
        }
        let Some(seen) = self.seen.as_ref().filter(|(p, _)| *p == pane).map(|(_, f)| f.clone()) else {
            return self.say(Err("no permission dialog visible in peek".into()));
        };
        // A prompt that appeared a moment ago may not have been read yet.
        if self.seen_at.elapsed() < Duration::from_millis(800) {
            return self.say(Err("prompt just changed; read it, then press again".into()));
        }
        let r = interact::decide(&pane, d, &seen);
        self.say(r);
    }

    fn start_reply(&mut self) {
        if self.grid {
            return self.say(Err("press m for the list view to reply".into()));
        }
        match self.selected_entry().map(|e| e.a.status) {
            Some(Status::Working) => self.say(Err("agent is working".into())),
            Some(_) => self.input = Some((InputKind::Reply, String::new())),
            None => self.say(Err("select an agent".into())),
        }
    }

    /// Index into `repos` for the selected row's repo.
    fn selected_repo(&self) -> Option<usize> {
        let name = match &self.rows.get(self.list.selected()?)?.kind {
            Kind::Group { label, .. } => label.clone(),
            Kind::Agent { entry, .. } => model::group_of(&self.entries[*entry]),
            Kind::Worktree { repo, .. } => return Some(*repo),
        };
        self.repos.iter().position(|r| r.name == name)
    }

    fn start_worktree(&mut self) {
        match self.selected_repo() {
            Some(i) => self.input = Some((InputKind::Worktree(i), String::new())),
            None => self.say(Err("select a repo (or one of its rows) first".into())),
        }
    }

    fn start_chat(&mut self) {
        let cwd = self.selected_repo().map(|i| self.repos[i].path.clone()).unwrap_or_else(config::root);
        self.input = Some((InputKind::Chat(cwd), String::new()));
    }

    /// Run the pending footer input. Keeps the text on failure so it can be fixed.
    fn submit_input(&mut self) {
        let Some((kind, text)) = self.input.take() else { return };
        let client = self.client.clone();
        let r = match &kind {
            InputKind::Reply => match self.selected_entry().map(|e| e.a.pane_id.clone()) {
                Some(p) => interact::reply(&p, &text),
                None => Err("select an agent".into()),
            },
            InputKind::Worktree(i) => lifecycle::new_worktree(client.as_deref(), &self.repos[*i], &text, true)
                .map_err(|e| e.to_string()),
            InputKind::Chat(cwd) => {
                lifecycle::new_chat(client.as_deref(), &text, cwd).map_err(|e| e.to_string())
            }
        };
        match &r {
            Err(_) => self.input = Some((kind, text)),
            // New windows take focus; leave the popup so the user lands in them.
            Ok(_) if !matches!(kind, InputKind::Reply) => self.quit = true,
            Ok(_) => {}
        }
        self.say(r);
    }

    fn toggle_fold(&mut self) {
        if let Some(Row { key: Key::Group(g), .. }) = self.list.selected().and_then(|i| self.rows.get(i)) {
            let g = g.clone();
            if !self.toggled.remove(&g) {
                self.toggled.insert(g);
            }
            self.refresh();
        }
    }

    fn jump_to(&mut self, pane_id: &str) {
        if let Some(e) = self.entries.iter().find(|e| e.a.pane_id == pane_id) {
            tmux::jump(self.client.as_deref(), &e.p, pane_id);
            self.quit = true;
        }
    }

    fn jump_selected(&mut self) {
        match self.rows.get(self.list.selected().unwrap_or(usize::MAX)).map(|r| &r.key) {
            Some(Key::Agent(p)) => {
                let p = p.clone();
                self.jump_to(&p);
            }
            Some(Key::Group(_)) => self.toggle_fold(),
            Some(Key::Worktree(_)) => self.open_selected_worktree(),
            None => {}
        }
    }

    fn open_selected_worktree(&mut self) {
        let Some(&Kind::Worktree { repo, wt }) = self.list.selected().and_then(|i| self.rows.get(i)).map(|r| &r.kind) else {
            return;
        };
        let r = &self.repos[repo];
        match lifecycle::open_worktree(self.client.as_deref(), r, &r.worktrees[wt]) {
            Ok(()) => self.quit = true,
            Err(e) => self.say(Err(e.to_string())),
        }
    }

    /// Grid navigation: step through agent rows only.
    fn move_agent(&mut self, d: isize) {
        let agents: Vec<usize> = (0..self.rows.len()).filter(|&i| matches!(self.rows[i].kind, Kind::Agent { .. })).collect();
        if agents.is_empty() {
            return;
        }
        let cur = agents.iter().position(|&i| Some(i) == self.list.selected()).unwrap_or(0) as isize;
        let next = (cur + d).clamp(0, agents.len() as isize - 1) as usize;
        self.select(Some(agents[next]));
    }

    /// Longest-waiting agent that needs input.
    fn jump_next_waiting(&mut self) {
        let target = self
            .entries
            .iter()
            .filter(|e| e.a.status == Status::NeedsInput)
            .min_by_key(|e| e.a.since)
            .map(|e| e.a.pane_id.clone());
        if let Some(p) = target {
            self.jump_to(&p);
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if k.kind != KeyEventKind::Press {
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.picker.is_some() {
            self.on_picker_key(k);
            return;
        }
        if let Some((_, buf)) = self.input.as_mut() {
            match k.code {
                KeyCode::Enter => self.submit_input(),
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }
        if self.typing {
            match k.code {
                KeyCode::Enter => self.typing = false,
                KeyCode::Esc => {
                    self.filter.clear();
                    self.typing = false;
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => return,
            }
            self.refresh();
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc if self.filter.is_empty() => self.quit = true,
            KeyCode::Esc => {
                self.filter.clear();
                self.refresh();
            }
            KeyCode::Char('/') => self.typing = true,
            KeyCode::Char('m') => {
                self.grid = !self.grid;
                self.refresh();
            }
            KeyCode::Char('P') => self.picker = Some(Picker { query: String::new(), sel: 0 }),
            KeyCode::Char('w') => self.start_worktree(),
            KeyCode::Char('c') => self.start_chat(),
            KeyCode::Char('h') | KeyCode::Left if self.grid => self.move_agent(-1),
            KeyCode::Char('l') | KeyCode::Right if self.grid => self.move_agent(1),
            KeyCode::Char('j') | KeyCode::Down if self.grid => self.move_agent(self.grid_cols as isize),
            KeyCode::Char('k') | KeyCode::Up if self.grid => self.move_agent(-(self.grid_cols as isize)),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('g') | KeyCode::Home => self.move_by(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.move_by(isize::MAX / 2),
            KeyCode::Tab | KeyCode::Char(' ') => self.toggle_fold(),
            KeyCode::Enter => self.jump_selected(),
            KeyCode::Char('n') => self.jump_next_waiting(),
            KeyCode::Char('a') => self.decide(Decision::Allow),
            KeyCode::Char('d') => self.decide(Decision::Deny),
            KeyCode::Char('r') => self.start_reply(),
            KeyCode::Char('p') => {
                self.peek = !self.peek;
                self.refresh_peek();
            }
            _ => {}
        }
    }

    fn render(&mut self, f: &mut Frame) {
        let [head, main, foot] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
        let peek_w = if self.peek && !self.grid { main.width * 11 / 20 } else { 0 };
        let [body, peek] = Layout::horizontal([Constraint::Min(20), Constraint::Length(peek_w)]).areas(main);

        let count = |s| self.entries.iter().filter(|e| e.a.status == s).count();
        let mut title = vec![Span::styled(" ◆ cc-hud ", Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD))];
        for (s, word) in [(Status::NeedsInput, "need you"), (Status::Working, "working"), (Status::Idle, "idle")] {
            let n = count(s);
            let st = if n == 0 { Style::new().fg(FAINT) } else { status_style(s) };
            title.push(Span::styled(format!("  {} {n}", s.glyph()), st));
            title.push(Span::styled(format!(" {word}"), if n == 0 { Style::new().fg(FAINT) } else { dim() }));
        }
        f.render_widget(Line::from(title), head);

        if self.grid {
            self.render_grid(f, main);
            self.render_footer(f, foot);
            self.render_picker(f);
            return;
        }
        let now = state::now();
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| match &r.kind {
                Kind::Group { label, collapsed, counts } => {
                    let mut spans = vec![
                        Span::styled(if *collapsed { "▸ " } else { "▾ " }, Style::new().fg(ACCENT)),
                        Span::styled(label.clone(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
                    ];
                    for (n, s) in counts.iter().zip([Status::NeedsInput, Status::Working, Status::Idle]) {
                        if *n > 0 {
                            spans.push(Span::styled(format!("  {} {n}", s.glyph()), status_style(s)));
                        }
                    }
                    if counts.iter().all(|n| *n == 0) {
                        spans.push(Span::styled("  no agents", Style::new().fg(FAINT)));
                    }
                    ListItem::new(Line::from(spans))
                }
                Kind::Worktree { repo, wt } => {
                    let w = &self.repos[*repo].worktrees[*wt];
                    ListItem::new(Line::from(vec![
                        Span::styled("  ╰ ", Style::new().fg(FAINT)),
                        Span::styled(format!("{:<22}", w.branch.as_deref().unwrap_or("(detached)")), Style::new().fg(TEXT)),
                        Span::styled("no agent · ⏎ opens a shell", Style::new().fg(FAINT)),
                    ]))
                }
                Kind::Agent { entry, show_label } => {
                    let e = &self.entries[*entry];
                    let label = if *show_label {
                        if e.p.is_chat() {
                            e.p.window_name.clone()
                        } else {
                            e.a.branch.clone().unwrap_or_else(|| e.p.window_name.clone())
                        }
                    } else {
                        String::new()
                    };
                    let task = e.a.task.clone().unwrap_or_default();
                    let what = match e.a.status {
                        Status::NeedsInput => e.a.message.clone().unwrap_or(task),
                        Status::Working => match &e.a.tool {
                            Some(t) => format!("{task} › {t}"),
                            None => task,
                        },
                        Status::Idle => task,
                    };
                    let st = status_style(e.a.status);
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("  {} ", e.a.status.glyph()), st),
                        Span::styled(format!("{label:<22}"), Style::new().fg(TEXT)),
                        Span::styled(format!("{:<12}", e.a.status.label()), st),
                        Span::styled(format!("{:>4}  ", age(now.saturating_sub(e.a.since))), Style::new().fg(FAINT)),
                        Span::styled(what, dim()),
                    ]))
                }
            })
            .collect();
        let list_block = panel().title(Span::styled(" agents ", Style::new().fg(DIM)));
        if items.is_empty() {
            let msg = "no agents running (or none match the filter)";
            f.render_widget(Paragraph::new(format!("\n  {msg}")).style(Style::new().fg(FAINT)).block(list_block), body);
        } else {
            let inner = list_block.inner(body);
            f.render_widget(list_block, body);
            f.render_stateful_widget(
                List::new(items)
                    .highlight_symbol("▍")
                    .highlight_style(Style::new().bg(SURFACE).fg(ACCENT).add_modifier(Modifier::BOLD)),
                inner,
                &mut self.list,
            );
        }

        if self.peek && !self.grid {
            let title = match self.selected_entry() {
                Some(e) => Line::from(vec![
                    Span::styled(format!(" {} ", e.a.status.glyph()), status_style(e.a.status)),
                    Span::styled(format!("{}:{} ", e.p.session, e.p.window_name), Style::new().fg(TEXT)),
                    Span::styled(format!("{} ", e.a.pane_id), Style::new().fg(FAINT)),
                ]),
                None => Line::styled(" peek ", dim()),
            };
            let block = panel().title(title);
            let inner = block.inner(peek);
            let lines: Vec<&str> = self.peek_text.trim_end().lines().collect();
            let tail = lines[lines.len().saturating_sub(inner.height as usize)..].join("\n");
            let body_text = if lines.is_empty() {
                Paragraph::new("\n  select an agent to peek at its pane").style(Style::new().fg(FAINT))
            } else {
                Paragraph::new(tail).style(Style::new().fg(TEXT))
            };
            f.render_widget(body_text.block(block), peek);
        }

        self.render_footer(f, foot);
        self.render_picker(f);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let fresh = self.msg.as_ref().filter(|(_, t)| t.elapsed() < Duration::from_secs(4));
        let prompt = |label: String, buf: &str| -> Vec<Span<'static>> {
            vec![
                Span::styled(format!(" {label} "), Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {buf}"), Style::new().fg(TEXT)),
                Span::styled("▏", Style::new().fg(ACCENT)),
            ]
        };
        let foot_line = if let Some((kind, buf)) = &self.input {
            let label = match kind {
                InputKind::Reply => format!(
                    "reply to {}",
                    self.selected_entry().map(|e| e.p.window_name.clone()).unwrap_or_default()
                ),
                InputKind::Worktree(i) => format!("new branch in {}", self.repos[*i].name),
                InputKind::Chat(_) => "new chat topic".to_string(),
            };
            let mut spans = prompt(label, buf);
            if let Some((Err(e), _)) = fresh {
                spans.push(Span::styled(format!("   ✗ {e}"), Style::new().fg(RED)));
            }
            Line::from(spans)
        } else if let Some((r, _)) = fresh {
            match r {
                Ok(m) => Line::styled(format!(" ✓ {m}"), Style::new().fg(GREEN)),
                Err(m) => Line::styled(format!(" ✗ {m}"), Style::new().fg(RED)),
            }
        } else if self.typing || !self.filter.is_empty() {
            let mut spans = prompt("filter".into(), &self.filter);
            if !self.typing {
                spans.pop();
            }
            Line::from(spans)
        } else {
            let keys = [
                ("⏎", "jump"), ("a", "allow"), ("d", "deny"), ("r", "reply"), ("w", "worktree"), ("c", "chat"),
                ("P", "pin"), ("m", "grid"), ("/", "filter"), ("tab", "fold"), ("p", "peek"), ("n", "next"), ("q", "quit"),
            ];
            let mut spans = vec![];
            for (k, d) in keys {
                spans.push(Span::styled(format!(" {k} "), Style::new().fg(TEXT).bg(SURFACE).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(format!(" {d}  "), dim()));
            }
            Line::from(spans)
        };
        f.render_widget(foot_line, area);
    }

    /// Mission control: a live tile per agent. Read-only; enter jumps in.
    fn render_grid(&mut self, f: &mut Frame, area: Rect) {
        let agents: Vec<(usize, usize)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(ri, r)| match r.kind {
                Kind::Agent { entry, .. } => Some((ri, entry)),
                _ => None,
            })
            .collect();
        if agents.is_empty() {
            f.render_widget(Paragraph::new("\n  no agents running (or none match the filter)").style(Style::new().fg(FAINT)), area);
            return;
        }
        let cols = (agents.len() as f64).sqrt().ceil() as usize;
        let nrows = agents.len().div_ceil(cols);
        self.grid_cols = cols;
        let row_areas = Layout::vertical((0..nrows).map(|_| Constraint::Ratio(1, nrows as u32))).split(area);
        for (n, &(ri, ei)) in agents.iter().enumerate() {
            let cell = Layout::horizontal((0..cols).map(|_| Constraint::Ratio(1, cols as u32))).split(row_areas[n / cols])[n % cols];
            let e = &self.entries[ei];
            let selected = Some(ri) == self.list.selected();
            let name = if e.p.is_chat() {
                e.p.window_name.clone()
            } else {
                format!("{}/{}", e.a.repo.as_deref().unwrap_or("?"), e.a.branch.as_deref().unwrap_or(&e.p.window_name))
            };
            let color = status_color(e.a.status);
            let title = if selected {
                Span::styled(format!(" {} {name} ", e.a.status.glyph()), Style::new().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD))
            } else {
                Span::styled(format!(" {} {name} ", e.a.status.glyph()), Style::new().fg(color))
            };
            let text = self.tiles.get(&e.a.pane_id).map(String::as_str).unwrap_or("");
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            let h = cell.height.saturating_sub(2) as usize;
            let tail = lines[lines.len().saturating_sub(h)..].join("\n");
            let border = if selected { Style::new().fg(color) } else { Style::new().fg(FAINT) };
            f.render_widget(
                Paragraph::new(tail).style(Style::new().fg(TEXT)).block(
                    Block::bordered()
                        .border_type(if selected { BorderType::Thick } else { BorderType::Rounded })
                        .border_style(border)
                        .title(title),
                ),
                cell,
            );
        }
    }
}

pub fn run(client: Option<String>, grid: bool) -> Result<()> {
    // An unexpanded `#{client_name}` means the launcher didn't substitute it.
    let client = client.filter(|c| !c.contains("#{"));
    let mut term: DefaultTerminal = ratatui::init();
    let res = (|| -> Result<()> {
        let mut app = App::new(client, grid);
        while !app.quit {
            term.draw(|f| app.render(f))?;
            if event::poll(Duration::from_millis(300))? {
                if let Event::Key(k) = event::read()? {
                    app.on_key(k);
                }
            } else {
                app.refresh();
            }
        }
        Ok(())
    })();
    ratatui::restore();
    res
}

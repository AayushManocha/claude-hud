use crate::discover::{self, Repo};
use crate::interact::{self, Decision};
use crate::model::{self, Entry, Key, Kind, Row};
use crate::state::{self, Status};
use crate::{config, lifecycle, sync, tmux};
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
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
    pinned: Vec<String>,
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

fn status_style(s: Status) -> Style {
    match s {
        Status::NeedsInput => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        Status::Working => Style::new().fg(Color::Yellow),
        Status::Idle => Style::new().fg(Color::Green),
    }
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
            pinned: config::projects(),
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
        a.refresh();
        a
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
        let peek_w = if self.peek && !self.grid { main.width / 2 } else { 0 };
        let [body, peek] = Layout::horizontal([Constraint::Min(20), Constraint::Length(peek_w)]).areas(main);

        let count = |s| self.entries.iter().filter(|e| e.a.status == s).count();
        let mut title = vec![Span::styled(" cc-hud ", Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED))];
        for (s, word) in [(Status::NeedsInput, "need you"), (Status::Working, "working"), (Status::Idle, "idle")] {
            title.push(Span::styled(format!("  {} {} {}", s.glyph(), count(s), word), status_style(s)));
        }
        f.render_widget(Line::from(title), head);

        if self.grid {
            self.render_grid(f, body);
            self.render_footer(f, foot);
            return;
        }
        let now = state::now();
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| match &r.kind {
                Kind::Group { label, collapsed, counts } => {
                    let mut spans = vec![Span::styled(
                        format!("{} {label}", if *collapsed { "▸" } else { "▾" }),
                        Style::new().add_modifier(Modifier::BOLD),
                    )];
                    for (n, s) in counts.iter().zip([Status::NeedsInput, Status::Working, Status::Idle]) {
                        if *n > 0 {
                            spans.push(Span::styled(format!("  {}{n}", s.glyph()), status_style(s)));
                        }
                    }
                    if counts.iter().all(|n| *n == 0) {
                        spans.push(Span::styled("  no agents", Style::new().fg(Color::DarkGray)));
                    }
                    ListItem::new(Line::from(spans))
                }
                Kind::Worktree { repo, wt } => {
                    let w = &self.repos[*repo].worktrees[*wt];
                    ListItem::new(Line::from(vec![
                        Span::styled("   · ", Style::new().fg(Color::DarkGray)),
                        Span::raw(format!("{:<24}", w.branch.as_deref().unwrap_or("(detached)"))),
                        Span::styled("no agent — enter opens a shell", Style::new().fg(Color::DarkGray)),
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
                        Span::styled(format!("   {} ", e.a.status.glyph()), st),
                        Span::raw(format!("{label:<24}")),
                        Span::styled(format!("{:<12}", e.a.status.label()), st),
                        Span::styled(format!("{:>5}  ", age(now.saturating_sub(e.a.since))), Style::new().fg(Color::DarkGray)),
                        Span::raw(what),
                    ]))
                }
            })
            .collect();
        if items.is_empty() {
            f.render_widget(
                Paragraph::new("  no agents running (or none match the filter)").style(Style::new().fg(Color::DarkGray)),
                body,
            );
        } else {
            f.render_stateful_widget(
                List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
                body,
                &mut self.list,
            );
        }

        if self.peek && !self.grid {
            let title = self
                .selected_entry()
                .map(|e| format!(" {} · {}:{} ", e.a.pane_id, e.p.session, e.p.window_name))
                .unwrap_or_else(|| " peek (select an agent) ".into());
            let lines: Vec<&str> = self.peek_text.trim_end().lines().collect();
            let h = peek.height as usize;
            let tail = lines[lines.len().saturating_sub(h)..].join("\n");
            f.render_widget(Paragraph::new(tail).block(Block::new().borders(Borders::LEFT).title(title)), peek);
        }

        self.render_footer(f, foot);
    }
    fn render_footer(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let fresh = self.msg.as_ref().filter(|(_, t)| t.elapsed() < Duration::from_secs(4));
        let foot_line = if let Some((kind, buf)) = &self.input {
            let label = match kind {
                InputKind::Reply => format!(
                    "reply to {}",
                    self.selected_entry().map(|e| e.p.window_name.clone()).unwrap_or_default()
                ),
                InputKind::Worktree(i) => format!("new worktree branch in {}", self.repos[*i].name),
                InputKind::Chat(_) => "new chat topic".to_string(),
            };
            let mut spans = vec![
                Span::styled(format!(" {label}: "), Style::new().fg(Color::Cyan)),
                Span::raw(buf.clone()),
                Span::styled("█", Style::new().fg(Color::Cyan)),
            ];
            if let Some((Err(e), _)) = fresh {
                spans.push(Span::styled(format!("   ✗ {e}"), Style::new().fg(Color::Red)));
            }
            Line::from(spans)
        } else if let Some((r, _)) = fresh {
            match r {
                Ok(m) => Line::styled(format!(" ✓ {m}"), Style::new().fg(Color::Green)),
                Err(m) => Line::styled(format!(" ✗ {m}"), Style::new().fg(Color::Red)),
            }
        } else if self.typing || !self.filter.is_empty() {
            Line::from(vec![
                Span::styled(" /", Style::new().fg(Color::Cyan)),
                Span::raw(self.filter.clone()),
                Span::styled(if self.typing { "█" } else { "" }, Style::new().fg(Color::Cyan)),
            ])
        } else {
            Line::styled(
                " [enter] jump/open  [a]llow  [d]eny  [r]eply  [w]orktree  [c]hat  [m] grid  [/] filter  [tab] fold  [p] peek  [n] next  [q] quit",
                Style::new().fg(Color::DarkGray),
            )
        };
        f.render_widget(foot_line, area);
    }

    /// Mission control: a live tile per agent. Read-only; enter jumps in.
    fn render_grid(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
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
            f.render_widget(Paragraph::new("  no agents running (or none match the filter)").style(Style::new().fg(Color::DarkGray)), area);
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
            let mut border = status_style(e.a.status);
            if selected {
                border = border.add_modifier(Modifier::REVERSED);
            }
            let text = self.tiles.get(&e.a.pane_id).map(String::as_str).unwrap_or("");
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            let h = cell.height.saturating_sub(2) as usize;
            let tail = lines[lines.len().saturating_sub(h)..].join("\n");
            f.render_widget(
                Paragraph::new(tail).block(
                    Block::new()
                        .borders(Borders::ALL)
                        .border_style(status_style(e.a.status))
                        .title(Span::styled(format!(" {} {name} ", e.a.status.glyph()), border)),
                ),
                cell,
            );
        }
    }
}

pub fn run(client: Option<String>, grid: bool) -> Result<()> {
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

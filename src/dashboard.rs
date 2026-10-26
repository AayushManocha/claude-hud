use crate::discover::{self, Repo};
use crate::interact::{self, Decision};
use crate::model::{self, Entry, Key, Kind, Row};
use crate::state::{self, Status};
use crate::transcript::{self, Transcript};
use crate::tmux::PaneInfo;
use crate::{config, lifecycle, mux, sync, usage};
use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind, EnableFocusChange, DisableFocusChange,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What the footer input line is collecting.
enum InputKind {
    Reply,
    /// New worktree in repo `repos[i]`.
    Worktree(usize),
    Chat(PathBuf),
}

/// A destructive action waiting on a y/n answer.
enum Confirm {
    /// Remove a linked worktree: (repo name, path, label, force). `force` is asked for
    /// only after git refused because of uncommitted changes.
    Worktree(String, PathBuf, String, bool),
    /// Kill a chat's tmux session: (session name, label).
    Chat(String, String),
}

/// Outcome of a background worktree removal.
struct Removal {
    repo: String,
    path: PathBuf,
    label: String,
    force: bool,
    result: Result<String, String>,
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
    /// Worktrees being removed in the background, hidden from the list. The flag marks
    /// finished ones, kept until a rescan no longer lists them (an older scan may still).
    removing: HashMap<PathBuf, bool>,
    removal_tx: Sender<Removal>,
    removal_rx: Receiver<Removal>,
    toggled: HashSet<String>,
    grid: bool,
    grid_cols: usize,
    tiles: HashMap<String, String>,
    /// Screen captures are slow for some backends (Rex), so a thread takes them and keeps only
    /// the newest request; results are picked up by `apply_captures`.
    cap_tx: Sender<CaptureReq>,
    cap_rx: Receiver<CaptureRes>,
    peek_seq: u64,
    peek_applied: u64,
    tiles_pending: bool,
    filter: String,
    typing: bool,
    /// Pins from tmux config (read-only here) and pins saved from the dashboard.
    config_pins: Vec<String>,
    saved_pins: Vec<String>,
    pinned: Vec<String>,
    picker: Option<Picker>,
    peek: bool,
    peek_text: String,
    /// Same capture with colour codes, for display only.
    peek_ansi: String,
    /// Lines the peek is scrolled up from the bottom of the pane; 0 follows the live output.
    peek_scroll: usize,
    peek_pane: Option<String>,
    /// Session transcript shown instead of the pane while scrolled up, if the agent has one.
    transcript: Option<Transcript>,
    peek_area: Rect,
    /// Fingerprint of the dialog the user is currently looking at in the peek.
    seen: Option<(String, String)>,
    /// When `seen` last changed; decisions right after a change are refused.
    seen_at: Instant,
    input: Option<(InputKind, String)>,
    confirm: Option<Confirm>,
    usage: Option<usage::Usage>,
    msg: Option<(Result<String, String>, Instant)>,
    list: ListState,
    sel: Option<Key>,
    /// Latest pane listing, refreshed in the background.
    live: Option<HashMap<String, PaneInfo>>,
    live_rx: Receiver<Option<HashMap<String, PaneInfo>>>,
    /// Where the last frame drew things, for hit-testing mouse events.
    list_area: Rect,
    grid_cells: Vec<(Rect, usize)>,
    picker_list: Rect,
    picker_state: ListState,
    /// Row and time of the last click, to recognise a double-click.
    last_click: Option<(usize, Instant)>,
    quit: bool,
    /// A dashboard that stays running (a Rex tab): leaving it goes back instead of exiting,
    /// and it only polls while shown.
    persistent: bool,
    visible: Arc<AtomicBool>,
    wake: Vec<Sender<()>>,
}

/// Lines captured for the live peek, and lines moved per wheel tick. Scrolling back captures the whole scrollback.
const PEEK_LIVE: usize = 200;
const PEEK_SCROLL_STEP: usize = 3;
const TEXT: Color = Color::Rgb(235, 219, 178);
const DIM: Color = Color::Rgb(146, 131, 116);
const FAINT: Color = Color::Rgb(80, 73, 69);
const BACKDROP: Color = Color::Rgb(40, 40, 40);
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

/// Under Rex the dashboard can be a floating layer, and cells without a background colour let what
/// is underneath show through. Give every cell one (`CC_HUD_OPAQUE=0|1` overrides).
fn opaque() -> bool {
    match std::env::var("CC_HUD_OPAQUE").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => mux::active() == mux::Backend::Rex,
    }
}

fn fill_backdrop(f: &mut Frame, area: Rect) {
    if opaque() {
        f.render_widget(Block::new().style(Style::new().bg(BACKDROP)), area);
    }
}

/// Hold back until `ready`, but for no longer than a moment.
fn wait_ready(ready: &AtomicBool) {
    let start = Instant::now();
    while !ready.load(Ordering::Relaxed) && start.elapsed() < Duration::from_millis(1500) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

enum CaptureReq {
    Peek { seq: u64, pane: String, all: bool },
    Tiles(Vec<String>),
}

enum CaptureRes {
    Peek { seq: u64, pane: String, text: String, ansi: String },
    Tiles(HashMap<String, String>),
}

/// Takes screen captures off the UI thread. Requests that pile up while one is being served are
/// dropped in favour of the newest of each kind, so scrolling through the list never queues work.
fn spawn_capture_worker() -> (Sender<CaptureReq>, Receiver<CaptureRes>) {
    let (req_tx, req_rx) = mpsc::channel::<CaptureReq>();
    let (res_tx, res_rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(first) = req_rx.recv() {
            let (mut peek, mut tiles) = (None, None);
            for r in std::iter::once(first).chain(req_rx.try_iter()) {
                match r {
                    r @ CaptureReq::Peek { .. } => peek = Some(r),
                    r @ CaptureReq::Tiles(_) => tiles = Some(r),
                }
            }
            if let Some(CaptureReq::Peek { seq, pane, all }) = peek {
                let text = mux::capture(&pane, interact::CAPTURE_LINES);
                let ansi = if all { mux::capture_ansi_all(&pane) } else { mux::capture_ansi(&pane, PEEK_LIVE) };
                if res_tx.send(CaptureRes::Peek { seq, pane, text, ansi }).is_err() {
                    return;
                }
            }
            if let Some(CaptureReq::Tiles(panes)) = tiles {
                let tiles = panes.into_iter().map(|p| { let t = mux::capture_ansi(&p, 40); (p, t) }).collect();
                if res_tx.send(CaptureRes::Tiles(tiles)).is_err() {
                    return;
                }
            }
        }
    });
    (req_tx, res_rx)
}

fn live_cache_file() -> PathBuf {
    state::state_dir().join("live.json")
}

fn load_live_cache() -> Option<HashMap<String, PaneInfo>> {
    serde_json::from_slice(&std::fs::read(live_cache_file()).ok()?).ok()
}

/// Write `live` unless it is what `last` says was written already.
fn save_live_cache(live: &HashMap<String, PaneInfo>, last: &mut String) {
    // Sorted, so an unchanged listing serialises to the same text.
    let sorted: std::collections::BTreeMap<_, _> = live.iter().collect();
    let Ok(text) = serde_json::to_string(&sorted) else { return };
    if text != *last {
        let _ = std::fs::write(live_cache_file(), &text);
        *last = text;
    }
}

/// Truncate to `n` chars with an ellipsis.
fn fit(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).chain(['…']).collect()
    }
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
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

impl App {
    fn new(client: Option<String>, grid: bool, persistent: bool, ready: Arc<AtomicBool>) -> Self {
        let visible = Arc::new(AtomicBool::new(true));
        let (cap_tx, cap_rx) = spawn_capture_worker();
        // Listing panes can mean several subprocess calls (Rex, and slow while it is busy opening
        // this very popup), so it runs off the UI thread. The first frame is drawn from the
        // previous listing, kept on disk, rather than waiting for a fresh one.
        let live = load_live_cache();
        let (live_tx, live_rx) = mpsc::channel();
        let (live_wake, live_wake_rx) = mpsc::channel::<()>();
        let vis = visible.clone();
        let wait = ready.clone();
        std::thread::spawn(move || {
            wait_ready(&wait);
            let mut cached = String::new();
            loop {
                // Hidden: nothing to refresh until the dashboard is shown again.
                while !vis.load(Ordering::Relaxed) {
                    if live_wake_rx.recv().is_err() {
                        return;
                    }
                }
                let live = mux::live_panes();
                if let Some(l) = &live {
                    sync::live_agents(l); // prunes records of dead panes, against a listing that is fresh
                    save_live_cache(l, &mut cached);
                }
                if live_tx.send(live).is_err() {
                    break;
                }
                if let Err(RecvTimeoutError::Disconnected) = live_wake_rx.recv_timeout(Duration::from_millis(250)) {
                    break;
                }
            }
        });
        // Scanning runs git per repo, so keep it off the UI thread.
        let (tx, repo_rx) = mpsc::channel();
        let (scan_wake, scan_wake_rx) = mpsc::channel::<()>();
        let vis = visible.clone();
        std::thread::spawn(move || {
            wait_ready(&ready);
            const EVERY: Duration = Duration::from_secs(10);
            let mut last: Option<Instant> = None;
            loop {
                if last.is_none_or(|t| t.elapsed() >= EVERY) && vis.load(Ordering::Relaxed) {
                    if tx.send(discover::scan(&config::root())).is_err() {
                        break;
                    }
                    last = Some(Instant::now());
                }
                // Also wakes when shown again, to scan if the last scan is old.
                let wait = last.map_or(Duration::ZERO, |t| EVERY.saturating_sub(t.elapsed()));
                if let Err(RecvTimeoutError::Disconnected) = scan_wake_rx.recv_timeout(wait.max(Duration::from_millis(250))) {
                    break;
                }
            }
        });
        let (removal_tx, removal_rx) = mpsc::channel();
        let mut a = App {
            live,
            live_rx,
            client,
            entries: vec![],
            rows: vec![],
            repos: vec![],
            repo_rx,
            removing: HashMap::new(),
            removal_tx,
            removal_rx,
            toggled: HashSet::new(),
            grid,
            grid_cols: 1,
            tiles: HashMap::new(),
            cap_tx,
            cap_rx,
            peek_seq: 0,
            peek_applied: 0,
            tiles_pending: false,
            filter: String::new(),
            typing: false,
            pinned: vec![],
            config_pins: config::config_pins(),
            saved_pins: config::saved_pins(),
            picker: None,
            peek: true,
            peek_text: String::new(),
            peek_ansi: String::new(),
            peek_scroll: 0,
            peek_pane: None,
            transcript: None,
            peek_area: Rect::default(),
            seen: None,
            seen_at: Instant::now(),
            input: None,
            confirm: None,
            usage: None,
            msg: None,
            list: ListState::default(),
            sel: None,
            list_area: Rect::default(),
            grid_cells: vec![],
            picker_list: Rect::default(),
            picker_state: ListState::default(),
            last_click: None,
            quit: false,
            persistent,
            visible,
            wake: vec![live_wake, scan_wake],
        };
        a.pinned = a.all_pins();
        // Start on the agent the user was just working in; refresh keeps `sel` if that row exists.
        a.sel = mux::current_pane(a.client.as_deref()).map(Key::Agent);
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

    fn render_picker(&mut self, f: &mut Frame) {
        let Some(p) = &self.picker else { return };
        let (query, psel) = (p.query.clone(), p.sel);
        let a = f.area();
        let (w, h) = (a.width.min(60), a.height.min(20));
        let area = Rect::new(a.x + (a.width - w) / 2, a.y + (a.height - h) / 2, w, h);
        f.render_widget(Clear, area);
        fill_backdrop(f, area);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(ACCENT))
            .title(Span::styled(" pin repos ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
            .title_bottom(Line::from(Span::styled(" ⏎ toggle · esc close ", dim())).right_aligned());
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [q, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        self.picker_list = list;
        f.render_widget(
            Line::from(vec![
                Span::styled(" ", Style::new().fg(ACCENT)),
                Span::styled(query, Style::new().fg(TEXT)),
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
            self.picker_state.select(Some(psel.min(items.len() - 1)));
            let hl = Style::new().bg(SURFACE).add_modifier(Modifier::BOLD);
            f.render_stateful_widget(List::new(items).highlight_style(hl), list, &mut self.picker_state);
        }
    }

    /// Reload state from disk/tmux, keeping the selection on the same row.
    fn refresh(&mut self) {
        while let Ok(l) = self.live_rx.try_recv() {
            self.live = l;
        }
        self.entries = match &self.live {
            Some(live) => sync::known_agents(live)
                .into_iter()
                // An agent of a multiplexer that wasn't reachable has a record but no pane to show.
                .filter_map(|a| Some(Entry { p: live.get(&a.pane_id)?.clone(), a }))
                .collect(),
            None => vec![],
        };
        while let Ok(r) = self.removal_rx.try_recv() {
            self.on_removed(r);
        }
        while let Ok(r) = self.repo_rx.try_recv() {
            self.repos = r;
            let listed: HashSet<&PathBuf> = self.repos.iter().flat_map(|r| &r.worktrees).map(|w| &w.path).collect();
            self.removing.retain(|p, done| !*done || listed.contains(p));
        }
        for r in &mut self.repos {
            r.worktrees.retain(|w| !self.removing.contains_key(&w.path));
        }
        if self.grid {
            let panes = self.entries.iter().map(|e| e.a.pane_id.clone()).collect();
            if self.cap_tx.send(CaptureReq::Tiles(panes)).is_ok() {
                self.tiles_pending = true;
            }
        }
        self.usage = usage::load();
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

    /// One step up/down the list, wrapping past either end.
    fn step(&mut self, d: isize) {
        let n = self.rows.len() as isize;
        if n == 0 {
            return;
        }
        let cur = self.list.selected().unwrap_or(0) as isize;
        self.select(Some((cur + d).rem_euclid(n) as usize));
        self.refresh_peek();
    }

    fn move_by(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.list.selected().unwrap_or(0) as isize;
        self.select(Some((cur + d).clamp(0, self.rows.len() as isize - 1) as usize));
        self.refresh_peek();
    }

    /// Keep the transcript loaded and current while scrolled up; drop it when following live output.
    fn sync_transcript(&mut self) {
        if self.peek_scroll == 0 || !self.peek || self.grid {
            self.transcript = None;
            return;
        }
        let Some((cwd, sid)) = self.selected_entry().map(|e| (e.a.cwd.clone(), e.a.session_id.clone())) else {
            self.transcript = None;
            return;
        };
        let width = self.peek_area.width.saturating_sub(2);
        let fresh = self.transcript.as_ref().is_none_or(|t| t.session_id != sid);
        if fresh {
            self.transcript = transcript::find(&cwd, &sid).map(|path| Transcript::open(path, sid));
        }
        if let Some(t) = self.transcript.as_mut() {
            let before = t.lines(width).len();
            t.update();
            let after = t.lines(width).len();
            // Keep the lines being read in place as the conversation grows below them.
            if !fresh {
                self.peek_scroll += after - before;
            }
        }
    }

    /// Ask for the selected agent's screen. It arrives later, through `apply_captures`; until then
    /// the previous agent's screen is not shown, and `seen` is empty so nothing can be answered blind.
    fn refresh_peek(&mut self) {
        let pane = self.selected_entry().map(|e| e.a.pane_id.clone());
        if pane != self.peek_pane {
            self.peek_scroll = 0;
            self.peek_pane = pane.clone();
            self.peek_text.clear();
            self.peek_ansi.clear();
            self.seen = None;
        }
        self.sync_transcript();
        let Some(p) = pane.filter(|_| self.peek && !self.grid) else {
            self.peek_text.clear();
            self.peek_ansi.clear();
            self.seen = None;
            return;
        };
        let all = self.peek_scroll > 0 && self.transcript.is_none();
        self.peek_seq += 1;
        let req = CaptureReq::Peek { seq: self.peek_seq, pane: p, all };
        if self.cap_tx.send(req).is_err() {
            self.peek_applied = self.peek_seq;
        }
    }

    /// Take the captures that finished since the last call.
    fn apply_captures(&mut self) {
        while let Ok(res) = self.cap_rx.try_recv() {
            match res {
                CaptureRes::Peek { seq, pane, text, ansi } => {
                    self.peek_applied = seq;
                    if !self.peek || self.grid || self.peek_pane.as_ref() != Some(&pane) {
                        continue;
                    }
                    let seen = interact::parse(&text).map(|d| (pane, d.fingerprint));
                    if seen != self.seen {
                        self.seen_at = Instant::now();
                    }
                    self.seen = seen;
                    self.peek_text = text;
                    self.peek_ansi = ansi;
                }
                CaptureRes::Tiles(tiles) => {
                    self.tiles = tiles;
                    self.tiles_pending = false;
                }
            }
        }
    }

    fn captures_pending(&self) -> bool {
        self.peek_seq != self.peek_applied || self.tiles_pending
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

    /// The linked worktree under the selection: a worktree row, or an agent running inside one.
    /// Never the main checkout.
    fn selected_worktree(&self) -> Option<(usize, usize)> {
        let (ri, dir) = match &self.rows.get(self.list.selected()?)?.kind {
            Kind::Worktree { repo, wt } => return Some((*repo, *wt)).filter(|&(r, w)| self.repos[r].worktrees[w].path != self.repos[r].path),
            Kind::Agent { entry, .. } => {
                let e = &self.entries[*entry];
                if e.p.is_chat() {
                    return None;
                }
                let name = model::group_of(e);
                let ri = self.repos.iter().position(|r| r.name == name)?;
                (ri, std::fs::canonicalize(&e.a.cwd).unwrap_or_else(|_| PathBuf::from(&e.a.cwd)))
            }
            Kind::Group { .. } => return None,
        };
        let repo = &self.repos[ri];
        let wi = (0..repo.worktrees.len())
            .filter(|&w| dir.starts_with(&repo.worktrees[w].path))
            .max_by_key(|&w| repo.worktrees[w].path.as_os_str().len())?;
        (repo.worktrees[wi].path != repo.path).then_some((ri, wi))
    }

    fn start_remove(&mut self) {
        if let Some(e) = self.selected_entry().filter(|e| e.p.is_chat()) {
            self.confirm = Some(Confirm::Chat(e.p.session_key.clone(), model::label(e)));
            return;
        }
        let Some((ri, wi)) = self.selected_worktree() else {
            return self.say(Err("select a chat or linked worktree (the main checkout can't be removed)".into()));
        };
        let (r, w) = (&self.repos[ri], &self.repos[ri].worktrees[wi]);
        let label = format!("{}/{}", r.name, w.branch.as_deref().unwrap_or("detached"));
        self.confirm = Some(Confirm::Worktree(r.name.clone(), w.path.clone(), label, false));
    }

    fn confirm_remove(&mut self) {
        let (repo, path, force) = match self.confirm.take() {
            Some(Confirm::Worktree(repo, path, _, force)) => (repo, path, force),
            Some(Confirm::Chat(session, _)) => {
                let r = lifecycle::kill_chat(&session).map_err(|e| e.to_string());
                self.say(r);
                return self.refresh();
            }
            None => return,
        };
        let Some(ri) = self.repos.iter().position(|r| r.name == repo) else { return };
        let Some(wt) = self.repos[ri].worktrees.iter().find(|w| w.path == path).cloned() else { return };
        let label = format!("{}/{}", repo, wt.branch.as_deref().unwrap_or("detached"));
        // Hide the row now; git and the file deletion run off the UI thread.
        self.removing.insert(path.clone(), false);
        self.say(Ok(format!("removing {label}…")));
        let (r, tx, client) = (self.repos[ri].clone(), self.removal_tx.clone(), self.client.clone());
        std::thread::spawn(move || {
            let result = lifecycle::remove_worktree(client.as_deref(), &r, &wt, force).map_err(|e| e.to_string());
            let _ = tx.send(Removal { repo, path, label, force, result });
        });
        self.refresh();
    }

    fn on_removed(&mut self, r: Removal) {
        match r.result {
            Ok(m) => {
                self.removing.insert(r.path, true);
                self.say(Ok(m));
            }
            Err(e) => {
                // The worktree is intact; it reappears on the next rescan.
                self.removing.remove(&r.path);
                // Git names --force only for modified/untracked files; offer it instead of a dead end.
                if !r.force && e.contains("--force") {
                    self.confirm = Some(Confirm::Worktree(r.repo, r.path, r.label, true));
                } else {
                    self.say(Err(format!("{}: {e}", r.label)));
                }
            }
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
            Ok(_) if !matches!(kind, InputKind::Reply) => self.leave(false),
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

    /// Done with the dashboard. A plain one exits. A persistent one stays running for next time when
    /// it is put away with q/esc (`go_back`: it is still on screen and hands the screen back). After
    /// a jump or a new window the user is elsewhere, and it exits so no tab is left in the session
    /// they were in.
    fn leave(&mut self, go_back: bool) {
        if !self.persistent || !go_back {
            self.quit = true;
            return;
        }
        self.filter.clear();
        self.input = None;
        self.confirm = None;
        if let Err(e) = mux::leave_dashboard(self.client.as_deref()) {
            self.say(Err(e));
        }
    }

    fn shown(&mut self) {
        self.visible.store(true, Ordering::Relaxed);
        for w in &self.wake {
            let _ = w.send(());
        }
        self.refresh();
    }

    fn hidden(&mut self) {
        self.visible.store(false, Ordering::Relaxed);
    }

    fn jump_to(&mut self, pane_id: &str) {
        if let Some(e) = self.entries.iter().find(|e| e.a.pane_id == pane_id) {
            match mux::jump(self.client.as_deref(), &e.p, pane_id) {
                Ok(()) => self.leave(false),
                Err(e) => self.say(Err(e)),
            }
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
            Ok(()) => self.leave(false),
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
        let n = agents.len() as isize;
        let cols = self.grid_cols as isize;
        let t = cur + d;
        let next = if (0..n).contains(&t) {
            t
        } else if d.abs() == cols && cols > 1 {
            // Vertical wrap keeps the column: down off the bottom lands in the top row,
            // up off the top lands in the last row that has this column.
            let col = cur % cols;
            if d > 0 { col } else { (0..n).rev().find(|i| i % cols == col).unwrap_or(cur) }
        } else {
            t.rem_euclid(n)
        };
        self.select(Some(agents[next as usize]));
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

    /// Row index under a terminal cell in the list view.
    fn list_row_at(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.list_area;
        if x < a.x || x >= a.x + a.width || y < a.y || y >= a.y + a.height {
            return None;
        }
        Some(self.list.offset() + (y - a.y) as usize).filter(|&i| i < self.rows.len())
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        if self.confirm.is_some() || self.input.is_some() {
            return;
        }
        let (x, y) = (m.column, m.row);
        let up = matches!(m.kind, MouseEventKind::ScrollUp);
        let in_peek = self.peek && !self.grid && self.picker.is_none() && {
            let a = self.peek_area;
            x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height
        };
        if in_peek && matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
            let was = self.peek_scroll;
            self.peek_scroll = if up { was + PEEK_SCROLL_STEP } else { was.saturating_sub(PEEK_SCROLL_STEP) };
            // Leaving or entering the live view changes how much history is captured.
            if (was == 0) != (self.peek_scroll == 0) {
                self.refresh_peek();
            }
            return;
        }
        if self.picker.is_some() {
            let a = self.picker_list;
            let hit = x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height;
            match m.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    if let Some(p) = self.picker.as_mut() {
                        p.sel = if up { p.sel.saturating_sub(1) } else { p.sel + 1 };
                    }
                }
                MouseEventKind::Down(MouseButton::Left) if hit => {
                    let i = self.picker_state.offset() + (y - a.y) as usize;
                    if i < self.picker_matches().len() {
                        if let Some(p) = self.picker.as_mut() {
                            p.sel = i;
                        }
                        self.toggle_pin();
                    }
                }
                _ => {}
            }
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let d = if up { -1 } else { 1 };
                if self.grid {
                    self.move_agent(d * self.grid_cols as isize);
                } else {
                    self.move_by(d);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let idx = if self.grid {
                    self.grid_cells.iter().find(|(r, _)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height).map(|&(_, i)| i)
                } else {
                    self.list_row_at(x, y)
                };
                let Some(idx) = idx else { return };
                let double = self.last_click.take().is_some_and(|(i, t)| i == idx && t.elapsed() < Duration::from_millis(400));
                self.select(Some(idx));
                self.refresh_peek();
                if matches!(self.rows[idx].kind, Kind::Group { .. }) {
                    self.toggle_fold();
                } else if double {
                    self.jump_selected();
                } else {
                    self.last_click = Some((idx, Instant::now()));
                }
            }
            _ => {}
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
        if self.confirm.is_some() {
            if k.code == KeyCode::Char('y') {
                self.confirm_remove();
            } else {
                self.confirm = None;
            }
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
            KeyCode::Char('q') => self.leave(true),
            KeyCode::Esc if self.filter.is_empty() => self.leave(true),
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
            KeyCode::Char('x') => self.start_remove(),
            KeyCode::Char('h') | KeyCode::Left if self.grid => self.move_agent(-1),
            KeyCode::Char('l') | KeyCode::Right if self.grid => self.move_agent(1),
            KeyCode::Char('j') | KeyCode::Down if self.grid => self.move_agent(self.grid_cols as isize),
            KeyCode::Char('k') | KeyCode::Up if self.grid => self.move_agent(-(self.grid_cols as isize)),
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::Char('g') | KeyCode::Home => self.move_by(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.move_by(isize::MAX / 2),
            KeyCode::Tab => self.toggle_fold(),
            KeyCode::Enter | KeyCode::Char(' ') => self.jump_selected(),
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
        fill_backdrop(f, f.area());
        // Rex.app stretches the background of the last row into the padding below the grid, which
        // makes the footer's key caps look too tall. A blank row beneath it takes the stretch instead.
        let pad = u16::from(opaque());
        let [head, main, foot, _] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1), Constraint::Length(pad)])
                .areas(f.area());
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
        let title_w = title.iter().map(|s| s.content.chars().count()).sum::<usize>() as u16;
        let [head_l, head_r] = Layout::horizontal([Constraint::Length(title_w), Constraint::Min(0)]).areas(head);
        f.render_widget(Line::from(title), head_l);
        f.render_widget(Line::from(self.usage_spans(head_r.width as usize)).right_aligned(), head_r);

        self.list_area = Rect::default();
        self.peek_area = Rect::default();
        self.grid_cells.clear();
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
                    let label = if *show_label { fit(&model::label(e), 21) } else { String::new() };
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
            self.list_area = inner;
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
                    Span::styled(
                        if e.p.is_chat() { format!("{} ", model::chat_title(e)) } else { format!("{}:{} ", e.p.session, e.p.window_name) },
                        Style::new().fg(TEXT),
                    ),
                    Span::styled(format!("{} ", mux::short_id(&e.a.pane_id)), Style::new().fg(FAINT)),
                ]),
                None => Line::styled(" peek ", dim()),
            };
            self.peek_area = peek;
            let mut block = panel().title(title);
            if self.peek_scroll > 0 {
                block = block.title_bottom(Line::styled(format!(" ↑{} lines up ", self.peek_scroll), Style::new().fg(YELLOW)).right_aligned());
            }
            let inner = block.inner(peek);
            let mut lines = if self.peek_scroll > 0 && self.transcript.is_some() {
                vec![]
            } else {
                crate::ansi::lines(&self.peek_ansi)
            };
            while lines.last().is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty())) {
                lines.pop();
            }
            let body_text = if let (true, Some(t)) = (self.peek_scroll > 0, self.transcript.as_mut()) {
                let all = t.lines(inner.width);
                let max_scroll = all.len().saturating_sub(inner.height as usize);
                self.peek_scroll = self.peek_scroll.min(max_scroll);
                let skip = max_scroll - self.peek_scroll;
                let end = (skip + inner.height as usize).min(all.len());
                Paragraph::new(all[skip..end].to_vec())
            } else if lines.is_empty() {
                Paragraph::new("\n  select an agent to peek at its pane").style(Style::new().fg(FAINT))
            } else {
                let max_scroll = lines.len().saturating_sub(inner.height as usize);
                self.peek_scroll = self.peek_scroll.min(max_scroll);
                let skip = max_scroll - self.peek_scroll;
                let end = (skip + inner.height as usize).min(lines.len());
                Paragraph::new(lines[skip..end].to_vec())
            };
            f.render_widget(body_text.block(block), peek);
        }

        self.render_footer(f, foot);
        self.render_picker(f);
    }

    /// Plan limits for the header's right edge; empty if there's no room.
    fn usage_spans(&self, width: usize) -> Vec<Span<'static>> {
        let Some(u) = &self.usage else {
            let hint = "limits: run cc-hud install --write ";
            return if width > hint.len() { vec![Span::styled(hint, Style::new().fg(YELLOW))] } else { vec![] };
        };
        let now = state::now();
        let mut spans = vec![];
        for w in &u.windows {
            let color = if w.pct >= 85.0 { RED } else if w.pct >= 60.0 { YELLOW } else { GREEN };
            let filled = ((w.pct / 20.0).round() as usize).min(5);
            spans.push(Span::styled(format!("{} ", w.label), dim()));
            spans.push(Span::styled("█".repeat(filled), Style::new().fg(color)));
            spans.push(Span::styled("░".repeat(5 - filled), Style::new().fg(FAINT)));
            spans.push(Span::styled(format!(" {:.0}%", w.pct), Style::new().fg(color)));
            spans.push(Span::styled(format!(" ↻{}   ", age(w.resets_at.saturating_sub(now))), dim()));
        }
        // The numbers are as of a session's last response; say so when that's a while ago.
        let stale = now.saturating_sub(u.updated);
        if stale > 300 {
            spans.push(Span::styled(format!("({} old) ", age(stale)), Style::new().fg(FAINT)));
        }
        let w: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        if w > width { vec![] } else { spans }
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
        let foot_line = if let Some(c) = &self.confirm {
            let (title, what) = match c {
                Confirm::Worktree(_, _, l, false) => (" remove worktree ", format!("{l} and close its session")),
                Confirm::Worktree(_, _, l, true) => {
                    (" force remove ", format!("{l} has uncommitted or untracked files; discard them and close its session"))
                }
                Confirm::Chat(_, l) => (" kill chat ", format!("{l} and its session")),
            };
            Line::from(vec![
                Span::styled(title, Style::new().fg(Color::Black).bg(RED).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {what}? "), Style::new().fg(TEXT)),
                Span::styled("y", Style::new().fg(RED).add_modifier(Modifier::BOLD)),
                Span::styled("/any other key cancels", dim()),
            ])
        } else if let Some((kind, buf)) = &self.input {
            let label = match kind {
                InputKind::Reply => format!(
                    "reply to {}",
                    self.selected_entry().map(model::label).unwrap_or_default()
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
            // Most useful first; what doesn't fit the width is dropped from the end, never the way out.
            let leave = if self.persistent { "back" } else { "quit" };
            let keys = [
                ("⏎", "jump"), ("a", "allow"), ("d", "deny"), ("r", "reply"), ("w", "worktree"), ("c", "chat"), ("x", "kill/remove"),
                ("m", "grid"), ("/", "filter"), ("P", "pin"), ("p", "peek"), ("tab", "fold"), ("n", "next"),
            ];
            let width = |k: &str, d: &str| k.chars().count() + 2 + d.chars().count() + 3;
            let room = (area.width as usize).saturating_sub(width("q", leave));
            let mut used = 0;
            let mut shown: Vec<(&str, &str)> = vec![];
            for (k, d) in keys {
                if used + width(k, d) > room {
                    break;
                }
                used += width(k, d);
                shown.push((k, d));
            }
            shown.push(("q", leave));
            let mut spans = vec![];
            for (k, d) in shown {
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
            self.grid_cells.push((cell, ri));
            let name = if e.p.is_chat() {
                model::chat_title(e)
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
            let mut lines = crate::ansi::lines(text);
            lines.retain(|l| l.spans.iter().any(|s| !s.content.trim().is_empty()));
            let h = cell.height.saturating_sub(2) as usize;
            let tail = lines.split_off(lines.len().saturating_sub(h));
            let border = if selected { Style::new().fg(color) } else { Style::new().fg(FAINT) };
            f.render_widget(
                Paragraph::new(tail).block(
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

pub fn run(client: Option<String>, grid: bool, persistent: bool) -> Result<()> {
    // An unexpanded `#{client_name}` means the launcher didn't substitute it.
    let client = client.filter(|c| !c.contains("#{"));
    let mut term: DefaultTerminal = ratatui::init();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    if persistent {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableFocusChange);
    }
    // Showing the tab is what the user is waiting for, so it goes first; the dashboard's other
    // startup work (listing panes, scanning repos) would only slow the Rex calls it makes.
    let ready = Arc::new(AtomicBool::new(!persistent));
    if persistent {
        let (client, ready) = (client.clone(), ready.clone());
        std::thread::spawn(move || {
            crate::rex::show_own_window(client.as_deref());
            ready.store(true, Ordering::Relaxed);
        });
    }
    let res = (|| -> Result<()> {
        let mut app = App::new(client, grid, persistent, ready);
        while !app.quit {
            let shown = app.visible.load(Ordering::Relaxed);
            app.apply_captures();
            if shown {
                term.draw(|f| app.render(f))?;
            }
            // While a capture is on its way, look again soon rather than at the next refresh.
            let wait = if !shown { 1000 } else if app.captures_pending() { 20 } else { 300 };
            if event::poll(Duration::from_millis(wait))? {
                match event::read()? {
                    Event::Key(k) => app.on_key(k),
                    Event::Mouse(m) => app.on_mouse(m),
                    Event::FocusGained if persistent => app.shown(),
                    Event::FocusLost if persistent => app.hidden(),
                    _ => {}
                }
            } else if shown && !app.captures_pending() {
                app.refresh();
            }
        }
        Ok(())
    })();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture, DisableFocusChange);
    ratatui::restore();
    res
}

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::{DefaultTerminal, Frame};

use crate::{human, shown};
use crate::known::{self, Kind, Tag};
use crate::tree::{Node, Scanner};

#[derive(PartialEq)]
enum View {
    Folders,
    Caches,
}

struct App {
    home: PathBuf,
    // base is the folder space shows.
    base: PathBuf,
    scanner: Arc<Scanner>,
    root: Option<Node>,
    scanned: Receiver<Option<Node>>,
    // came_from is the folder to select once the scan above base arrives.
    came_from: Option<PathBuf>,
    view: View,
    // folder is the open folder in the Folders view, as names below base.
    folder: Vec<String>,
    // cursors keeps the selected row of each folder above the open one.
    cursors: Vec<usize>,
    table: TableState,
    marked: BTreeSet<PathBuf>,
    status: Vec<String>,
    confirm: bool,
    cleaning: bool,
    disk: (u64, u64),
}

pub fn run(home: PathBuf, base: PathBuf) -> io::Result<()> {
    let scanner = Arc::new(Scanner::new(home.clone(), &base));
    let scan = scanner.clone();
    let scan_base = base.clone();
    let scanned = background(move || scan.scan(&scan_base));

    let mut app = App {
        disk: disk(&base),
        home,
        base,
        scanner,
        root: None,
        scanned,
        came_from: None,
        view: View::Folders,
        folder: Vec::new(),
        cursors: Vec::new(),
        table: TableState::default().with_selected(0),
        marked: BTreeSet::new(),
        status: Vec::new(),
        confirm: false,
        cleaning: false,
    };
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

impl App {
    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            if let Ok(root) = self.scanned.try_recv() {
                self.root = root;
                if let Some(path) = self.came_from.take() {
                    let row = self.rows().iter().position(|row| row.path == path);
                    self.table = TableState::default().with_selected(row.or(Some(0)));
                }
            }
            terminal.draw(|frame| self.draw(frame))?;
            if self.cleaning {
                self.clean();
                continue;
            }
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(());
            }
            if self.confirm {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => {
                        self.confirm = false;
                        self.cleaning = true;
                    }
                    KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => self.confirm = false,
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
                KeyCode::PageUp => self.move_by(-20),
                KeyCode::PageDown => self.move_by(20),
                KeyCode::Home | KeyCode::Char('g') => self.move_by(-1_000_000),
                KeyCode::End | KeyCode::Char('G') => self.move_by(1_000_000),
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.open(),
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => self.back(),
                KeyCode::Tab => self.switch_view(),
                KeyCode::Char(' ') => self.mark(),
                KeyCode::Char('d') => self.ask_clean(),
                KeyCode::Char('o') => self.reveal(),
                _ => {}
            }
        }
    }

    fn rows(&self) -> Vec<&Node> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        match self.view {
            View::Folders => self.current(root).children.iter().collect(),
            View::Caches => {
                let mut tagged = Vec::new();
                root.tagged(&mut tagged);
                tagged.sort_by(|a, b| b.size.cmp(&a.size));
                tagged
            }
        }
    }

    fn current<'a>(&self, root: &'a Node) -> &'a Node {
        let mut node = root;
        for name in &self.folder {
            match node.children.iter().find(|child| &child.name == name) {
                Some(child) => node = child,
                None => break,
            }
        }
        node
    }

    fn selected(&self) -> Option<&Node> {
        self.rows().get(self.table.selected()?).copied()
    }

    fn move_by(&mut self, delta: i64) {
        let count = self.rows().len() as i64;
        let row = self.table.selected().unwrap_or(0) as i64;
        self.table.select(Some((row + delta).clamp(0, (count - 1).max(0)) as usize));
    }

    fn open(&mut self) {
        let Some(node) = self.selected() else {
            return;
        };
        if self.view == View::Caches {
            let Ok(relative) = node.path.strip_prefix(&self.base) else {
                return;
            };
            let parts: Vec<String> = relative.iter().map(|part| part.to_string_lossy().into_owned()).collect();
            let path = node.path.clone();
            self.view = View::Folders;
            self.folder = parts[..parts.len() - 1].to_vec();
            self.cursors = vec![0; self.folder.len()];
            let row = self.rows().iter().position(|row| row.path == path);
            self.table = TableState::default().with_selected(row.or(Some(0)));
            return;
        }
        if node.children.is_empty() {
            return;
        }
        let name = node.name.clone();
        self.folder.push(name);
        self.cursors.push(self.table.selected().unwrap_or(0));
        self.table = TableState::default().with_selected(0);
    }

    fn back(&mut self) {
        if self.view != View::Folders {
            return;
        }
        if self.folder.pop().is_none() {
            self.up();
            return;
        }
        let row = self.cursors.pop().unwrap_or(0);
        self.table = TableState::default().with_selected(Some(row));
    }

    // up goes above base: it scans the parent folder again, but not base.
    fn up(&mut self) {
        let Some(parent) = self.base.parent().map(Path::to_path_buf) else {
            return;
        };
        let Some(root) = self.root.take() else {
            return;
        };
        self.came_from = Some(root.path.clone());
        self.base = parent;
        self.scanner.entries.store(0, Ordering::Relaxed);
        self.scanner.bytes.store(root.size, Ordering::Relaxed);
        let scanner = self.scanner.clone();
        self.scanned = background(move || scanner.scan_parent(root));
    }

    fn switch_view(&mut self) {
        self.view = match self.view {
            View::Folders => View::Caches,
            View::Caches => View::Folders,
        };
        self.table = TableState::default().with_selected(0);
    }

    fn mark(&mut self) {
        let Some(node) = self.selected() else {
            return;
        };
        let path = node.path.clone();
        if !self.marked.remove(&path) {
            self.marked.insert(path);
        }
        self.move_by(1);
    }

    fn ask_clean(&mut self) {
        if self.marked.is_empty() {
            let Some(node) = self.selected() else {
                return;
            };
            self.marked.insert(node.path.clone());
        }
        self.confirm = true;
    }

    fn reveal(&self) {
        if let Some(node) = self.selected() {
            _ = Command::new("open").arg("-R").arg(&node.path).status();
        }
    }

    // clean applies to each marked path what its tag says, and scans it again.
    fn clean(&mut self) {
        self.cleaning = false;
        let Some(root) = &mut self.root else {
            return;
        };
        let mut freed = 0;
        for path in std::mem::take(&mut self.marked) {
            let Some(node) = root.find(&path) else {
                continue;
            };
            let (tag, before) = (node.tag, node.size);
            if let Err(err) = known::clean(&self.home, tag, &path) {
                self.status.push(err);
            }
            root.refresh(&self.scanner, &path);
            if tag.is_none() {
                root.refresh(&self.scanner, &self.home.join(".Trash"));
            }
            freed += before.saturating_sub(root.find(&path).map_or(0, |node| node.size));
        }
        self.status.push(format!("Freed {}", human(freed)));
        self.disk = disk(&self.base);
        self.move_by(0);
    }

    fn draw(&mut self, frame: &mut Frame) {
        let status_height = self.status.len().min(3) as u16;
        let [header, body, status, keys] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(status_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let (free, total) = self.disk;
        let title = match self.view {
            View::Folders => shown(&self.home, &self.folder.iter().fold(self.base.clone(), |path, name| path.join(name))),
            View::Caches => "known folders".into(),
        };
        let size = match (&self.root, &self.view) {
            (Some(root), View::Folders) => human(self.current(root).size),
            (Some(_), View::Caches) => human(self.rows().iter().map(|node| node.size).sum()),
            (None, _) => String::new(),
        };
        let left = Line::from(vec![
            Span::styled("clean  ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(title),
            Span::styled(format!("  {size}"), Style::new().fg(Color::Cyan)),
        ]);
        let right = Line::from(format!("{} free of {}", human(free), human(total))).right_aligned();
        frame.render_widget(left, header);
        frame.render_widget(right, header);

        match &self.root {
            None => {
                let entries = self.scanner.entries.load(Ordering::Relaxed);
                let bytes = self.scanner.bytes.load(Ordering::Relaxed);
                let text = format!("Scanning {}  {entries} entries, {}", shown(&self.home, &self.base), human(bytes));
                frame.render_widget(Line::from(text).centered(), center(body, 60, 1));
            }
            Some(_) => self.draw_table(frame, body),
        }

        let lines: Vec<Line> = self.status.iter().rev().take(3).rev().map(|line| Line::from(line.as_str())).collect();
        frame.render_widget(Paragraph::new(lines).style(Style::new().fg(Color::Cyan)), status);

        let mut help = "↑↓ move  → open  ← back  tab folders/known  space mark  d clean  o Finder  q quit".to_string();
        if !self.marked.is_empty() {
            let marked: u64 = self.marked.iter().filter_map(|path| self.root.as_ref()?.find(path)).map(|node| node.size).sum();
            help = format!("{} marked, {}  ·  {help}", self.marked.len(), human(marked));
        }
        frame.render_widget(Line::styled(help, Style::new().add_modifier(Modifier::DIM)), keys);

        if self.confirm || self.cleaning {
            self.draw_confirm(frame);
        }
    }

    fn draw_table(&mut self, frame: &mut Frame, area: Rect) {
        let rows = self.rows();
        let largest = rows.first().map_or(1, |node| node.size.max(1));
        let parent = match (&self.root, &self.view) {
            (Some(root), View::Folders) => self.current(root).size.max(1),
            _ => rows.iter().map(|node| node.size).sum::<u64>().max(1),
        };
        let (bar_width, _) = columns(area.width);
        let lines: Vec<Row> = rows
            .iter()
            .map(|node| {
                let mark = if self.marked.contains(&node.path) { "● " } else { "  " };
                let name = match self.view {
                    View::Folders if node.is_dir => format!("{mark}{}/", node.name),
                    View::Folders => format!("{mark}{}", node.name),
                    View::Caches => {
                        format!("{mark}{}", shown(&self.home, &node.path))
                    }
                };
                let percent = node.size * 100 / parent;
                Row::new(vec![
                    Cell::from(name),
                    Cell::from(Line::from(human(node.size)).right_aligned()),
                    Cell::from(Line::from(format!("{percent}%")).right_aligned()).style(Style::new().add_modifier(Modifier::DIM)),
                    Cell::from(bar(node.size as f64 / largest as f64, bar_width as usize)).style(Style::new().fg(Color::Blue)),
                    tag_cell(node.tag),
                ])
            })
            .collect();
        let (bar_width, tag_width) = columns(area.width);
        let table = Table::new(
            lines,
            [Constraint::Fill(1), Constraint::Length(9), Constraint::Length(4), Constraint::Length(bar_width), Constraint::Length(tag_width)],
        )
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(table, area, &mut self.table);
    }

    fn draw_confirm(&self, frame: &mut Frame) {
        let Some(root) = &self.root else {
            return;
        };
        let mut lines = Vec::new();
        let mut total = 0;
        for path in &self.marked {
            let Some(node) = root.find(path) else {
                continue;
            };
            total += node.size;
            lines.push(Line::from(vec![
                Span::raw(format!("{:>9}  {}  ", human(node.size), shown(&self.home, path))),
                Span::styled(known::plan(node.tag), Style::new().fg(Color::Yellow)),
            ]));
        }
        lines.push(Line::raw(""));
        if self.cleaning {
            lines.push(Line::raw("Cleaning..."));
        } else {
            lines.push(Line::raw(format!("Clean {}?  y yes  n no", human(total))));
        }
        let width = lines.iter().map(|line| line.width()).max().unwrap_or(0) as u16 + 4;
        let area = center(frame.area(), width, lines.len() as u16 + 2);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(lines).block(Block::bordered().title(" clean ")), area);
    }
}

fn tag_cell(tag: Option<&Tag>) -> Cell<'static> {
    let Some(tag) = tag else {
        return Cell::from("");
    };
    let color = match tag.kind {
        Kind::Rebuilds => Color::Green,
        Kind::Downloads => Color::Yellow,
        Kind::Yours => Color::Magenta,
    };
    Cell::from(tag.describe()).style(Style::new().fg(color))
}

// columns sizes the bar and the tag to the width, so the name keeps at least 30
// columns: the tag goes first, then the bar shrinks.
fn columns(width: u16) -> (u16, u16) {
    let room = width.saturating_sub(30 + 9 + 4 + 4);
    let tag = if room >= 20 + 30 { 30 } else { 0 };
    (room.saturating_sub(tag).min(20), tag)
}

// bar draws the fraction with eighth blocks, so close sizes still look different.
fn bar(fraction: f64, width: usize) -> String {
    const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = (fraction.clamp(0.0, 1.0) * (width * 8) as f64).round() as usize;
    format!("{}{}", "█".repeat(eighths / 8), EIGHTHS[eighths % 8])
}

fn background(scan: impl FnOnce() -> Option<Node> + Send + 'static) -> Receiver<Option<Node>> {
    let (send, scanned) = mpsc::channel();
    thread::spawn(move || _ = send.send(scan()));
    scanned
}

fn center(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 2, width, height)
}

// disk returns the free and total bytes of the volume holding the path.
fn disk(home: &Path) -> (u64, u64) {
    let Ok(output) = Command::new("df").arg("-k").arg("-P").arg(home).output() else {
        return (0, 0);
    };
    let listing = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<u64> = listing.lines().nth(1).map_or(Vec::new(), |line| {
        line.split_whitespace().skip(1).take(3).filter_map(|field| field.parse().ok()).collect()
    });
    match fields[..] {
        [total, _, free] => (free * 1024, total * 1024),
        _ => (0, 0),
    }
}

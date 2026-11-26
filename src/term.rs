//! The terminal session: a PTY running the shell, and a reader thread that
//! feeds its output to alacritty_terminal's parser. Graphics commands are
//! taken out of the stream first (see graphics.rs). Replies the terminal
//! owes the program (cursor position, colours, images) go straight back to
//! the PTY from the thread. The shell's prompt marks (OSC 133) are kept in
//! `marks` (see marks.rs), the folder it reports (OSC 7) in `reported`.
use crate::graphics::{Action, Piece, Placement, Scanner, Store};
use crate::marks::{Finished, Marks};
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, Options, Shell};
use alacritty_terminal::vte::ansi::Processor;
use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// What the window must handle, sent from the terminal side.
pub enum Note {
    Title(Option<String>),
    Clipboard(alacritty_terminal::term::ClipboardType, String),
    ClipboardLoad(
        alacritty_terminal::term::ClipboardType,
        Arc<dyn Fn(&str) -> String + Sync + Send>,
    ),
    Bell,
    /// A command marked by the shell has finished.
    Finished(Finished),
    /// The shell reported a new folder on this machine (OSC 7).
    Cwd(std::path::PathBuf),
    Exit,
}

#[derive(Clone)]
pub struct Listener {
    tx: Sender<Note>,
    wake: i32,
    pty: Arc<Mutex<std::fs::File>>,
    size: Arc<Mutex<WindowSize>>,
    colors: Arc<Mutex<crate::palette::Palette>>,
}

pub fn poke(fd: i32) {
    let one: u64 = 1;
    unsafe { libc::write(fd, &one as *const u64 as *const libc::c_void, 8) };
}

impl Listener {
    fn send(&self, n: Note) {
        let _ = self.tx.send(n);
        poke(self.wake);
    }
    pub fn write(&self, s: &[u8]) {
        if let Ok(mut f) = self.pty.lock() {
            let _ = f.write_all(s);
        }
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(t) => self.send(Note::Title(Some(t))),
            Event::ResetTitle => self.send(Note::Title(None)),
            Event::ClipboardStore(k, s) => self.send(Note::Clipboard(k, s)),
            Event::ClipboardLoad(k, f) => self.send(Note::ClipboardLoad(k, f)),
            Event::PtyWrite(s) => self.write(s.as_bytes()),
            Event::Bell => self.send(Note::Bell),
            Event::ColorRequest(i, f) => {
                let p = self.colors.lock().map(|p| p.clone()).unwrap_or_default();
                let rgb = match i {
                    0..=255 => crate::palette::indexed(&p, i as u8),
                    256 => p.fg,
                    257 => p.bg,
                    258 => p.cursor,
                    _ => p.fg,
                };
                let c = alacritty_terminal::vte::ansi::Rgb {
                    r: (rgb >> 16) as u8,
                    g: (rgb >> 8) as u8,
                    b: rgb as u8,
                };
                self.write(f(c).as_bytes());
            }
            Event::TextAreaSizeRequest(f) => {
                let s = self.size.lock().map(|s| *s).unwrap_or(WindowSize {
                    num_lines: 24,
                    num_cols: 80,
                    cell_width: 8,
                    cell_height: 16,
                });
                self.write(f(s).as_bytes());
            }
            Event::Wakeup => poke(self.wake),
            Event::Exit | Event::ChildExit(_) => self.send(Note::Exit),
            _ => {}
        }
    }
}

pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    pub graphics: Arc<Mutex<Store>>,
    /// The prompt marks. Locked after `term` when both are needed.
    pub marks: Arc<Mutex<Marks>>,
    pub listener: Listener,
    pty: Arc<Mutex<tty::Pty>>,
    pub size: Arc<Mutex<WindowSize>>,
    /// The last folder the shell reported with OSC 7.
    reported: Arc<Mutex<Option<std::path::PathBuf>>>,
}

/// Grid size for the terminal model.
pub struct GridSize {
    pub cols: usize,
    pub lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Spawn {
    /// `SHELL` for the programs inside, when NIRUTERM runs a chosen shell.
    pub shell_env: Option<String>,
    pub program: Option<String>,
    pub args: Vec<String>,
    pub dir: Option<std::path::PathBuf>,
    pub scrollback: usize,
}

impl Session {
    pub fn start(
        spawn: Spawn,
        size: WindowSize,
        tx: Sender<Note>,
        wake: i32,
        colors: Arc<Mutex<crate::palette::Palette>>,
    ) -> Result<Session, String> {
        let mut env = std::collections::HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "niruterm".to_string());
        env.insert(
            "TERM_PROGRAM_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        );
        if let Some(sh) = spawn.shell_env {
            env.insert("SHELL".to_string(), sh);
        }
        let shell = spawn.program.map(|p| Shell::new(p, spawn.args));
        let opts = Options {
            shell,
            working_directory: spawn.dir,
            drain_on_exit: true,
            env,
        };
        let pty =
            tty::new(&opts, size, 0).map_err(|e| format!("could not start the shell: {e}"))?;
        let write_file = pty.file().try_clone().map_err(|e| e.to_string())?;
        let read_file = pty.file().try_clone().map_err(|e| e.to_string())?;
        // alacritty_terminal opens the PTY non-blocking for its own event
        // loop; this reader thread wants plain blocking reads.
        {
            use std::os::fd::AsRawFd;
            let fd = read_file.as_raw_fd();
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
                }
            }
        }
        let size_shared = Arc::new(Mutex::new(size));
        let listener = Listener {
            tx,
            wake,
            pty: Arc::new(Mutex::new(write_file)),
            size: size_shared.clone(),
            colors,
        };
        let history = spawn.scrollback;
        let config = Config {
            scrolling_history: spawn.scrollback,
            ..Default::default()
        };
        let dims = GridSize {
            cols: size.num_cols as usize,
            lines: size.num_lines as usize,
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &dims, listener.clone())));
        let graphics = Arc::new(Mutex::new(Store::default()));
        let marks = Arc::new(Mutex::new(Marks::default()));
        let reported = Arc::new(Mutex::new(None));
        let session = Session {
            term: term.clone(),
            graphics: graphics.clone(),
            marks: marks.clone(),
            listener: listener.clone(),
            pty: Arc::new(Mutex::new(pty)),
            size: size_shared.clone(),
            reported: reported.clone(),
        };
        let shared = Shared {
            term,
            graphics,
            marks,
            reported,
            listener,
            size: size_shared,
        };
        std::thread::spawn(move || reader(read_file, shared, history));
        Ok(session)
    }

    pub fn write(&self, bytes: &[u8]) {
        self.listener.write(bytes);
    }

    pub fn resize(&self, size: WindowSize) {
        if let Ok(mut s) = self.size.lock() {
            if (s.num_lines, s.num_cols, s.cell_width, s.cell_height)
                == (
                    size.num_lines,
                    size.num_cols,
                    size.cell_width,
                    size.cell_height,
                )
            {
                return;
            }
            *s = size;
        }
        if let Ok(mut p) = self.pty.lock() {
            use alacritty_terminal::event::OnResize;
            p.on_resize(size);
        }
        let dims = GridSize {
            cols: size.num_cols as usize,
            lines: size.num_lines as usize,
        };
        let mut t = self.term.lock();
        let alt = t.mode().contains(TermMode::ALT_SCREEN);
        match self.marks.lock() {
            Ok(mut m) if !m.is_empty() => {
                if alt {
                    // The main screen is rewrapped out of sight.
                    m.clear();
                    t.resize(dims);
                } else {
                    let saved = m.before_resize(t.grid());
                    t.resize(dims);
                    m.after_resize(t.grid(), saved);
                }
            }
            _ => t.resize(dims),
        }
    }

    /// The folder of what runs in the terminal (for "new window here"):
    /// the one the shell reported (OSC 7) while the shell itself is in the
    /// foreground, else the folder of the program in the foreground, else
    /// the shell's own.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        let (shell, fg) = {
            use std::os::fd::AsRawFd;
            let p = self.pty.lock().ok()?;
            let fg = unsafe { libc::tcgetpgrp(p.file().as_raw_fd()) };
            (p.child().id() as i32, fg)
        };
        let reported = self.reported.lock().ok().and_then(|r| r.clone());
        let proc_cwd = |pid: i32| std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
        if fg <= 0 || fg == shell {
            if let Some(d) = reported.clone().filter(|d| d.is_dir()) {
                return Some(d);
            }
        } else if let Some(d) = proc_cwd(fg) {
            return Some(d);
        }
        proc_cwd(shell).or(reported)
    }
}

/// What the reader thread shares with the window.
struct Shared {
    term: Arc<FairMutex<Term<Listener>>>,
    graphics: Arc<Mutex<Store>>,
    marks: Arc<Mutex<Marks>>,
    reported: Arc<Mutex<Option<std::path::PathBuf>>>,
    listener: Listener,
    size: Arc<Mutex<WindowSize>>,
}

/// The folder in an OSC 7 report (`file://host/path`, percent-encoded) when
/// it is on this machine: a report from a shell over SSH names a folder on
/// the other machine.
pub fn reported_folder(params: &[u8], host: &str) -> Option<std::path::PathBuf> {
    let s = std::str::from_utf8(params).ok()?;
    let rest = s.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (h, path) = rest.split_at(slash);
    if !(h.is_empty() || h.eq_ignore_ascii_case("localhost") || h.eq_ignore_ascii_case(host)) {
        return None;
    }
    Some(crate::x::decode_path(path))
}

/// This machine's name, as shells put it in OSC 7.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn reader(mut file: std::fs::File, shared: Shared, history: usize) {
    let Shared {
        term,
        graphics,
        marks: marks_arc,
        reported,
        listener,
        size,
    } = shared;
    let host = hostname();
    let mut parser: Processor = Processor::new();
    let mut scanner = Scanner::default();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                use std::os::fd::AsRawFd;
                let mut pfd = libc::pollfd {
                    fd: file.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                unsafe { libc::poll(&mut pfd, 1, -1) };
                continue;
            }
            Err(_) => break,
        };
        let mut t = term.lock();
        let mut pieces: Vec<Vec<u8>> = Vec::new();
        // Collect first: the scanner borrows the buffer.
        let mut order: Vec<Item> = Vec::new();
        let base = buf.as_ptr() as usize;
        scanner.feed(&buf[..n], |p| match p {
            Piece::Text(s) => order.push(Item::Text(s.as_ptr() as usize - base, s.len())),
            Piece::Owned(v) => {
                pieces.push(v);
                order.push(Item::Owned(pieces.len() - 1));
            }
            Piece::Command(c) => {
                pieces.push(c);
                order.push(Item::Command(pieces.len() - 1));
            }
            Piece::Mark(m) => {
                pieces.push(m);
                order.push(Item::Mark(pieces.len() - 1));
            }
            Piece::Cwd(c) => {
                if let Some(d) = reported_folder(&c, &host) {
                    if let Ok(mut r) = reported.lock() {
                        if r.as_ref() != Some(&d) {
                            *r = Some(d.clone());
                            listener.send(Note::Cwd(d));
                        }
                    }
                }
            }
        });
        // Marks are moved with the output only once the shell has sent one.
        let mut marks = marks_arc.lock().ok();
        let had_marks = marks.as_ref().is_some_and(|m| !m.is_empty());
        if let Some(m) = marks.as_mut().filter(|_| had_marks) {
            // Rows are reused once the history is full: one read that
            // scrolls through all of them leaves the marks nowhere.
            let g = t.grid();
            let ring = history + g.screen_lines();
            if n >= ring && !t.mode().contains(TermMode::ALT_SCREEN) {
                let newlines = buf[..n].iter().filter(|b| **b == b'\n').count();
                m.flood(n, newlines, g.columns(), ring);
            }
        }
        for item in order {
            match item {
                Item::Text(s, l) => parser.advance(&mut *t, &buf[s..s + l]),
                Item::Owned(k) => parser.advance(&mut *t, &pieces[k]),
                Item::Mark(k) => {
                    if t.mode().contains(TermMode::ALT_SCREEN) {
                        continue;
                    }
                    if let Some(f) = marks.as_mut().and_then(|m| m.record(&pieces[k], &t)) {
                        listener.send(Note::Finished(f));
                    }
                }
                Item::Command(k) => {
                    let cell = size
                        .lock()
                        .map(|s| (s.cell_width as u32, s.cell_height as u32))
                        .unwrap_or((8, 16));
                    let action = graphics.lock().map(|mut g| g.command(&pieces[k], cell));
                    match action {
                        Ok(Action::Reply(r)) => listener.write(r.as_bytes()),
                        Ok(Action::Place {
                            image,
                            cols,
                            rows,
                            move_cursor,
                            reply,
                        }) => {
                            let cursor = t.grid().cursor.point;
                            let abs = t.grid().history_size() as i64 + cursor.line.0 as i64;
                            if let Ok(mut g) = graphics.lock() {
                                g.placements.push(Placement {
                                    image,
                                    abs_line: abs,
                                    col: cursor.column.0,
                                    cols,
                                    rows,
                                });
                                // Placements far above the history are dropped.
                                let oldest = abs - 20_000;
                                g.placements.retain(|p| p.abs_line > oldest);
                            }
                            if move_cursor {
                                // As kitty: the cursor ends after the picture, on its last row.
                                let mut s = "\n".repeat(rows.saturating_sub(1));
                                if cols > 0 {
                                    s.push_str(&format!("\x1b[{cols}C"));
                                }
                                parser.advance(&mut *t, s.as_bytes());
                            }
                            if let Some(r) = reply {
                                listener.write(r.as_bytes());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(m) = marks.as_mut().filter(|m| !m.is_empty()) {
            if !t.mode().contains(TermMode::ALT_SCREEN) {
                m.sync(t.grid());
            }
        }
        drop(marks);
        drop(t);
        poke(listener.wake);
    }
    listener.send(Note::Exit);
}

/// One piece of a read, in order.
enum Item {
    Text(usize, usize),
    Owned(usize),
    Command(usize),
    Mark(usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_folder_report_counts_only_from_this_machine() {
        let r = |s: &str| reported_folder(s.as_bytes(), "yggdrasil");
        assert_eq!(r("file:///tmp/a%20b"), Some(PathBuf::from("/tmp/a b")));
        assert_eq!(r("file://localhost/srv"), Some(PathBuf::from("/srv")));
        assert_eq!(
            r("file://YGGDRASIL/home/%C3%A5"),
            Some(PathBuf::from("/home/å"))
        );
        assert_eq!(r("file://othala/home/x"), None);
        assert_eq!(r("http://yggdrasil/x"), None);
        assert_eq!(r("file://yggdrasil"), None);
    }
}

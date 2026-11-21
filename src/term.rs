//! The terminal session: a PTY running the shell, and a reader thread that
//! feeds its output to alacritty_terminal's parser. Graphics commands are
//! taken out of the stream first (see graphics.rs). Replies the terminal
//! owes the program (cursor position, colours, images) go straight back to
//! the PTY from the thread.
use crate::graphics::{Action, Piece, Placement, Scanner, Store};
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
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
    pub listener: Listener,
    pty: Arc<Mutex<tty::Pty>>,
    pub size: Arc<Mutex<WindowSize>>,
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
        let session = Session {
            term: term.clone(),
            graphics: graphics.clone(),
            listener: listener.clone(),
            pty: Arc::new(Mutex::new(pty)),
            size: size_shared.clone(),
        };
        std::thread::spawn(move || reader(read_file, term, graphics, listener, size_shared));
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
        self.term.lock().resize(dims);
    }

    /// The shell's current folder (for "new window here").
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        let pid = self.pty.lock().ok()?.child().id();
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
}

fn reader(
    mut file: std::fs::File,
    term: Arc<FairMutex<Term<Listener>>>,
    graphics: Arc<Mutex<Store>>,
    listener: Listener,
    size: Arc<Mutex<WindowSize>>,
) {
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
        let mut text_runs: Vec<(usize, usize)> = Vec::new();
        // Collect first: the scanner borrows the buffer.
        let mut order: Vec<Result<(usize, usize), usize>> = Vec::new();
        let base = buf.as_ptr() as usize;
        scanner.feed(&buf[..n], |p| match p {
            Piece::Text(s) => {
                let start = s.as_ptr() as usize - base;
                text_runs.push((start, s.len()));
                order.push(Ok((start, s.len())));
            }
            Piece::Owned(v) => {
                pieces.push(v);
                order.push(Err(pieces.len() - 1 + (1 << 30)));
            }
            Piece::Command(c) => {
                pieces.push(c);
                order.push(Err(pieces.len() - 1));
            }
        });
        for item in order {
            match item {
                Ok((s, l)) => parser.advance(&mut *t, &buf[s..s + l]),
                Err(k) if k >= 1 << 30 => parser.advance(&mut *t, &pieces[k - (1 << 30)]),
                Err(k) => {
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
        drop(t);
        poke(listener.wake);
    }
    listener.send(Note::Exit);
}

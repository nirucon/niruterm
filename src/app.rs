//! The NIRUTERM window: drawing the grid, keys, mouse, selection, clipboard,
//! images, theme and font changes; the history search, the keyboard hints,
//! prompt jumps and notifications; the paste confirmation, and the
//! scratchpad's folder.
use crate::canvas::{Canvas, Image, Rect};
use crate::font::Fonts;
use crate::palette::{self, Cursor, Palette, Settings};
use crate::term::{Note, Session, Spawn};
use crate::x::{Res, X};
use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{ClipboardType, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;

/// Turns pasted text into the escape sequence a program asked for (OSC 52).
type Formatter = Arc<dyn Fn(&str) -> String + Sync + Send>;

/// One cell copied out of the terminal while it is locked.
struct Cell {
    row: i32,
    col: i32,
    c: char,
    fg: u32,
    bg: u32,
    style: u8,
    wide: bool,
    flags: Flags,
    extra: Option<Vec<char>>,
    link: bool,
}

/// What the command line asks of the window.
#[derive(Default)]
pub struct Options {
    pub title: Option<String>,
    pub class: Option<String>,
    /// Keep the window open after the program ends, until a key is pressed.
    pub hold: bool,
}

/// A scaled picture and the picture it was scaled from.
type ScaledPicture = (Arc<Image>, Arc<Image>);

/// At most this many matches are kept by the history search.
const SEARCH_LIMIT: usize = 100_000;

/// The history search bar (Ctrl+Shift+F).
#[derive(Default)]
struct Search {
    query: String,
    matches: Vec<(Point, Point)>,
    current: Option<usize>,
    /// Output arrived since the matches were found.
    stale: bool,
}

/// A paste waiting for Enter (paste protection).
struct Confirm {
    text: String,
    reason: crate::paste::Reason,
}

/// Keyboard hints (Ctrl+Shift+E): a label on every link and path.
struct Hints {
    list: Vec<(String, crate::text::Hint)>,
    typed: String,
}

pub struct App {
    hold: bool,
    held: bool,
    x: X,
    cv: Canvas,
    fonts: Fonts,
    settings: Settings,
    pal: Palette,
    shared_pal: Arc<Mutex<Palette>>,
    session: Session,
    notes: std::sync::mpsc::Receiver<Note>,
    wake: i32,
    inotify: i32,
    dirty: bool,
    quit: bool,
    focused: bool,
    cols: usize,
    lines: usize,
    dpi: f32,
    base_points: f32,
    points: f32,
    selecting: bool,
    clicks: (Instant, u8, i32, i32),
    clip_text: String,
    primary_text: String,
    pending_load: Option<Formatter>,
    paste_wanted: bool,
    dead: Option<u32>,
    bell_until: Option<Instant>,
    blink_on: bool,
    blink_at: Instant,
    /// Scaled pictures by (image id, width, height), with the picture they
    /// were scaled from: a program that sends a new picture under the same id
    /// (NPLAY's album cover) must not get the old one back.
    scaled: HashMap<(u32, i32, i32), ScaledPicture>,
    theme_reload: Option<Instant>,
    last_draw: Instant,
    started: Instant,
    shown: bool,
    mouse_btn: Option<u8>,
    drop_source: Option<Window>,
    search: Option<Search>,
    hints: Option<Hints>,
    /// The urgency flag is set (a bell while the window was not focused).
    urgent: bool,
    /// A short message at the top right, until the time.
    toast: Option<(String, Instant)>,
    confirm: Option<Confirm>,
    /// This window is the scratchpad and keeps its folder.
    remember: bool,
    /// The folder kept last for the scratchpad.
    kept: Option<std::path::PathBuf>,
}

impl App {
    pub fn new(spawn: Spawn, opts: Options, started: Instant) -> Res<Self> {
        let settings = palette::settings();
        let pal = palette::load(&settings);
        // The window is opened at kitty's default size (1200 x 760).
        let x = X::open(
            1200,
            760,
            opts.title.as_deref().unwrap_or("NIRUTERM"),
            opts.class.as_deref().unwrap_or(""),
        )?;
        let dpi = crate::font::dpi_scale(x.xft_dpi());
        let mut fonts = Fonts::new(
            settings.font.as_deref(),
            settings.font_size,
            dpi,
            settings.line_height,
        )?;
        fonts.set_ligatures(settings.ligatures);
        let pad = settings.padding;
        let cols = ((1200 - 2 * pad) / fonts.cell_w).max(2) as usize;
        let lines = ((760 - 2 * pad) / fonts.cell_h).max(1) as usize;
        let wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        let inotify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if inotify >= 0 {
            use std::os::unix::ffi::OsStrExt;
            if let Some(dir) = palette::theme_file().parent() {
                if let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) {
                    unsafe {
                        libc::inotify_add_watch(
                            inotify,
                            c.as_ptr(),
                            libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE,
                        )
                    };
                }
            }
        }
        let (tx, notes) = std::sync::mpsc::channel();
        let shared_pal = Arc::new(Mutex::new(pal.clone()));
        let size = WindowSize {
            num_lines: lines as u16,
            num_cols: cols as u16,
            cell_width: fonts.cell_w as u16,
            cell_height: fonts.cell_h as u16,
        };
        let session = Session::start(spawn, size, tx, wake, shared_pal.clone())?;
        x.text_cursor();
        let _ = x.set_icon(&window_icons(&pal));
        x.map()?;
        let points = settings.font_size;
        let remember =
            settings.remember_folder && opts.class.as_deref() == Some(crate::folder::SCRATCHPAD);
        Ok(App {
            cv: Canvas::new(1200, 760),
            x,
            fonts,
            settings,
            pal,
            shared_pal,
            session,
            notes,
            wake,
            inotify,
            dirty: true,
            quit: false,
            hold: opts.hold,
            held: false,
            focused: true,
            cols,
            lines,
            dpi,
            base_points: points,
            points,
            selecting: false,
            clicks: (Instant::now() - Duration::from_secs(5), 0, -1, -1),
            clip_text: String::new(),
            primary_text: String::new(),
            pending_load: None,
            paste_wanted: false,
            dead: None,
            bell_until: None,
            blink_on: true,
            blink_at: Instant::now(),
            scaled: HashMap::new(),
            theme_reload: None,
            last_draw: Instant::now() - Duration::from_secs(1),
            started,
            shown: false,
            mouse_btn: None,
            drop_source: None,
            search: None,
            hints: None,
            urgent: false,
            toast: None,
            confirm: None,
            remember,
            kept: None,
        })
    }

    fn pad(&self) -> i32 {
        self.settings.padding
    }

    fn relayout(&mut self) {
        let pad = self.pad();
        let cols = ((self.cv.w - 2 * pad) / self.fonts.cell_w).max(2) as usize;
        let lines = ((self.cv.h - 2 * pad) / self.fonts.cell_h).max(1) as usize;
        self.cols = cols;
        self.lines = lines;
        self.session.resize(WindowSize {
            num_lines: lines as u16,
            num_cols: cols as u16,
            cell_width: self.fonts.cell_w as u16,
            cell_height: self.fonts.cell_h as u16,
        });
        self.scaled.clear();
        self.dirty = true;
    }

    fn set_points(&mut self, pt: f32) {
        self.points = pt.clamp(6.0, 48.0);
        self.fonts
            .set_size(self.points * self.dpi, self.settings.line_height);
        self.relayout();
    }

    fn reload_theme(&mut self) {
        self.settings = palette::settings();
        if self.fonts.ligatures() != self.settings.ligatures {
            self.fonts.set_ligatures(self.settings.ligatures);
            self.dirty = true;
        }
        let p = palette::load(&self.settings);
        if p != self.pal {
            self.pal = p.clone();
            if let Ok(mut s) = self.shared_pal.lock() {
                *s = p;
            }
            self.dirty = true;
        }
    }

    // ---------------------------------------------------------------- loop

    pub fn run(&mut self) -> Res<()> {
        let r = self.event_loop();
        // Also when the X server went away at logout.
        self.keep_folder(None);
        r
    }

    /// The scratchpad keeps its folder for the next start: the one the shell
    /// reported, or the one it is in now.
    fn keep_folder(&mut self, reported: Option<std::path::PathBuf>) {
        if !self.remember {
            return;
        }
        let Some(dir) = reported.or_else(|| self.session.cwd()) else {
            return;
        };
        if self.kept.as_ref() != Some(&dir)
            && crate::folder::save(&crate::folder::file(), &dir).is_ok()
        {
            self.kept = Some(dir);
        }
    }

    fn event_loop(&mut self) -> Res<()> {
        let xfd = {
            use std::os::fd::AsRawFd;
            self.x.c.stream().as_raw_fd()
        };
        while !self.quit {
            while let Some(ev) = self.x.c.poll_for_event()? {
                self.event(ev)?;
            }
            while let Ok(n) = self.notes.try_recv() {
                self.note(n);
            }
            if self.quit {
                break;
            }
            if self.theme_reload.is_some_and(|t| Instant::now() >= t) {
                self.theme_reload = None;
                self.reload_theme();
            }
            if self.bell_until.is_some_and(|t| Instant::now() >= t) {
                self.bell_until = None;
                self.dirty = true;
            }
            if self.toast.as_ref().is_some_and(|t| Instant::now() >= t.1) {
                self.toast = None;
                self.dirty = true;
            }
            if self.settings.cursor_blink
                && self.focused
                && self.blink_at.elapsed() >= Duration::from_millis(530)
            {
                self.blink_on = !self.blink_on;
                self.blink_at = Instant::now();
                self.dirty = true;
            }
            if self.dirty {
                // At most about 120 frames a second: a flood of output is
                // drawn as it settles, not once per read.
                let since = self.last_draw.elapsed();
                if since >= Duration::from_millis(8) {
                    self.draw();
                    self.x.present(&self.cv)?;
                    self.dirty = false;
                    self.last_draw = Instant::now();
                    if !self.shown {
                        self.shown = true;
                        if std::env::var_os("NIRUTERM_TIMING").is_some() {
                            eprintln!(
                                "niruterm: first frame after {:.1} ms",
                                self.started.elapsed().as_secs_f64() * 1000.0
                            );
                        }
                    }
                    continue;
                }
            }
            self.x.c.flush()?;
            let mut timeout: i32 = -1;
            let mut at = |d: Duration| {
                let ms = d.as_millis() as i32 + 1;
                timeout = if timeout < 0 { ms } else { timeout.min(ms) };
            };
            if self.dirty {
                at(Duration::from_millis(8).saturating_sub(self.last_draw.elapsed()));
            }
            if let Some(t) = self.theme_reload {
                at(t.saturating_duration_since(Instant::now()));
            }
            if let Some(t) = self.bell_until {
                at(t.saturating_duration_since(Instant::now()));
            }
            if let Some((_, t)) = &self.toast {
                at(t.saturating_duration_since(Instant::now()));
            }
            if self.settings.cursor_blink && self.focused {
                at(Duration::from_millis(530).saturating_sub(self.blink_at.elapsed()));
            }
            let mut fds = [
                libc::pollfd {
                    fd: xfd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.wake,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.inotify,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let n = if self.inotify >= 0 { 3 } else { 2 };
            unsafe { libc::poll(fds.as_mut_ptr(), n, timeout) };
            if fds[1].revents & libc::POLLIN != 0 {
                let mut v = 0u64;
                unsafe { libc::read(self.wake, &mut v as *mut u64 as *mut libc::c_void, 8) };
                self.dirty = true;
                if let Some(s) = self.search.as_mut() {
                    s.stale = true;
                }
            }
            if n == 3 && fds[2].revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 4096];
                while unsafe {
                    libc::read(
                        self.inotify,
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    )
                } > 0
                {}
                self.theme_reload = Some(Instant::now() + Duration::from_millis(150));
            }
        }
        Ok(())
    }

    fn note(&mut self, n: Note) {
        match n {
            Note::Title(t) => {
                let t = t.unwrap_or_else(|| "NIRUTERM".into());
                let _ = self.x.set_title(&t);
            }
            Note::Clipboard(kind, s) => {
                if kind == ClipboardType::Clipboard {
                    self.clip_text = s;
                    let _ = self.x.own_clipboard();
                } else {
                    self.primary_text = s;
                    let _ = self.x.own_selection(self.x.atoms.primary);
                }
            }
            Note::ClipboardLoad(kind, f) => {
                let which = if kind == ClipboardType::Clipboard {
                    self.x.atoms.clipboard
                } else {
                    self.x.atoms.primary
                };
                self.pending_load = Some(f);
                let _ = self.x.request_text(which);
            }
            Note::Bell => {
                self.bell_until = Some(Instant::now() + Duration::from_millis(90));
                self.dirty = true;
                if !self.focused && !self.urgent {
                    self.urgent = self.x.set_urgent(true).is_ok();
                }
            }
            Note::Finished(f) => {
                let after = self.settings.notify_after;
                if after > 0 && !self.focused && f.took >= Duration::from_secs(after) {
                    let summary = match f.status {
                        Some(0) | None => "Command finished".to_string(),
                        Some(n) => format!("Command failed (exit {n})"),
                    };
                    let took = crate::open::took(f.took);
                    let body = if f.command.is_empty() {
                        format!("took {took}")
                    } else {
                        format!("{}\ntook {took}", f.command)
                    };
                    crate::open::notify(&summary, &body);
                }
            }
            Note::Cwd(dir) => self.keep_folder(Some(dir)),
            Note::Exit if self.hold => {
                self.held = true;
                self.dirty = true;
            }
            Note::Exit => self.quit = true,
        }
    }

    fn event(&mut self, ev: Event) -> Res<()> {
        match ev {
            Event::Expose(e) if e.count == 0 => self.dirty = true,
            Event::ConfigureNotify(e) => {
                if e.width as i32 != self.cv.w || e.height as i32 != self.cv.h {
                    self.cv.resize(e.width as i32, e.height as i32);
                    self.relayout();
                }
            }
            Event::ClientMessage(e) if e.type_ == self.x.atoms.wm_protocols => {
                if e.data.as_data32()[0] == self.x.atoms.wm_delete {
                    self.quit = true;
                }
            }
            Event::ClientMessage(e) => self.xdnd(e.type_, e.data.as_data32()),
            Event::PropertyNotify(e) => {
                if e.window == self.x.root && e.atom == self.x.atoms.niruwm_theme {
                    // NIRUX rewrites the palette file after the theme change.
                    self.theme_reload = Some(Instant::now() + Duration::from_millis(400));
                }
            }
            Event::FocusIn(_) => {
                self.focused = true;
                if self.urgent {
                    self.urgent = false;
                    let _ = self.x.set_urgent(false);
                }
                if self.mode().contains(TermMode::FOCUS_IN_OUT) {
                    self.session.write(b"\x1b[I");
                }
                self.dirty = true;
            }
            Event::FocusOut(_) => {
                self.focused = false;
                // The scratchpad is hidden: a good moment to keep its folder.
                self.keep_folder(None);
                if self.mode().contains(TermMode::FOCUS_IN_OUT) {
                    self.session.write(b"\x1b[O");
                }
                self.dirty = true;
            }
            Event::SelectionRequest(e) => {
                let text = if e.selection == self.x.atoms.clipboard {
                    self.clip_text.clone()
                } else {
                    self.primary_text.clone()
                };
                let _ = self.x.answer_text(&e, &text);
            }
            Event::SelectionNotify(e) => self.got_selection(e.property, e.selection),
            Event::MappingNotify(e) if e.request != Mapping::POINTER => self.x.reload_keymap(),
            Event::KeyPress(_) if self.held => self.quit = true,
            Event::KeyPress(e) => self.key(e.detail, e.state),
            Event::ButtonPress(e) => {
                self.button(e.detail, e.event_x as i32, e.event_y as i32, e.state, true)
            }
            Event::ButtonRelease(e) => {
                self.button(e.detail, e.event_x as i32, e.event_y as i32, e.state, false)
            }
            Event::MotionNotify(e) => self.motion(e.event_x as i32, e.event_y as i32, e.state),
            _ => {}
        }
        Ok(())
    }

    fn mode(&self) -> TermMode {
        *self.session.term.lock().mode()
    }

    // ---------------------------------------------------------- clipboard

    fn got_selection(&mut self, property: u32, selection: u32) {
        if property == x11rb::NONE {
            self.pending_load = None;
            self.paste_wanted = false;
            return;
        }
        let Some(data) = self.x.take_selection() else {
            return;
        };
        if selection == self.x.atoms.xdnd_selection {
            // Dropped files: their paths, quoted for the shell.
            let (files, _) = crate::x::parse_uris(&data);
            let text: Vec<String> = files
                .iter()
                .map(|p| shell_quote(&p.to_string_lossy()))
                .collect();
            self.paste(&text.join(" "));
            if let Some(src) = self.drop_source.take() {
                self.x.send_client(
                    src,
                    self.x.atoms.xdnd_finished,
                    [self.x.win, 1, self.x.atoms.xdnd_copy, 0, 0],
                );
            }
            return;
        }
        let text = String::from_utf8_lossy(&data).into_owned();
        if let Some(f) = self.pending_load.take() {
            self.session.write(f(&text).as_bytes());
        } else if self.paste_wanted {
            self.paste_wanted = false;
            self.paste(&text);
        }
    }

    fn paste(&mut self, text: &str) {
        if let Some(s) = self.search.as_mut() {
            s.query.push_str(text.lines().next().unwrap_or(""));
            return self.search_update();
        }
        if text.is_empty() {
            return;
        }
        if self.settings.paste_protection {
            let mode = self.mode();
            let reason = crate::paste::check(
                text,
                mode.contains(TermMode::BRACKETED_PASTE),
                mode.contains(TermMode::ALT_SCREEN),
            );
            if let Some(reason) = reason {
                self.confirm = Some(Confirm {
                    text: text.to_string(),
                    reason,
                });
                self.dirty = true;
                return;
            }
        }
        self.paste_now(text);
    }

    /// Keys while a paste waits: Enter pastes it, Esc (or Ctrl+C) drops it.
    fn confirm_key(&mut self, sym: u32, ctrl: bool) {
        match sym {
            0xff0d | 0xff8d => {
                if let Some(c) = self.confirm.take() {
                    self.paste_now(&c.text);
                }
            }
            0xff1b => self.confirm = None,
            _ if ctrl && sym | 0x20 == 0x63 => self.confirm = None,
            _ => return,
        }
        self.dirty = true;
    }

    fn paste_now(&mut self, text: &str) {
        let mode = self.mode();
        let clean = text
            .replace("\x1b[201~", "")
            .replace("\r\n", "\r")
            .replace('\n', "\r");
        self.scroll_bottom();
        if mode.contains(TermMode::BRACKETED_PASTE) {
            self.session.write(b"\x1b[200~");
            self.session.write(clean.as_bytes());
            self.session.write(b"\x1b[201~");
        } else {
            // Without bracketed paste, control characters could run commands.
            let safe: String = clean
                .chars()
                .filter(|c| !c.is_control() || *c == '\r' || *c == '\t')
                .collect();
            self.session.write(safe.as_bytes());
        }
    }

    fn paste_from(&mut self, which: u32) {
        let own = if which == self.x.atoms.clipboard {
            &self.clip_text
        } else {
            &self.primary_text
        };
        if self
            .x
            .c
            .get_selection_owner(which)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.owner == self.x.win)
        {
            let t = own.clone();
            self.paste(&t);
            return;
        }
        self.paste_wanted = true;
        let _ = self.x.request_text(which);
    }

    fn copy_selection(&mut self, to_clipboard: bool) {
        let text = self.session.term.lock().selection_to_string();
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            return;
        };
        self.primary_text = text.clone();
        let _ = self.x.own_selection(self.x.atoms.primary);
        if to_clipboard {
            self.clip_text = text;
            let _ = self.x.own_clipboard();
        }
    }

    fn scroll_bottom(&mut self) {
        let mut t = self.session.term.lock();
        if t.grid().display_offset() != 0 {
            t.scroll_display(Scroll::Bottom);
            drop(t);
            self.dirty = true;
        }
    }

    // ---------------------------------------------------------------- keys

    fn key(&mut self, code: u8, state: KeyButMask) {
        let sym = self.x.keysym(code, state);
        let ctrl = state.contains(KeyButMask::CONTROL);
        let shift = state.contains(KeyButMask::SHIFT);
        let alt = state.contains(KeyButMask::MOD1);
        if (0xffe1..=0xffee).contains(&sym) || sym == 0xfe03 || sym == 0xff7e {
            return; // modifier keys alone
        }
        if self.confirm.is_some() {
            return self.confirm_key(sym, ctrl);
        }
        if self.hints.is_some() {
            return self.hint_key(sym);
        }
        if self.search.is_some() && self.search_key(sym, ctrl, shift) {
            return;
        }
        // Terminal shortcuts (as kitty).
        if ctrl && shift {
            match sym | 0x20 {
                0x63 => return self.copy_selection(true),               // C
                0x76 => return self.paste_from(self.x.atoms.clipboard), // V
                0x6e => return self.new_window(),                       // N
                0x66 => return self.open_search(),                      // F
                0x65 => return self.open_hints(),                       // E
                0x67 => return self.copy_last_output(),                 // G
                _ => {}
            }
            if !self.mode().contains(TermMode::ALT_SCREEN) {
                match sym {
                    0xff52 => return self.jump_prompt(true),  // Up
                    0xff54 => return self.jump_prompt(false), // Down
                    _ => {}
                }
            }
            match sym {
                0x2b | 0x3d | 0xffab => return self.set_points(self.points + 1.0), // +
                0x2d | 0x5f | 0xffad => return self.set_points(self.points - 1.0), // -
                0x29 | 0x30 | 0x3d0 => return self.set_points(self.base_points),   // 0 / )
                _ => {}
            }
        }
        if shift && !ctrl {
            let mut t = self.session.term.lock();
            let mode = *t.mode();
            if !mode.contains(TermMode::ALT_SCREEN) {
                let scroll = match sym {
                    0xff55 => Some(Scroll::PageUp),
                    0xff56 => Some(Scroll::PageDown),
                    0xff50 => Some(Scroll::Top),
                    0xff57 => Some(Scroll::Bottom),
                    _ => None,
                };
                if let Some(s) = scroll {
                    t.scroll_display(s);
                    drop(t);
                    self.dirty = true;
                    return;
                }
            }
            if sym == 0xff63 {
                drop(t);
                return self.paste_from(self.x.atoms.primary); // Shift+Insert
            }
        }
        let mode = self.mode();
        // Dead keys (Swedish ~ ^ ` ´ ¨) combine with the next letter.
        if (0xfe50..=0xfe5f).contains(&sym) {
            if let Some(prev) = self.dead.take() {
                if let Some(c) = dead_alone(prev) {
                    self.send_text(&c.to_string(), alt);
                }
                if prev == sym {
                    return;
                }
            }
            self.dead = Some(sym);
            return;
        }
        if let Some(bytes) = key_bytes(sym, ctrl, shift, alt, mode) {
            if let Some(d) = self.dead.take() {
                if let Some(c) = dead_alone(d) {
                    self.send_text(&c.to_string(), false);
                }
            }
            self.after_input();
            self.session.write(&bytes);
            return;
        }
        let Some(mut ch) = keysym_char(sym) else {
            return;
        };
        if let Some(d) = self.dead.take() {
            match compose(d, ch) {
                Some(c) => ch = c,
                None => {
                    if let Some(c) = dead_alone(d) {
                        self.send_text(&c.to_string(), false);
                    }
                }
            }
        }
        if ctrl {
            if let Some(b) = ctrl_byte(ch) {
                self.after_input();
                let mut v = Vec::new();
                if alt {
                    v.push(0x1b);
                }
                v.push(b);
                self.session.write(&v);
                return;
            }
        }
        self.send_text(&ch.to_string(), alt);
    }

    fn send_text(&mut self, s: &str, alt: bool) {
        self.after_input();
        if alt {
            self.session.write(b"\x1b");
        }
        self.session.write(s.as_bytes());
    }

    fn after_input(&mut self) {
        self.scroll_bottom();
        self.blink_on = true;
        self.blink_at = Instant::now();
        let mut t = self.session.term.lock();
        if t.selection.is_some() {
            t.selection = None;
            self.dirty = true;
        }
    }

    /// A short message at the top right for a moment.
    fn say(&mut self, text: &str) {
        self.toast = Some((
            text.to_string(),
            Instant::now() + Duration::from_millis(1800),
        ));
        self.dirty = true;
    }

    // -------------------------------------------------------------- search

    fn open_search(&mut self) {
        self.search = Some(Search::default());
        self.dirty = true;
    }

    /// Keys while the search bar is open; false lets a key through.
    fn search_key(&mut self, sym: u32, ctrl: bool, shift: bool) -> bool {
        match sym {
            0xff1b => {
                self.search = None;
                self.dirty = true;
            }
            0xff0d | 0xff8d => self.search_step(!shift),
            0xff52 => self.search_step(true),
            0xff54 => self.search_step(false),
            0xff08 => {
                if let Some(s) = self.search.as_mut() {
                    s.query.pop();
                }
                self.search_update();
            }
            _ if ctrl && shift && matches!(sym | 0x20, 0x63 | 0x76) => return false,
            _ if ctrl && shift && sym | 0x20 == 0x66 => {
                self.search = None;
                self.dirty = true;
            }
            _ if ctrl && sym | 0x20 == 0x75 => {
                // Ctrl+U empties the search, as in a shell.
                if let Some(s) = self.search.as_mut() {
                    s.query.clear();
                }
                self.search_update();
            }
            _ if ctrl => {}
            _ => {
                let c = keysym_char(sym).or_else(|| dead_alone(sym));
                if let (Some(c), Some(s)) = (c, self.search.as_mut()) {
                    s.query.push(c);
                    self.search_update();
                }
            }
        }
        true
    }

    /// Finds the query again in the whole history and picks the match
    /// nearest the bottom of the view.
    fn search_update(&mut self) {
        self.dirty = true;
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let t = self.session.term.lock();
        let top = -(t.grid().history_size() as i32);
        let bottom = t.grid().screen_lines() as i32 - 1;
        s.matches = crate::text::find(&t, &s.query, top, bottom, SEARCH_LIMIT);
        s.stale = false;
        let view_bottom = bottom - t.grid().display_offset() as i32;
        drop(t);
        s.current = s
            .matches
            .iter()
            .rposition(|m| m.0.line.0 <= view_bottom)
            .or((!s.matches.is_empty()).then_some(0));
        if let Some(p) = s.current.map(|i| s.matches[i].0) {
            self.reveal(p.line.0);
        }
    }

    /// Enter goes to the match above (older), Shift+Enter to the one below.
    fn search_step(&mut self, older: bool) {
        if self.search.as_ref().is_some_and(|s| s.stale) {
            self.search_update();
        }
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let n = s.matches.len();
        if n == 0 {
            return;
        }
        let i = match (s.current, older) {
            (Some(i), true) => (i + n - 1) % n,
            (Some(i), false) => (i + 1) % n,
            (None, _) => n - 1,
        };
        s.current = Some(i);
        let line = s.matches[i].0.line.0;
        self.reveal(line);
        self.dirty = true;
    }

    /// Scrolls the history so that a grid line is in view, in the middle
    /// when it was out of sight.
    fn reveal(&mut self, line: i32) {
        let mut t = self.session.term.lock();
        let offset = t.grid().display_offset() as i32;
        let lines = self.lines as i32;
        let (top, bottom) = (-offset, -offset + lines - 2);
        if line >= top && line <= bottom {
            return;
        }
        let want = (lines / 2 - line).clamp(0, t.grid().history_size() as i32);
        t.scroll_display(Scroll::Delta(want - offset));
        self.dirty = true;
    }

    // --------------------------------------------------------------- hints

    fn open_hints(&mut self) {
        let cwd = self.session.cwd().unwrap_or_else(|| "/".into());
        let home = palette::home();
        let t = self.session.term.lock();
        let first = -(t.grid().display_offset() as i32);
        let last = first + self.lines as i32 - 1;
        let found = crate::text::hints(&t, first, last, |p| crate::text::resolve(p, &cwd, &home));
        drop(t);
        if found.is_empty() {
            return self.say("No links or paths on the screen");
        }
        // The labels start at the bottom, where the newest output is.
        let labels = crate::text::labels(found.len());
        let n = labels.len();
        let list = found
            .into_iter()
            .rev()
            .take(n)
            .zip(labels)
            .map(|(h, l)| (l, h))
            .collect();
        self.hints = Some(Hints {
            list,
            typed: String::new(),
        });
        self.dirty = true;
    }

    /// Keys while the hints are shown: a label opens its link or path, the
    /// label in capitals types it instead, Esc goes back.
    fn hint_key(&mut self, sym: u32) {
        self.dirty = true;
        let Some(h) = self.hints.as_mut() else {
            return;
        };
        match sym {
            0xff1b => {
                self.hints = None;
                return;
            }
            0xff08 => {
                h.typed.pop();
                return;
            }
            _ => {}
        }
        let Some(c) = keysym_char(sym).filter(|c| c.is_ascii_alphabetic()) else {
            return;
        };
        h.typed.push(c.to_ascii_lowercase());
        if !h.list.iter().any(|(l, _)| l.starts_with(&h.typed)) {
            h.typed.pop();
            return;
        }
        let Some(i) = h.list.iter().position(|(l, _)| *l == h.typed) else {
            return;
        };
        let hint = h.list.swap_remove(i).1;
        self.hints = None;
        if c.is_ascii_uppercase() {
            // Text from the screen, a link or a path: no need to ask.
            return self.paste_now(&hint.text);
        }
        match hint.target {
            crate::text::Target::Url(u) => crate::open::xdg_open(&u),
            crate::text::Target::Path(p, line) => {
                if std::path::Path::new(&p).is_dir() {
                    crate::open::xdg_open(&p)
                } else {
                    crate::open::file(&p, line, self.settings.editor.as_deref())
                }
            }
        }
    }

    // ------------------------------------------------------ shell marks

    /// Scrolls the history to the prompt above or below the top of the view.
    fn jump_prompt(&mut self, up: bool) {
        let mut t = self.session.term.lock();
        let offset = t.grid().display_offset() as i32;
        let top = -offset;
        let (target, any) = match self.session.marks.lock() {
            Ok(m) => {
                let p = m.prompts();
                let target = if up {
                    p.filter(|&l| l < top).max()
                } else {
                    p.filter(|&l| l > top).min()
                };
                (target, !m.is_empty())
            }
            Err(_) => (None, false),
        };
        let want = match target {
            Some(l) => (-l).clamp(0, t.grid().history_size() as i32),
            None if up => {
                drop(t);
                return self.say(if any {
                    "No earlier prompt"
                } else {
                    "No prompt marks: niruprompt install bash|fish"
                });
            }
            None => 0,
        };
        t.scroll_display(Scroll::Delta(want - offset));
        self.dirty = true;
    }

    /// Ctrl+Shift+G: the output of the last command to the clipboard.
    fn copy_last_output(&mut self) {
        let t = self.session.term.lock();
        let out = self
            .session
            .marks
            .lock()
            .ok()
            .and_then(|m| m.last_output(&t));
        drop(t);
        match out {
            Some(text) if !text.is_empty() => {
                let n = text.lines().count();
                self.clip_text = text;
                let _ = self.x.own_clipboard();
                self.say(&format!(
                    "Copied the output ({n} line{})",
                    if n == 1 { "" } else { "s" }
                ));
            }
            Some(_) => self.say("The last command printed nothing"),
            None => self.say("No command output yet"),
        }
    }

    fn new_window(&mut self) {
        let mut c = std::process::Command::new(
            std::env::current_exe().unwrap_or_else(|_| "niruterm".into()),
        );
        if let Some(d) = self.session.cwd() {
            c.arg("--working-directory").arg(d);
        }
        use std::os::unix::process::CommandExt;
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let _ = c
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    // --------------------------------------------------------------- mouse

    fn cell_at(&self, x: i32, y: i32) -> (Point, Side) {
        let pad = self.pad();
        let col = ((x - pad).max(0) / self.fonts.cell_w).min(self.cols as i32 - 1);
        let row = ((y - pad).max(0) / self.fonts.cell_h).min(self.lines as i32 - 1);
        let rel = (x - pad) - col * self.fonts.cell_w;
        let side = if rel * 2 < self.fonts.cell_w {
            Side::Left
        } else {
            Side::Right
        };
        let offset = self.session.term.lock().grid().display_offset() as i32;
        (Point::new(Line(row - offset), Column(col as usize)), side)
    }

    fn report_mouse(
        &mut self,
        btn: u8,
        x: i32,
        y: i32,
        state: KeyButMask,
        press: bool,
        motion: bool,
    ) -> bool {
        let mode = self.mode();
        if !mode.intersects(TermMode::MOUSE_MODE) || state.contains(KeyButMask::SHIFT) {
            return false;
        }
        if motion && !mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG) {
            return true;
        }
        let pad = self.pad();
        let col = ((x - pad).max(0) / self.fonts.cell_w).min(self.cols as i32 - 1) + 1;
        let row = ((y - pad).max(0) / self.fonts.cell_h).min(self.lines as i32 - 1) + 1;
        let mut code: u32 = match btn {
            1 => 0,
            2 => 1,
            3 => 2,
            4 => 64,
            5 => 65,
            6 => 66,
            7 => 67,
            _ => 3,
        };
        if motion {
            code += 32;
        }
        if state.contains(KeyButMask::MOD1) {
            code += 8;
        }
        if state.contains(KeyButMask::CONTROL) {
            code += 16;
        }
        if mode.contains(TermMode::SGR_MOUSE) {
            let end = if press || motion { 'M' } else { 'm' };
            self.session
                .write(format!("\x1b[<{code};{col};{row}{end}").as_bytes());
        } else if col < 223 && row < 223 {
            let c = if press || motion { code } else { 3 };
            self.session.write(&[
                0x1b,
                b'[',
                b'M',
                (32 + c) as u8,
                (32 + col) as u8,
                (32 + row) as u8,
            ]);
        }
        true
    }

    fn button(&mut self, b: u8, x: i32, y: i32, state: KeyButMask, press: bool) {
        if press && self.hints.take().is_some() {
            self.dirty = true;
        }
        if self.report_mouse(b, x, y, state, press, false) {
            if press {
                self.mouse_btn = Some(b);
            } else {
                self.mouse_btn = None;
            }
            return;
        }
        if !press {
            if b == 1 && self.selecting {
                self.selecting = false;
                let to_clip = self.settings.copy_on_select;
                self.copy_selection(to_clip);
            }
            return;
        }
        match b {
            4 | 5 => {
                let mode = self.mode();
                if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL)
                {
                    // Programs on the alternate screen (less, man) get arrows.
                    let seq: &[u8] = match (b, mode.contains(TermMode::APP_CURSOR)) {
                        (4, true) => b"\x1bOA\x1bOA\x1bOA",
                        (4, false) => b"\x1b[A\x1b[A\x1b[A",
                        (_, true) => b"\x1bOB\x1bOB\x1bOB",
                        (_, false) => b"\x1b[B\x1b[B\x1b[B",
                    };
                    self.session.write(seq);
                } else {
                    let d = if b == 4 { 3 } else { -3 };
                    self.session.term.lock().scroll_display(Scroll::Delta(d));
                    self.dirty = true;
                }
            }
            1 => {
                let ctrl = state.contains(KeyButMask::CONTROL);
                if ctrl {
                    if let Some(url) = self.url_at(x, y) {
                        crate::open::xdg_open(&url);
                        return;
                    }
                }
                let (pt, side) = self.cell_at(x, y);
                let (t0, n, px, py) = self.clicks;
                let same = (x - px).abs() < 4
                    && (y - py).abs() < 4
                    && t0.elapsed() < Duration::from_millis(400);
                let count = if same { (n % 3) + 1 } else { 1 };
                self.clicks = (Instant::now(), count, x, y);
                let mut t = self.session.term.lock();
                if state.contains(KeyButMask::SHIFT) && t.selection.is_some() {
                    if let Some(s) = t.selection.as_mut() {
                        s.update(pt, side);
                    }
                } else {
                    let ty = match count {
                        2 => SelectionType::Semantic,
                        3 => SelectionType::Lines,
                        _ => SelectionType::Simple,
                    };
                    t.selection = Some(Selection::new(ty, pt, side));
                }
                drop(t);
                self.selecting = true;
                self.dirty = true;
            }
            2 => self.paste_from(self.x.atoms.primary),
            _ => {}
        }
    }

    fn motion(&mut self, x: i32, y: i32, state: KeyButMask) {
        if let Some(b) = self.mouse_btn {
            if self.report_mouse(b, x, y, state, true, true) {
                return;
            }
        }
        if self.selecting && state.contains(KeyButMask::BUTTON1) {
            let (pt, side) = self.cell_at(x, y);
            // Dragging above or below the window scrolls the history.
            let pad = self.pad();
            if y < pad {
                self.session.term.lock().scroll_display(Scroll::Delta(1));
            } else if y > self.cv.h - pad {
                self.session.term.lock().scroll_display(Scroll::Delta(-1));
            }
            if let Some(s) = self.session.term.lock().selection.as_mut() {
                s.update(pt, side);
            }
            self.dirty = true;
        }
    }

    /// A link under the pointer: an OSC 8 hyperlink or a plain URL.
    fn url_at(&self, x: i32, y: i32) -> Option<String> {
        let (pt, _) = self.cell_at(x, y);
        let t = self.session.term.lock();
        let grid = t.grid();
        let row = &grid[pt.line];
        if let Some(h) = row[pt.column].hyperlink() {
            return Some(h.uri().to_string());
        }
        let text: Vec<char> = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
        let col = pt.column.0;
        let is_url = |c: char| !c.is_whitespace() && !"<>\"'`|".contains(c);
        let mut s = col;
        while s > 0 && is_url(text[s - 1]) {
            s -= 1;
        }
        let mut e = col;
        while e < text.len() && is_url(text[e]) {
            e += 1;
        }
        let word: String = text[s..e].iter().collect();
        let word = word.trim_end_matches(['.', ',', ')', ']', ';', ':']);
        ["https://", "http://", "file://", "ftp://", "mailto:"]
            .iter()
            .find_map(|p| word.find(p).map(|i| word[i..].to_string()))
    }

    // ------------------------------------------------------- drag and drop

    fn xdnd(&mut self, t: u32, d: [u32; 5]) {
        let a = &self.x.atoms;
        let win = self.x.win;
        if t == a.xdnd_position {
            self.x
                .send_client(d[0], a.xdnd_status, [win, 1, 0, 0, a.xdnd_copy]);
        } else if t == a.xdnd_drop {
            self.drop_source = Some(d[0]);
            let _ = self
                .x
                .c
                .convert_selection(win, a.xdnd_selection, a.uri_list, a.sel_prop, d[2]);
            let _ = self.x.c.flush();
        }
    }

    // ---------------------------------------------------------------- draw

    fn color(&self, c: Color, colors: &alacritty_terminal::term::color::Colors) -> u32 {
        match c {
            Color::Spec(rgb) => (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32,
            Color::Indexed(i) => match colors[i as usize] {
                Some(rgb) => (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32,
                None => palette::indexed(&self.pal, i),
            },
            Color::Named(n) => {
                let idx = n as usize;
                if let Some(rgb) = colors[idx] {
                    return (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32;
                }
                match n {
                    NamedColor::Foreground | NamedColor::BrightForeground => self.pal.fg,
                    NamedColor::Background => self.pal.bg,
                    NamedColor::Cursor => self.pal.cursor,
                    NamedColor::DimForeground => crate::canvas::mix(self.pal.fg, self.pal.bg, 0.33),
                    _ if idx < 16 => self.pal.ansi[idx],
                    _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize)
                        .contains(&idx) =>
                    {
                        let base = self.pal.ansi[idx - NamedColor::DimBlack as usize];
                        crate::canvas::mix(base, self.pal.bg, 0.33)
                    }
                    _ => self.pal.fg,
                }
            }
        }
    }

    fn draw(&mut self) {
        let pal = self.pal.clone();
        let (cw, ch, pad) = (self.fonts.cell_w, self.fonts.cell_h, self.pad());
        let (w, h) = (self.cv.w, self.cv.h);
        self.cv.reset_clip();
        self.cv.fill(Rect::new(0, 0, w, h), pal.bg);
        let term = self.session.term.clone();
        let t = term.lock();
        let content = t.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let sel = content.selection;
        let cursor = content.cursor;
        let mode = content.mode;
        // Search matches in view: 1 a match, 2 the current one.
        let (cols, lines) = (self.cols, self.lines);
        let mut found: Vec<u8> = Vec::new();
        if let Some(s) = self.search.as_ref().filter(|s| !s.query.is_empty()) {
            found = vec![0; cols * lines];
            let current = s.current.and_then(|i| s.matches.get(i)).map(|m| m.0);
            let first = -offset;
            let seen = crate::text::find(&t, &s.query, first, first + lines as i32 - 1, 10_000);
            for (a, b) in seen {
                let v = if Some(a) == current { 2 } else { 1 };
                for line in a.line.0..=b.line.0 {
                    let row = line + offset;
                    if row < 0 || row >= lines as i32 {
                        continue;
                    }
                    let c0 = if line == a.line.0 { a.column.0 } else { 0 };
                    let c1 = if line == b.line.0 {
                        b.column.0
                    } else {
                        cols - 1
                    };
                    for c in c0..=c1.min(cols - 1) {
                        found[row as usize * cols + c] = v;
                    }
                }
            }
        }
        let mut cells: Vec<Cell> = Vec::with_capacity(self.cols * self.lines);
        for item in content.display_iter {
            let cell = item.cell;
            let row = item.point.line.0 + offset;
            let col = item.point.column.0 as i32;
            let flags = cell.flags;
            if flags.contains(Flags::WIDE_CHAR_SPACER)
                || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let mut fg = self.color(cell.fg, colors);
            let mut bg = self.color(cell.bg, colors);
            if flags.contains(Flags::DIM) {
                fg = crate::canvas::mix(fg, bg, 0.33);
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let selected = sel.as_ref().is_some_and(|s| s.contains(item.point));
            if selected {
                bg = pal.sel_bg;
                if let Some(f) = pal.sel_fg {
                    fg = f;
                }
            }
            if !found.is_empty() && row >= 0 && (row as usize) < lines && (col as usize) < cols {
                match found[row as usize * cols + col as usize] {
                    1 => (fg, bg) = (pal.bg, pal.ansi[3]),
                    2 => (fg, bg) = (pal.bg, pal.fg),
                    _ => {}
                }
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let style = u8::from(flags.contains(Flags::BOLD))
                | (u8::from(flags.contains(Flags::ITALIC)) << 1);
            cells.push(Cell {
                row,
                col,
                c: cell.c,
                fg,
                bg,
                style,
                wide: flags.contains(Flags::WIDE_CHAR),
                flags,
                extra: cell.zerowidth().map(|z| z.to_vec()),
                link: cell.hyperlink().is_some(),
            });
        }
        let history = t.grid().history_size() as i64;
        let cursor_style = t.cursor_style();
        drop(t);
        // Backgrounds first, then text, so wide glyphs are not cut.
        for c in &cells {
            if c.bg != pal.bg {
                let span = if c.wide { 2 } else { 1 };
                self.cv.fill(
                    Rect::new(pad + c.col * cw, pad + c.row * ch, cw * span, ch),
                    c.bg,
                );
            }
        }
        let ligatures = self.fonts.ligatures();
        // Cells up to this index were drawn as part of a joined run; runs
        // are looked at once, from their first cell.
        let mut joined_until = 0;
        let mut looked_until = 0;
        let mut run = String::new();
        for (i, c) in cells.iter().enumerate() {
            let (x, y) = (pad + c.col * cw, pad + c.row * ch);
            let span = if c.wide { 2 } else { 1 };
            if ligatures && i >= looked_until && crate::font::joins(c.c) && c.extra.is_none() {
                // The run of punctuation in the same colour and style.
                run.clear();
                run.push(c.c);
                let mut j = i + 1;
                while let Some(n) = cells.get(j) {
                    let prev = &cells[j - 1];
                    if n.row != c.row
                        || n.col != prev.col + 1
                        || n.fg != c.fg
                        || n.style != c.style
                        || n.extra.is_some()
                        || !crate::font::joins(n.c)
                    {
                        break;
                    }
                    run.push(n.c);
                    j += 1;
                }
                looked_until = j;
                if j - i >= 2 && self.fonts.draw_run(&mut self.cv, x, y, &run, c.style, c.fg) {
                    joined_until = j;
                }
            }
            if i >= joined_until {
                self.fonts
                    .draw(&mut self.cv, x, y, c.c, c.style, c.fg, span);
            }
            if let Some(extra) = &c.extra {
                for z in extra {
                    self.fonts.draw(&mut self.cv, x, y, *z, c.style, c.fg, span);
                }
            }
            let line = (ch / 14).max(1);
            if c.flags.intersects(Flags::ALL_UNDERLINES) || c.link {
                let uy = y + self.fonts.ascent + line.max(1);
                let col = if c.link && !c.flags.intersects(Flags::ALL_UNDERLINES) {
                    pal.url
                } else {
                    c.fg
                };
                if c.flags.contains(Flags::UNDERCURL) {
                    for k in 0..cw * span {
                        let dy = if (k / (cw / 4).max(1)) % 2 == 0 {
                            0
                        } else {
                            line
                        };
                        self.cv.fill(Rect::new(x + k, uy + dy, 1, line), col);
                    }
                } else {
                    self.cv
                        .fill(Rect::new(x, uy.min(y + ch - line), cw * span, line), col);
                    if c.flags.contains(Flags::DOUBLE_UNDERLINE) {
                        self.cv.fill(
                            Rect::new(x, (uy + 2 * line).min(y + ch - line), cw * span, line),
                            col,
                        );
                    }
                }
            }
            if c.flags.contains(Flags::STRIKEOUT) {
                self.cv
                    .fill(Rect::new(x, y + ch / 2, cw * span, (ch / 14).max(1)), c.fg);
            }
        }
        // Pictures, anchored to their grid lines.
        let placements = self
            .session
            .graphics
            .lock()
            .map(|g| (g.placements.clone(), g.images.clone()))
            .ok();
        if let Some((places, images)) = placements {
            self.cv.set_clip(Rect::new(
                pad,
                pad,
                self.cols as i32 * cw,
                self.lines as i32 * ch,
            ));
            for p in places {
                let row = p.abs_line - history + offset as i64;
                if row + p.rows as i64 <= 0 || row >= self.lines as i64 {
                    continue;
                }
                let Some(img) = images.get(&p.image) else {
                    continue;
                };
                let (bw, bh) = (p.cols as i32 * cw, p.rows as i32 * ch);
                let key = (p.image, bw, bh);
                let scaled = match self.scaled.get(&key) {
                    Some((source, scaled)) if Arc::ptr_eq(source, img) => scaled.clone(),
                    _ => {
                        let scaled = Arc::new(crate::image::scale_to_fit(img, bw, bh));
                        self.scaled.insert(key, (img.clone(), scaled.clone()));
                        scaled
                    }
                };
                self.cv
                    .image(pad + p.col as i32 * cw, pad + row as i32 * ch, &scaled, 255);
            }
            self.cv.reset_clip();
            if self.scaled.len() > 64 {
                self.scaled.clear();
            }
        }
        // The cursor, when it is on screen.
        let show = mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;
        let crow = cursor.point.line.0 + offset;
        if show && crow >= 0 && crow < self.lines as i32 && (self.blink_on || !self.focused) {
            let (x, y) = (pad + cursor.point.column.0 as i32 * cw, pad + crow * ch);
            let shape = match (cursor_style.shape, self.settings.cursor) {
                (CursorShape::Block, Cursor::Beam) if !mode.contains(TermMode::ALT_SCREEN) => {
                    CursorShape::Beam
                }
                (s, _) => s,
            };
            let thick = (cw / 8).max(2);
            if !self.focused {
                self.cv.frame(Rect::new(x, y, cw, ch), pal.cursor);
            } else {
                match shape {
                    CursorShape::Beam => self.cv.fill(Rect::new(x, y, thick, ch), pal.cursor),
                    CursorShape::Underline => self
                        .cv
                        .fill(Rect::new(x, y + ch - thick, cw, thick), pal.cursor),
                    CursorShape::HollowBlock => self.cv.frame(Rect::new(x, y, cw, ch), pal.cursor),
                    _ => {
                        self.cv.fill(Rect::new(x, y, cw, ch), pal.cursor);
                        if let Some(c) = cells
                            .iter()
                            .find(|c| c.row == crow && c.col == cursor.point.column.0 as i32)
                        {
                            self.fonts
                                .draw(&mut self.cv, x, y, c.c, c.style, pal.cursor_text, 1);
                        }
                    }
                }
            }
        }
        // Keyboard hints: the link underlined, its label over its start.
        if let Some(h) = &self.hints {
            for (label, hint) in &h.list {
                if !label.starts_with(&h.typed) {
                    continue;
                }
                let (a, b) = (hint.start, hint.end);
                let thick = (ch / 14).max(2);
                for line in a.line.0..=b.line.0 {
                    let row = line + offset;
                    if row < 0 || row >= lines as i32 {
                        continue;
                    }
                    let c0 = if line == a.line.0 { a.column.0 } else { 0 };
                    let c1 = if line == b.line.0 {
                        b.column.0
                    } else {
                        cols - 1
                    };
                    let r = Rect::new(
                        pad + c0 as i32 * cw,
                        pad + row * ch + ch - thick,
                        (c1 + 1 - c0) as i32 * cw,
                        thick,
                    );
                    self.cv.fill(r, pal.ansi[3]);
                }
                let row = a.line.0 + offset;
                if (0..lines as i32).contains(&row) {
                    let (x, y) = (pad + a.column.0 as i32 * cw, pad + row * ch);
                    let n = label.chars().count() as i32;
                    self.cv.fill(Rect::new(x, y, n * cw, ch), pal.ansi[3]);
                    for (k, c) in label.chars().enumerate() {
                        self.fonts
                            .draw(&mut self.cv, x + k as i32 * cw, y, c, 1, pal.bg, 1);
                    }
                }
            }
        }
        // The search bar over the last line.
        if let Some(s) = &self.search {
            let y = pad + (lines as i32 - 1) * ch;
            self.cv.fill(Rect::new(0, y, w, ch + pad), pal.sel_bg);
            let count = match (s.current, s.matches.len()) {
                _ if s.query.is_empty() => String::new(),
                (_, 0) => "no matches".to_string(),
                (Some(i), n) => format!("{}/{n}", i + 1),
                (None, n) => format!("{n}"),
            };
            let text = format!("Find: {}", s.query);
            let n = text.chars().count() as i32;
            for (k, c) in text.chars().enumerate() {
                self.fonts
                    .draw(&mut self.cv, pad + k as i32 * cw, y, c, 0, pal.fg, 1);
            }
            self.cv
                .fill(Rect::new(pad + n * cw, y, (cw / 8).max(2), ch), pal.cursor);
            let cn = count.chars().count() as i32;
            for (k, c) in count.chars().enumerate() {
                let x = w - pad - (cn - k as i32) * cw;
                self.fonts.draw(&mut self.cv, x, y, c, 0, pal.fg, 1);
            }
        }
        if self.confirm.is_some() {
            self.draw_confirm();
        }
        // A message, or the history position while scrolled back, top right.
        let label = match &self.toast {
            Some((t, _)) => Some(t.clone()),
            None if offset > 0 => Some(format!("{offset}/{history}")),
            None => None,
        };
        if let Some(label) = label {
            let lw = label.chars().count() as i32 * cw;
            let r = Rect::new(w - pad - lw - cw, pad / 2, lw + cw, ch);
            self.cv.fill(r, pal.sel_bg);
            for (k, c) in label.chars().enumerate() {
                self.fonts.draw(
                    &mut self.cv,
                    r.x + cw / 2 + k as i32 * cw,
                    r.y,
                    c,
                    0,
                    pal.fg,
                    1,
                );
            }
        }
        if self.bell_until.is_some() {
            self.cv.tint(Rect::new(0, 0, w, h), pal.fg, 28);
        }
    }
}

impl App {
    /// Text in cells from (x, y), cut with an ellipsis after `max` cells;
    /// returns the cells used.
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        style: u8,
        color: u32,
        max: usize,
    ) -> usize {
        let cw = self.fonts.cell_w;
        let total: usize = text.chars().map(cell_width).sum();
        let room = if total > max {
            max.saturating_sub(1)
        } else {
            max
        };
        let mut used = 0;
        let mut last = x;
        for c in text.chars() {
            let n = cell_width(c);
            if n == 0 {
                self.fonts.draw(&mut self.cv, last, y, c, style, color, 1);
                continue;
            }
            if used + n > room {
                break;
            }
            last = x + used as i32 * cw;
            self.fonts
                .draw(&mut self.cv, last, y, c, style, color, n as i32);
            used += n;
        }
        if total > max && max > 0 {
            self.fonts
                .draw(&mut self.cv, x + used as i32 * cw, y, '…', style, color, 1);
            used += 1;
        }
        used
    }

    /// The paste confirmation: a box in the middle of the window with the
    /// reason, the first lines of the paste and the keys.
    fn draw_confirm(&mut self) {
        let Some(c) = &self.confirm else { return };
        let pal = self.pal.clone();
        let (cw, ch) = (self.fonts.cell_w, self.fonts.cell_h);
        let (w, h) = (self.cv.w, self.cv.h);
        const SHOWN: usize = 6;
        let (lines, total) = crate::paste::preview(&c.text, SHOWN);
        let danger = c.reason.danger.is_some();
        let sentence = c.reason.sentence();
        let title = match total {
            1 => "Paste this line?".to_string(),
            n => format!("Paste {n} lines?"),
        };
        let more = total.saturating_sub(lines.len());
        // The box: as wide as the longest line, within the window.
        let fit = ((w / cw) as usize).saturating_sub(4).max(10);
        let longest = lines
            .iter()
            .map(|l| l.chars().map(cell_width).sum::<usize>() + 4)
            .chain([title.chars().count() + 4, 50])
            .max()
            .unwrap_or(50);
        let inner = longest.clamp(50, 80).min(fit);
        let words = wrap(&sentence, inner);
        let accent = if danger { pal.ansi[1] } else { pal.ansi[3] };
        let surface = crate::canvas::mix(pal.bg, pal.fg, 0.07);
        let dim = crate::canvas::mix(pal.fg, surface, 0.45);
        let gap = ch / 2;
        let rows = 1 + words.len() + lines.len() + usize::from(more > 0) + 1;
        let bw = (inner as i32 + 2) * cw;
        let bh = rows as i32 * ch + 5 * gap;
        let x0 = ((w - bw) / 2).max(0);
        let y0 = ((h - bh) / 2).max(0);
        // The terminal behind is dimmed, so the box stands out.
        self.cv.tint(Rect::new(0, 0, w, h), pal.bg, 150);
        let border = (ch / 12).max(2);
        self.cv.fill(
            Rect::new(x0 - border, y0 - border, bw + 2 * border, bh + 2 * border),
            accent,
        );
        self.cv.fill(Rect::new(x0, y0, bw, bh), surface);
        // Title bar.
        self.cv.fill(Rect::new(x0, y0, bw, ch + gap), accent);
        let tx = x0 + cw;
        let mut y = y0 + gap / 2;
        self.draw_text(tx, y, &title, 1, pal.bg, inner);
        y += ch + gap + gap / 2;
        for l in &words {
            self.draw_text(tx, y, l, 0, pal.fg, inner);
            y += ch;
        }
        y += gap;
        // The paste itself on the terminal background, with a bar in the
        // accent colour at its left.
        let ph = (lines.len() as i32 + i32::from(more > 0)) * ch;
        self.cv
            .fill(Rect::new(tx, y, inner as i32 * cw, ph), pal.bg);
        self.cv.fill(Rect::new(tx, y, (cw / 4).max(2), ph), accent);
        for l in &lines {
            self.draw_text(tx + cw, y, l, 0, pal.fg, inner - 2);
            y += ch;
        }
        if more > 0 {
            let s = format!("and {more} more line{}", if more == 1 { "" } else { "s" });
            self.draw_text(tx + cw, y, &s, 2, dim, inner - 2);
            y += ch;
        }
        y += gap;
        let mut x = tx;
        for (k, (key, what)) in [("Enter", " paste"), ("Esc", " cancel")].iter().enumerate() {
            if k > 0 {
                x += 3 * cw;
            }
            let n = self.draw_text(x, y, key, 1, accent, inner);
            x += n as i32 * cw;
            let n = self.draw_text(x, y, what, 0, dim, inner);
            x += n as i32 * cw;
        }
    }
}

/// Cells a character takes in the confirmation box: 2 for East Asian wide
/// characters and emoji, 0 for combining marks.
fn cell_width(c: char) -> usize {
    match c as u32 {
        0x0300..=0x036f | 0x200b..=0x200f | 0xfe00..=0xfe0f => 0,
        0x1100..=0x115f
        | 0x2e80..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// Words in lines of at most `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// ARGB window icons: a pixel ">_" prompt on the theme background.
fn window_icons(p: &Palette) -> Vec<(u32, u32, Vec<u32>)> {
    // 8x8 pattern, scaled by the icon size.
    const ART: [u8; 8] = [
        0b0000_0000,
        0b0100_0000,
        0b0010_0000,
        0b0001_0000,
        0b0010_0000,
        0b0100_0111,
        0b0000_0000,
        0b0000_0000,
    ];
    [16u32, 32, 48]
        .iter()
        .map(|&s| {
            let u = s / 8;
            let mut px = vec![0xff00_0000 | p.bg; (s * s) as usize];
            for (row, bits) in ART.iter().enumerate() {
                for col in 0..8 {
                    if bits & (0x80 >> col) != 0 {
                        for dy in 0..u {
                            for dx in 0..u {
                                let i = ((row as u32 * u + dy) * s + col as u32 * u + dx) as usize;
                                px[i] = 0xff00_0000 | p.ansi[2];
                            }
                        }
                    }
                }
            }
            // A one-pixel frame in the accent colour.
            for k in 0..s {
                for i in [k, (s - 1) * s + k, k * s, k * s + s - 1] {
                    px[i as usize] = 0xff00_0000 | p.ansi[4];
                }
            }
            (s, s, px)
        })
        .collect()
}

// ------------------------------------------------------------------- input

/// The bytes for a special key (xterm encoding), or None for text keys.
pub fn key_bytes(sym: u32, ctrl: bool, shift: bool, alt: bool, mode: TermMode) -> Option<Vec<u8>> {
    let m = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let app = mode.contains(TermMode::APP_CURSOR);
    let csi = |s: &str| -> Vec<u8> { format!("\x1b[{s}").into_bytes() };
    let letter = |l: char| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{l}").into_bytes()
        } else if app {
            format!("\x1bO{l}").into_bytes()
        } else {
            format!("\x1b[{l}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let func = |l: char| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{l}").into_bytes()
        } else {
            format!("\x1bO{l}").into_bytes()
        }
    };
    let esc = |mut v: Vec<u8>| -> Vec<u8> {
        if alt {
            v.insert(0, 0x1b);
        }
        v
    };
    Some(match sym {
        0xff52 => letter('A'),
        0xff54 => letter('B'),
        0xff53 => letter('C'),
        0xff51 => letter('D'),
        0xff50 | 0xff95 => letter('H'),
        0xff57 | 0xff9c => letter('F'),
        0xff63 | 0xff9e => tilde(2),
        0xffff | 0xff9f => tilde(3),
        0xff55 | 0xff9a => tilde(5),
        0xff56 | 0xff9b => tilde(6),
        0xffbe => func('P'),
        0xffbf => func('Q'),
        0xffc0 => func('R'),
        0xffc1 => func('S'),
        0xffc2 => tilde(15),
        0xffc3 => tilde(17),
        0xffc4 => tilde(18),
        0xffc5 => tilde(19),
        0xffc6 => tilde(20),
        0xffc7 => tilde(21),
        0xffc8 => tilde(23),
        0xffc9 => tilde(24),
        0xff0d | 0xff8d => esc(vec![b'\r']),
        0xff08 => {
            if ctrl {
                esc(vec![0x08])
            } else {
                esc(vec![0x7f])
            }
        }
        0xff09 => esc(vec![b'\t']),
        0xfe20 => csi("Z"),
        0xff1b => esc(vec![0x1b]),
        _ => return None,
    })
}

/// Control characters for Ctrl+key.
pub fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '/' | '7' | '-' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

pub fn keysym_char(sym: u32) -> Option<char> {
    match sym {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym),
        0xffb0..=0xffb9 => char::from_u32(sym - 0xffb0 + b'0' as u32),
        0xffaa => Some('*'),
        0xffab => Some('+'),
        0xffad => Some('-'),
        0xffae => Some('.'),
        0xffaf => Some('/'),
        0xffac => Some(','),
        0x20ac => Some('€'),
        0x0100_0000..=0x0110_ffff => char::from_u32(sym - 0x0100_0000),
        _ => None,
    }
}

/// The character a dead key gives on its own (dead key + space).
fn dead_alone(sym: u32) -> Option<char> {
    Some(match sym {
        0xfe50 => '`',
        0xfe51 => '´',
        0xfe52 => '^',
        0xfe53 => '~',
        0xfe57 => '¨',
        _ => return None,
    })
}

/// A dead key followed by a letter.
pub fn compose(dead: u32, c: char) -> Option<char> {
    if c == ' ' {
        return dead_alone(dead);
    }
    let table: &[(char, char)] = match dead {
        0xfe50 => &[
            ('a', 'à'),
            ('e', 'è'),
            ('i', 'ì'),
            ('o', 'ò'),
            ('u', 'ù'),
            ('A', 'À'),
            ('E', 'È'),
            ('O', 'Ò'),
            ('U', 'Ù'),
        ],
        0xfe51 => &[
            ('a', 'á'),
            ('e', 'é'),
            ('i', 'í'),
            ('o', 'ó'),
            ('u', 'ú'),
            ('y', 'ý'),
            ('A', 'Á'),
            ('E', 'É'),
            ('I', 'Í'),
            ('O', 'Ó'),
            ('U', 'Ú'),
        ],
        0xfe52 => &[
            ('a', 'â'),
            ('e', 'ê'),
            ('i', 'î'),
            ('o', 'ô'),
            ('u', 'û'),
            ('A', 'Â'),
            ('E', 'Ê'),
            ('O', 'Ô'),
        ],
        0xfe53 => &[
            ('a', 'ã'),
            ('n', 'ñ'),
            ('o', 'õ'),
            ('A', 'Ã'),
            ('N', 'Ñ'),
            ('O', 'Õ'),
        ],
        0xfe57 => &[
            ('a', 'ä'),
            ('e', 'ë'),
            ('i', 'ï'),
            ('o', 'ö'),
            ('u', 'ü'),
            ('y', 'ÿ'),
            ('A', 'Ä'),
            ('E', 'Ë'),
            ('O', 'Ö'),
            ('U', 'Ü'),
        ],
        _ => &[],
    };
    table.iter().find(|(k, _)| *k == c).map(|(_, v)| *v)
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_follow_application_cursor_mode_and_modifiers() {
        assert_eq!(
            key_bytes(0xff52, false, false, false, TermMode::empty()).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            key_bytes(0xff52, false, false, false, TermMode::APP_CURSOR).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            key_bytes(0xff53, true, false, false, TermMode::empty()).unwrap(),
            b"\x1b[1;5C"
        );
        assert_eq!(
            key_bytes(0xff56, false, true, false, TermMode::empty()).unwrap(),
            b"\x1b[6;2~"
        );
        assert_eq!(
            key_bytes(0xffbe, false, false, false, TermMode::empty()).unwrap(),
            b"\x1bOP"
        );
        assert_eq!(
            key_bytes(0xffc2, false, false, false, TermMode::empty()).unwrap(),
            b"\x1b[15~"
        );
        assert_eq!(
            key_bytes(0xff08, false, false, true, TermMode::empty()).unwrap(),
            b"\x1b\x7f"
        );
        assert_eq!(
            key_bytes(0xfe20, false, true, false, TermMode::empty()).unwrap(),
            b"\x1b[Z"
        );
        assert!(key_bytes(0x61, false, false, false, TermMode::empty()).is_none());
    }

    #[test]
    fn control_letters_and_swedish_dead_keys() {
        assert_eq!(ctrl_byte('c'), Some(3));
        assert_eq!(ctrl_byte('C'), Some(3));
        assert_eq!(ctrl_byte('['), Some(0x1b));
        assert_eq!(compose(0xfe53, ' '), Some('~'));
        assert_eq!(compose(0xfe53, 'n'), Some('ñ'));
        assert_eq!(compose(0xfe51, 'e'), Some('é'));
        assert_eq!(compose(0xfe57, 'u'), Some('ü'));
        assert_eq!(compose(0xfe52, 'x'), None);
        assert_eq!(keysym_char(0xe5), Some('å'));
        assert_eq!(keysym_char(0xffb3), Some('3'));
    }

    #[test]
    fn dropped_paths_are_quoted_for_the_shell() {
        assert_eq!(shell_quote("/tmp/a.txt"), "/tmp/a.txt");
        assert_eq!(
            shell_quote("/tmp/my file's.txt"),
            "'/tmp/my file'\\''s.txt'"
        );
    }
}

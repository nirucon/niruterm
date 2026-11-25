// Shared with NIRUFM; some helpers are unused here.
#![allow(dead_code)]
//! The X11 side: one window, a keyboard map, and presenting the canvas with
//! MIT-SHM (one shared-memory copy per frame) or, without it, `PutImage` in
//! slices that fit the request size.
use crate::canvas::Canvas;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub struct Atoms {
    pub wm_protocols: Atom,
    pub wm_delete: Atom,
    pub net_wm_name: Atom,
    pub net_wm_icon: Atom,
    pub net_wm_pid: Atom,
    pub utf8: Atom,
    pub niruwm_theme: Atom,
    pub clipboard: Atom,
    pub targets: Atom,
    pub uri_list: Atom,
    pub gnome_copied: Atom,
    pub sel_prop: Atom,
    pub xdnd_aware: Atom,
    pub xdnd_enter: Atom,
    pub xdnd_position: Atom,
    pub xdnd_status: Atom,
    pub xdnd_leave: Atom,
    pub xdnd_drop: Atom,
    pub xdnd_finished: Atom,
    pub xdnd_selection: Atom,
    pub xdnd_type_list: Atom,
    pub xdnd_copy: Atom,
    pub xdnd_move: Atom,
    pub dnd_prop: Atom,
    pub net_wm_state: Atom,
    pub fullscreen: Atom,
    pub primary: Atom,
}

struct Shm {
    seg: u32,
    addr: *mut u8,
    size: usize,
}

pub struct X {
    pub c: RustConnection,
    pub root: Window,
    pub win: Window,
    pub depth: u8,
    gc: Gcontext,
    pub atoms: Atoms,
    shm: Option<Shm>,
    shm_ok: bool,
    min_keycode: u8,
    per: u8,
    pub keysyms: Vec<u32>,
}

fn atom(c: &RustConnection, name: &str) -> Res<Atom> {
    Ok(c.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

impl X {
    pub fn open(w: u16, h: u16, title: &str, class: &str) -> Res<Self> {
        let (c, screen) = x11rb::connect(None)?;
        let s = &c.setup().roots[screen];
        let root = s.root;
        let depth = s.root_depth;
        let visual = s.root_visual;
        let atoms = Atoms {
            wm_protocols: atom(&c, "WM_PROTOCOLS")?,
            wm_delete: atom(&c, "WM_DELETE_WINDOW")?,
            net_wm_name: atom(&c, "_NET_WM_NAME")?,
            net_wm_icon: atom(&c, "_NET_WM_ICON")?,
            net_wm_pid: atom(&c, "_NET_WM_PID")?,
            utf8: atom(&c, "UTF8_STRING")?,
            niruwm_theme: atom(&c, "_NIRUWM_THEME")?,
            clipboard: atom(&c, "CLIPBOARD")?,
            targets: atom(&c, "TARGETS")?,
            uri_list: atom(&c, "text/uri-list")?,
            gnome_copied: atom(&c, "x-special/gnome-copied-files")?,
            sel_prop: atom(&c, "_NIRUFM_SELECTION")?,
            xdnd_aware: atom(&c, "XdndAware")?,
            xdnd_enter: atom(&c, "XdndEnter")?,
            xdnd_position: atom(&c, "XdndPosition")?,
            xdnd_status: atom(&c, "XdndStatus")?,
            xdnd_leave: atom(&c, "XdndLeave")?,
            xdnd_drop: atom(&c, "XdndDrop")?,
            xdnd_finished: atom(&c, "XdndFinished")?,
            xdnd_selection: atom(&c, "XdndSelection")?,
            xdnd_type_list: atom(&c, "XdndTypeList")?,
            xdnd_copy: atom(&c, "XdndActionCopy")?,
            xdnd_move: atom(&c, "XdndActionMove")?,
            dnd_prop: atom(&c, "_NIRUFM_DND")?,
            net_wm_state: atom(&c, "_NET_WM_STATE")?,
            fullscreen: atom(&c, "_NET_WM_STATE_FULLSCREEN")?,
            primary: u32::from(AtomEnum::PRIMARY),
        };
        let win = c.generate_id()?;
        c.create_window(
            depth,
            win,
            root,
            0,
            0,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(s.black_pixel)
                .bit_gravity(Gravity::NORTH_WEST)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::KEY_PRESS
                        | EventMask::KEY_RELEASE
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::POINTER_MOTION
                        | EventMask::LEAVE_WINDOW
                        | EventMask::STRUCTURE_NOTIFY
                        | EventMask::FOCUS_CHANGE,
                ),
        )?;
        // The theme property on the root window changes when the user picks
        // another NIRU theme.
        c.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        c.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            &wm_class(class),
        )?;
        // Files can be dropped on the window (XDND version 5).
        c.change_property32(
            PropMode::REPLACE,
            win,
            atoms.xdnd_aware,
            AtomEnum::ATOM,
            &[5],
        )?;
        c.change_property32(
            PropMode::REPLACE,
            win,
            atoms.wm_protocols,
            AtomEnum::ATOM,
            &[atoms.wm_delete],
        )?;
        c.change_property32(
            PropMode::REPLACE,
            win,
            atoms.net_wm_pid,
            AtomEnum::CARDINAL,
            &[std::process::id()],
        )?;
        let gc = c.generate_id()?;
        c.create_gc(gc, win, &CreateGCAux::new().graphics_exposures(0))?;
        let shm_ok = c
            .extension_information(x11rb::protocol::shm::X11_EXTENSION_NAME)?
            .is_some()
            && c.shm_query_version()
                .ok()
                .and_then(|r| r.reply().ok())
                .is_some()
            && std::env::var_os("NIRUFM_NO_SHM").is_none();
        let setup = c.setup();
        let (min_keycode, max_keycode) = (setup.min_keycode, setup.max_keycode);
        let map = c
            .get_keyboard_mapping(min_keycode, max_keycode - min_keycode + 1)?
            .reply()?;
        let x = X {
            c,
            root,
            win,
            depth,
            gc,
            atoms,
            shm: None,
            shm_ok,
            min_keycode,
            per: map.keysyms_per_keycode,
            keysyms: map.keysyms,
        };
        x.set_title(title)?;
        Ok(x)
    }

    pub fn set_title(&self, title: &str) -> Res<()> {
        self.c.change_property8(
            PropMode::REPLACE,
            self.win,
            self.atoms.net_wm_name,
            self.atoms.utf8,
            title.as_bytes(),
        )?;
        self.c.change_property8(
            PropMode::REPLACE,
            self.win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        )?;
        Ok(())
    }

    /// The ICCCM urgency flag in WM_HINTS (NIRUWM and NIRUBAR show the
    /// workspace as urgent), with the input hint every terminal sets.
    pub fn set_urgent(&self, on: bool) -> Res<()> {
        self.c.change_property32(
            PropMode::REPLACE,
            self.win,
            AtomEnum::WM_HINTS,
            AtomEnum::WM_HINTS,
            &wm_hints(on),
        )?;
        self.c.flush()?;
        Ok(())
    }

    /// `_NET_WM_ICON`: width, height, then ARGB pixels, for every size.
    pub fn set_icon(&self, sizes: &[(u32, u32, Vec<u32>)]) -> Res<()> {
        let mut data = Vec::new();
        for (w, h, px) in sizes {
            data.push(*w);
            data.push(*h);
            data.extend_from_slice(px);
        }
        self.c.change_property32(
            PropMode::REPLACE,
            self.win,
            self.atoms.net_wm_icon,
            AtomEnum::CARDINAL,
            &data,
        )?;
        Ok(())
    }

    pub fn map(&self) -> Res<()> {
        self.c.map_window(self.win)?;
        self.c.flush()?;
        Ok(())
    }

    pub fn root_theme(&self) -> Option<String> {
        let r = self
            .c
            .get_property(
                false,
                self.root,
                self.atoms.niruwm_theme,
                AtomEnum::ANY,
                0,
                64,
            )
            .ok()?
            .reply()
            .ok()?;
        let s = String::from_utf8(r.value).ok()?;
        (!s.is_empty()).then_some(s)
    }

    /// Read the keyboard map again (a layout change, MappingNotify).
    pub fn reload_keymap(&mut self) {
        let setup = self.c.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        if let Ok(Ok(map)) = self
            .c
            .get_keyboard_mapping(min, max - min + 1)
            .map(|c| c.reply())
        {
            self.min_keycode = min;
            self.per = map.keysyms_per_keycode;
            self.keysyms = map.keysyms;
        }
    }

    /// The keysym of a key, with Shift (column 1) or AltGr (columns 4/5).
    pub fn keysym(&self, keycode: u8, state: KeyButMask) -> u32 {
        let idx = (keycode.saturating_sub(self.min_keycode)) as usize * self.per as usize;
        let shift = state.contains(KeyButMask::SHIFT) ^ state.contains(KeyButMask::LOCK);
        let altgr = state.contains(KeyButMask::MOD5);
        let col = match (altgr, shift) {
            (true, false) => 4,
            (true, true) => 5,
            (false, true) => 1,
            (false, false) => 0,
        };
        let get = |c: usize| self.keysyms.get(idx + c).copied().unwrap_or(0);
        let k = get(col);
        if k != 0 {
            return k;
        }
        let base = get(if altgr { 4 } else { 0 });
        if base != 0 {
            // A letter without its own shifted symbol: upper case.
            if shift && (0x61..=0x7a).contains(&base) {
                return base - 0x20;
            }
            return base;
        }
        get(0)
    }

    fn ensure_shm(&mut self, bytes: usize) -> bool {
        if !self.shm_ok {
            return false;
        }
        if self.shm.as_ref().is_some_and(|s| s.size >= bytes) {
            return true;
        }
        self.drop_shm();
        let size = bytes.next_multiple_of(1 << 20);
        unsafe {
            let id = libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600);
            if id < 0 {
                self.shm_ok = false;
                return false;
            }
            let addr = libc::shmat(id, std::ptr::null(), 0);
            if addr as isize == -1 {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                self.shm_ok = false;
                return false;
            }
            let Ok(seg) = self.c.generate_id() else {
                return false;
            };
            let attached = self
                .c
                .shm_attach(seg, id as u32, false)
                .ok()
                .and_then(|ck| ck.check().ok())
                .is_some();
            // Removed now: the segment lives until both sides detach, so a
            // crash cannot leak it.
            libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            if !attached {
                libc::shmdt(addr);
                self.shm_ok = false;
                return false;
            }
            self.shm = Some(Shm {
                seg,
                addr: addr as *mut u8,
                size,
            });
        }
        true
    }

    fn drop_shm(&mut self) {
        if let Some(s) = self.shm.take() {
            let _ = self.c.shm_detach(s.seg);
            unsafe { libc::shmdt(s.addr as *const libc::c_void) };
        }
    }

    /// Copy the canvas to the window and wait until the server has read it.
    pub fn present(&mut self, cv: &Canvas) -> Res<()> {
        let bytes = cv.as_bytes();
        let (w, h) = (cv.w as u16, cv.h as u16);
        if self.ensure_shm(bytes.len()) {
            let s = self.shm.as_ref().unwrap();
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), s.addr, bytes.len()) };
            self.c.shm_put_image(
                self.win,
                self.gc,
                w,
                h,
                0,
                0,
                w,
                h,
                0,
                0,
                self.depth,
                ImageFormat::Z_PIXMAP.into(),
                false,
                s.seg,
                0,
            )?;
        } else {
            let max = self.c.maximum_request_bytes().saturating_sub(64).max(4096);
            let row_bytes = cv.w as usize * 4;
            let rows = (max / row_bytes).max(1);
            let mut y = 0usize;
            while y < cv.h as usize {
                let n = rows.min(cv.h as usize - y);
                self.c.put_image(
                    ImageFormat::Z_PIXMAP,
                    self.win,
                    self.gc,
                    w,
                    n as u16,
                    0,
                    y as i16,
                    0,
                    self.depth,
                    &bytes[y * row_bytes..(y + n) * row_bytes],
                )?;
                y += n;
            }
        }
        // A round trip: the next frame may overwrite the shared buffer.
        self.c.get_input_focus()?.reply()?;
        Ok(())
    }
}

impl X {
    /// Take the CLIPBOARD so other programs can paste our files.
    pub fn own_clipboard(&self) -> Res<()> {
        self.c
            .set_selection_owner(self.win, self.atoms.clipboard, x11rb::CURRENT_TIME)?;
        Ok(())
    }

    pub fn own_selection(&self, which: Atom) -> Res<()> {
        self.c
            .set_selection_owner(self.win, which, x11rb::CURRENT_TIME)?;
        self.c.flush()?;
        Ok(())
    }

    /// Ask for text from a selection (CLIPBOARD or PRIMARY).
    pub fn request_text(&self, which: Atom) -> Res<()> {
        self.c.convert_selection(
            self.win,
            which,
            self.atoms.utf8,
            self.atoms.sel_prop,
            x11rb::CURRENT_TIME,
        )?;
        self.c.flush()?;
        Ok(())
    }

    /// Answer a paste request with plain text.
    pub fn answer_text(&self, e: &SelectionRequestEvent, text: &str) -> Res<()> {
        let a = &self.atoms;
        let mut prop = if e.property == x11rb::NONE {
            e.target
        } else {
            e.property
        };
        if e.target == a.targets {
            let t = [a.targets, a.utf8, u32::from(AtomEnum::STRING)];
            self.c
                .change_property32(PropMode::REPLACE, e.requestor, prop, AtomEnum::ATOM, &t)?;
        } else if e.target == a.utf8 || e.target == u32::from(AtomEnum::STRING) {
            self.c.change_property8(
                PropMode::REPLACE,
                e.requestor,
                prop,
                e.target,
                text.as_bytes(),
            )?;
        } else {
            prop = x11rb::NONE;
        }
        let ev = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: e.time,
            requestor: e.requestor,
            selection: e.selection,
            target: e.target,
            property: prop,
        };
        self.c
            .send_event(false, e.requestor, EventMask::NO_EVENT, ev)?;
        self.c.flush()?;
        Ok(())
    }

    /// Xft.dpi from the X resources, when set.
    pub fn xft_dpi(&self) -> Option<f32> {
        let r = self
            .c
            .get_property(
                false,
                self.root,
                AtomEnum::RESOURCE_MANAGER,
                AtomEnum::STRING,
                0,
                1 << 16,
            )
            .ok()?
            .reply()
            .ok()?;
        let text = String::from_utf8_lossy(&r.value).to_string();
        text.lines()
            .find_map(|l| l.strip_prefix("Xft.dpi:"))
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| (48.0..=480.0).contains(v))
    }

    /// The text I-beam pointer over the window.
    pub fn text_cursor(&self) {
        let Ok(font) = self.c.generate_id() else {
            return;
        };
        if self.c.open_font(font, b"cursor").is_err() {
            return;
        }
        let Ok(cur) = self.c.generate_id() else {
            return;
        };
        // xterm glyph 152 of the cursor font.
        let _ = self
            .c
            .create_glyph_cursor(cur, font, font, 152, 153, 0, 0, 0, 0xffff, 0xffff, 0xffff);
        let _ = self
            .c
            .change_window_attributes(self.win, &ChangeWindowAttributesAux::new().cursor(cur));
        let _ = self.c.close_font(font);
    }

    pub fn owns_clipboard(&self) -> bool {
        self.c
            .get_selection_owner(self.atoms.clipboard)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.owner == self.win)
    }

    /// Ask the clipboard owner for files; the answer is a SelectionNotify.
    pub fn request_clipboard(&self, target: Atom) -> Res<()> {
        self.c.convert_selection(
            self.win,
            self.atoms.clipboard,
            target,
            self.atoms.sel_prop,
            x11rb::CURRENT_TIME,
        )?;
        self.c.flush()?;
        Ok(())
    }

    pub fn take_selection(&self) -> Option<Vec<u8>> {
        let r = self
            .c
            .get_property(
                true,
                self.win,
                self.atoms.sel_prop,
                AtomEnum::ANY,
                0,
                1 << 24,
            )
            .ok()?
            .reply()
            .ok()?;
        (!r.value.is_empty()).then_some(r.value)
    }

    /// Answer another program's paste request with `files` (cut or copy).
    pub fn answer(
        &self,
        e: &SelectionRequestEvent,
        files: &[std::path::PathBuf],
        cut: bool,
    ) -> Res<()> {
        let a = &self.atoms;
        let uris: Vec<String> = files.iter().map(|p| file_uri(p)).collect();
        let mut prop = if e.property == x11rb::NONE {
            e.target
        } else {
            e.property
        };
        if e.target == a.targets {
            let t = [
                a.targets,
                a.gnome_copied,
                a.uri_list,
                a.utf8,
                u32::from(AtomEnum::STRING),
            ];
            self.c
                .change_property32(PropMode::REPLACE, e.requestor, prop, AtomEnum::ATOM, &t)?;
        } else if e.target == a.gnome_copied {
            let body = format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"));
            self.c.change_property8(
                PropMode::REPLACE,
                e.requestor,
                prop,
                e.target,
                body.as_bytes(),
            )?;
        } else if e.target == a.uri_list {
            let body = uris.iter().map(|u| format!("{u}\r\n")).collect::<String>();
            self.c.change_property8(
                PropMode::REPLACE,
                e.requestor,
                prop,
                e.target,
                body.as_bytes(),
            )?;
        } else if e.target == a.utf8 || e.target == u32::from(AtomEnum::STRING) {
            let body = files
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            self.c.change_property8(
                PropMode::REPLACE,
                e.requestor,
                prop,
                e.target,
                body.as_bytes(),
            )?;
        } else {
            prop = x11rb::NONE;
        }
        let ev = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: e.time,
            requestor: e.requestor,
            selection: e.selection,
            target: e.target,
            property: prop,
        };
        self.c
            .send_event(false, e.requestor, EventMask::NO_EVENT, ev)?;
        self.c.flush()?;
        Ok(())
    }
}

impl X {
    pub fn send_client(&self, to: Window, kind: Atom, data: [u32; 5]) {
        let ev = ClientMessageEvent::new(32, to, kind, data);
        let _ = self.c.send_event(false, to, EventMask::NO_EVENT, ev);
        let _ = self.c.flush();
    }

    /// The XDND-aware window under a root position (and its version).
    pub fn xdnd_target(&self, x: i16, y: i16) -> Option<(Window, u32)> {
        let mut w = self.root;
        for _ in 0..16 {
            let r = self
                .c
                .translate_coordinates(self.root, w, x, y)
                .ok()?
                .reply()
                .ok()?;
            if r.child == x11rb::NONE {
                return None;
            }
            w = r.child;
            let aware = self
                .c
                .get_property(false, w, self.atoms.xdnd_aware, AtomEnum::ATOM, 0, 1)
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| r.value32().and_then(|mut v| v.next()));
            if let Some(v) = aware {
                return Some((w, v.min(5)));
            }
        }
        None
    }

    pub fn to_window(&self, x: i16, y: i16) -> (i32, i32) {
        self.c
            .translate_coordinates(self.root, self.win, x, y)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| (r.dst_x as i32, r.dst_y as i32))
            .unwrap_or((-1, -1))
    }

    pub fn grab(&self) {
        let mask = EventMask::POINTER_MOTION | EventMask::BUTTON_RELEASE | EventMask::BUTTON_PRESS;
        let _ = self.c.grab_pointer(
            false,
            self.win,
            mask,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        );
        let _ = self.c.flush();
    }

    /// Ask the window manager to toggle full screen (EWMH).
    pub fn toggle_fullscreen(&self, on: bool) {
        let ev = ClientMessageEvent::new(
            32,
            self.win,
            self.atoms.net_wm_state,
            [u32::from(on), self.atoms.fullscreen, 0, 1, 0],
        );
        let _ = self.c.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ev,
        );
        let _ = self.c.flush();
    }

    pub fn ungrab(&self) {
        let _ = self.c.ungrab_pointer(x11rb::CURRENT_TIME);
        let _ = self.c.flush();
    }
}

pub fn file_uri(p: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Paths from text/uri-list or gnome-copied-files ("copy" or "cut" first).
pub fn parse_uris(data: &[u8]) -> (Vec<std::path::PathBuf>, bool) {
    let text = String::from_utf8_lossy(data);
    let mut cut = false;
    let mut v = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if i == 0 && (line == "cut" || line == "copy") {
            cut = line == "cut";
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("file://") {
            // file://host/path: the host part is empty or localhost.
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            v.push(decode_path(rest));
        } else if line.starts_with('/') {
            v.push(std::path::PathBuf::from(line));
        }
    }
    (v, cut)
}

impl Drop for X {
    fn drop(&mut self) {
        self.drop_shm();
        let _ = self.c.flush();
    }
}

pub fn decode_path(s: &str) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    std::path::PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// WM_CLASS: the instance and the class. `--class NAME` sets both, as kitty
/// does, so window rules written for `kitty --class NAME` keep matching.
pub fn wm_class(class: &str) -> Vec<u8> {
    if class.is_empty() || class == "niruterm" {
        return b"niruterm\0NIRUTERM\0".to_vec();
    }
    let mut v = Vec::with_capacity(class.len() * 2 + 2);
    for _ in 0..2 {
        v.extend_from_slice(class.as_bytes());
        v.push(0);
    }
    v
}

/// WM_HINTS (ICCCM 4.1.2.4): flags, input, then fields NIRUTERM leaves
/// unset. InputHint is 1, UrgencyHint 256.
pub fn wm_hints(urgent: bool) -> [u32; 9] {
    let flags = 1 | if urgent { 256 } else { 0 };
    [flags, 1, 0, 0, 0, 0, 0, 0, 0]
}

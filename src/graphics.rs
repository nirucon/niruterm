//! Images through the kitty graphics protocol (`ESC _ G keys ; payload ESC \`),
//! the protocol `icat`, yazi, ranger, timg and chafa use. The PTY stream is
//! scanned for these sequences before the terminal parser sees it; images are
//! decoded on the reader thread and placed at the cursor, anchored to their
//! grid line so they scroll with the text.
use crate::canvas::Image;
use std::collections::HashMap;

/// Splits a byte stream into terminal text and graphics commands, keeping
/// state across reads (a sequence may arrive in several pieces).
#[derive(Default)]
pub struct Scanner {
    state: u8, // 0 text, 1 saw ESC, 2 in APC, 3 in APC saw ESC
    apc: Vec<u8>,
}

pub enum Piece<'a> {
    Text(&'a [u8]),
    Owned(Vec<u8>),
    Command(Vec<u8>),
}

impl Scanner {
    /// Feed bytes; text pieces go to `text`, complete `G` commands to `cmd`.
    pub fn feed(&mut self, bytes: &[u8], mut out: impl FnMut(Piece)) {
        let mut start = 0;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match self.state {
                0 => {
                    if b == 0x1b {
                        if i > start {
                            out(Piece::Text(&bytes[start..i]));
                        }
                        self.state = 1;
                        start = i + 1;
                    }
                }
                1 => {
                    if b == b'_' {
                        self.state = 2;
                        self.apc.clear();
                        start = i + 1;
                    } else {
                        // Not an APC: give the ESC back to the parser.
                        out(Piece::Owned(vec![0x1b]));
                        self.state = 0;
                        start = i;
                        continue;
                    }
                }
                2 => {
                    if b == 0x1b {
                        self.state = 3;
                    } else if b == 0x07 {
                        self.finish(&mut out);
                        start = i + 1;
                    } else if self.apc.len() < 64 << 20 {
                        self.apc.push(b);
                    }
                }
                _ => {
                    if b == b'\\' {
                        self.finish(&mut out);
                        start = i + 1;
                    } else {
                        self.apc.push(0x1b);
                        self.apc.push(b);
                        self.state = 2;
                    }
                }
            }
            i += 1;
        }
        if self.state == 0 && start < bytes.len() {
            out(Piece::Text(&bytes[start..]));
        }
    }

    fn finish(&mut self, out: &mut impl FnMut(Piece)) {
        self.state = 0;
        let apc = std::mem::take(&mut self.apc);
        if apc.first() == Some(&b'G') {
            out(Piece::Command(apc[1..].to_vec()));
        }
        // Other APC strings are ignored, as other terminals do.
    }
}

pub fn base64(data: &[u8]) -> Vec<u8> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut acc = 0u32;
    let mut n = 0;
    for &c in data {
        let Some(v) = val(c) else { continue };
        acc = (acc << 6) | v;
        n += 1;
        if n == 4 {
            out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
            acc = 0;
            n = 0;
        }
    }
    match n {
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
        _ => {}
    }
    out
}

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

/// One parsed command: its keys and payload.
#[derive(Debug, Default, Clone)]
pub struct Command {
    pub keys: HashMap<char, String>,
    pub payload: Vec<u8>,
}

impl Command {
    pub fn parse(raw: &[u8]) -> Command {
        let (head, payload) = match raw.iter().position(|b| *b == b';') {
            Some(i) => (&raw[..i], raw[i + 1..].to_vec()),
            None => (raw, Vec::new()),
        };
        let mut keys = HashMap::new();
        for kv in String::from_utf8_lossy(head).split(',') {
            if let Some((k, v)) = kv.split_once('=') {
                if let Some(c) = k.chars().next() {
                    keys.insert(c, v.to_string());
                }
            }
        }
        Command { keys, payload }
    }
    pub fn get(&self, k: char) -> Option<&str> {
        self.keys.get(&k).map(String::as_str)
    }
    pub fn num(&self, k: char) -> u32 {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(0)
    }
}

/// An image shown in the grid.
#[derive(Clone)]
pub struct Placement {
    pub image: u32,
    /// Grid line counted from the top of the whole history at placement.
    pub abs_line: i64,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Default)]
pub struct Store {
    pub images: HashMap<u32, std::sync::Arc<Image>>,
    pub placements: Vec<Placement>,
    /// Chunks of an image still arriving (m=1).
    pending: Option<(Command, Vec<u8>)>,
    next_id: u32,
}

/// What the reader thread must do after a command.
pub enum Action {
    None,
    /// Write this answer to the program (a query reply or an error).
    Reply(String),
    /// Show `image` at the cursor: `cols` x `rows` cells; `move_cursor` says
    /// whether the cursor moves past the picture afterwards.
    Place {
        image: u32,
        cols: usize,
        rows: usize,
        move_cursor: bool,
        reply: Option<String>,
    },
}

impl Store {
    /// Handle one command. `cell` is the cell size in pixels.
    pub fn command(&mut self, raw: &[u8], cell: (u32, u32)) -> Action {
        let mut cmd = Command::parse(raw);
        // Chunked transmission: collect until m=0.
        if let Some((first, mut data)) = self.pending.take() {
            data.extend_from_slice(&cmd.payload);
            if cmd.num('m') == 1 {
                self.pending = Some((first, data));
                return Action::None;
            }
            cmd = Command {
                keys: first.keys,
                payload: data,
            };
        } else if cmd.num('m') == 1 {
            let data = std::mem::take(&mut cmd.payload);
            self.pending = Some((cmd, data));
            return Action::None;
        }
        let action = cmd.get('a').unwrap_or("t").to_string();
        let id = match cmd.num('i') {
            0 => {
                self.next_id += 1;
                0x7000_0000 + self.next_id
            }
            n => n,
        };
        let quiet = cmd.num('q');
        let ok = |id: u32| {
            if quiet >= 1 || cmd.num('i') == 0 {
                None
            } else {
                Some(format!("\x1b_Gi={id};OK\x1b\\"))
            }
        };
        let err = |id: u32, msg: &str| {
            if quiet >= 2 || cmd.num('i') == 0 {
                Action::None
            } else {
                Action::Reply(format!("\x1b_Gi={id};{msg}\x1b\\"))
            }
        };
        match action.as_str() {
            "q" => {
                // A query: can this image be decoded? Answer without storing.
                return match decode(&cmd) {
                    Ok(_) => Action::Reply(format!("\x1b_Gi={id};OK\x1b\\")),
                    Err(e) => Action::Reply(format!("\x1b_Gi={id};{e}\x1b\\")),
                };
            }
            "d" => {
                match cmd.get('d').unwrap_or("a") {
                    "i" | "I" => {
                        let target = cmd.num('i');
                        self.placements.retain(|p| p.image != target);
                        if cmd.get('d') == Some("I") {
                            self.images.remove(&target);
                        }
                    }
                    "A" => {
                        self.placements.clear();
                        self.images.clear();
                    }
                    _ => self.placements.clear(),
                }
                return Action::None;
            }
            "p" => {
                let Some(img) = self.images.get(&id).cloned() else {
                    return err(id, "ENOENT:no such image");
                };
                let (cols, rows) = size_in_cells(&cmd, &img, cell);
                return Action::Place {
                    image: id,
                    cols,
                    rows,
                    move_cursor: cmd.num('C') == 0,
                    reply: ok(id),
                };
            }
            _ => {}
        }
        // t (transmit) or T (transmit and show).
        match decode(&cmd) {
            Ok(img) => {
                let img = std::sync::Arc::new(img);
                let (cols, rows) = size_in_cells(&cmd, &img, cell);
                self.images.insert(id, img);
                // Keep memory bounded: at most 64 images.
                if self.images.len() > 64 {
                    let keep: std::collections::HashSet<u32> =
                        self.placements.iter().map(|p| p.image).collect();
                    let drop: Vec<u32> = self
                        .images
                        .keys()
                        .copied()
                        .filter(|k| !keep.contains(k))
                        .take(self.images.len() - 64)
                        .collect();
                    for k in drop {
                        self.images.remove(&k);
                    }
                }
                if action == "T" {
                    Action::Place {
                        image: id,
                        cols,
                        rows,
                        move_cursor: cmd.num('C') == 0,
                        reply: ok(id),
                    }
                } else {
                    match ok(id) {
                        Some(r) => Action::Reply(r),
                        None => Action::None,
                    }
                }
            }
            Err(e) => err(id, &e),
        }
    }
}

fn size_in_cells(cmd: &Command, img: &Image, cell: (u32, u32)) -> (usize, usize) {
    let (cw, ch) = (cell.0.max(1), cell.1.max(1));
    let c = cmd.num('c') as usize;
    let r = cmd.num('r') as usize;
    match (c, r) {
        (0, 0) => (
            (img.w as u32).div_ceil(cw) as usize,
            (img.h as u32).div_ceil(ch) as usize,
        ),
        (c, 0) => {
            let rows =
                (c as f32 * cw as f32 * img.h as f32 / img.w.max(1) as f32 / ch as f32).ceil();
            (c, rows.max(1.0) as usize)
        }
        (0, r) => {
            let cols =
                (r as f32 * ch as f32 * img.w as f32 / img.h.max(1) as f32 / cw as f32).ceil();
            (cols.max(1.0) as usize, r)
        }
        (c, r) => (c, r),
    }
}

/// The picture data of a command: PNG (f=100) or raw RGB/RGBA (f=24/32),
/// sent directly (t=d) or as a file (t=f, t=t).
fn decode(cmd: &Command) -> Result<Image, String> {
    let medium = cmd.get('t').unwrap_or("d");
    let mut data = base64(&cmd.payload);
    if medium == "f" || medium == "t" {
        let path = String::from_utf8_lossy(&data).to_string();
        let p = std::path::Path::new(&path);
        if !p.is_absolute() {
            return Err("EINVAL:relative path".into());
        }
        data = std::fs::read(p).map_err(|e| format!("EBADF:{e}"))?;
        if medium == "t" && (path.starts_with("/tmp/") || path.contains("tty-graphics-protocol")) {
            let _ = std::fs::remove_file(p);
        }
    }
    if cmd.get('o') == Some("z") {
        return Err("EINVAL:compressed data is not supported".into());
    }
    match cmd.num('f') {
        100 => crate::image::decode_png(&data).ok_or_else(|| "EBADPNG:cannot decode".to_string()),
        24 | 32 | 0 => {
            let bpp = if cmd.num('f') == 24 { 3 } else { 4 };
            let (w, h) = (cmd.num('s') as usize, cmd.num('v') as usize);
            if w == 0 || h == 0 || data.len() < w * h * bpp {
                return Err("EINVAL:size does not match the data".into());
            }
            let mut out = Vec::with_capacity(w * h * 4);
            for px in data.chunks(bpp).take(w * h) {
                let a = if bpp == 4 { px[3] } else { 255 } as u32;
                let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                out.extend_from_slice(&[pm(px[0]), pm(px[1]), pm(px[2]), a as u8]);
            }
            Ok(Image {
                w: w as i32,
                h: h as i32,
                data: out,
            })
        }
        _ => Err("EINVAL:unknown format".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for s in [&b""[..], b"a", b"ab", b"abc", b"hello world!"] {
            assert_eq!(base64(base64_encode(s).as_bytes()), s);
        }
    }

    #[test]
    fn apc_split_over_reads_is_found_and_text_kept() {
        let mut sc = Scanner::default();
        let mut text = Vec::new();
        let mut cmds = Vec::new();
        for chunk in [
            &b"ab\x1b_Gf=32,s=1"[..],
            b",v=1;AAAA",
            b"AA==\x1b",
            b"\\cd\x1b[1m",
        ] {
            sc.feed(chunk, |p| match p {
                Piece::Text(t) => text.extend_from_slice(t),
                Piece::Owned(t) => text.extend_from_slice(&t),
                Piece::Command(c) => cmds.push(c),
            });
        }
        assert_eq!(text, b"abcd\x1b[1m");
        assert_eq!(cmds.len(), 1);
        let c = Command::parse(&cmds[0]);
        assert_eq!(c.num('s'), 1);
        assert_eq!(base64(&c.payload), vec![0, 0, 0, 0]);
    }

    #[test]
    fn a_raw_rgba_image_is_placed_with_its_cell_size() {
        let mut st = Store::default();
        let payload = base64_encode(&[255u8; 4 * 20 * 40]);
        let raw = format!("a=T,f=32,s=20,v=40,i=5;{payload}");
        match st.command(raw.as_bytes(), (10, 20)) {
            Action::Place {
                image,
                cols,
                rows,
                reply,
                ..
            } => {
                assert_eq!((image, cols, rows), (5, 2, 2));
                assert!(reply.unwrap().contains("i=5;OK"));
            }
            _ => panic!("not placed"),
        }
    }

    #[test]
    fn chunks_are_joined() {
        let mut st = Store::default();
        let data = base64_encode(&[9u8; 4 * 2 * 2]);
        let (a, b) = data.split_at(8);
        assert!(matches!(
            st.command(format!("a=t,f=32,s=2,v=2,i=3,m=1;{a}").as_bytes(), (8, 16)),
            Action::None
        ));
        assert!(matches!(
            st.command(format!("m=0;{b}").as_bytes(), (8, 16)),
            Action::Reply(_)
        ));
        assert!(st.images.contains_key(&3));
    }
}

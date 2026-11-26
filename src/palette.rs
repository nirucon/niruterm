//! Colours and settings. The palette is the one NIRUX writes for the
//! terminal (`~/.config/nirux/theme/kitty.conf`: background, foreground,
//! cursor, selection and color0 to color15), so NIRUTERM and kitty look the
//! same and follow theme changes together. `~/.config/niruterm/config.toml`
//! can override font, size, padding, shell and any colour.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub bg: u32,
    pub fg: u32,
    pub cursor: u32,
    pub cursor_text: u32,
    pub sel_bg: u32,
    pub sel_fg: Option<u32>,
    pub ansi: [u32; 16],
    pub url: u32,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            bg: 0x0f0f10,
            fg: 0xe5e5e5,
            cursor: 0xe5e5e5,
            cursor_text: 0x0f0f10,
            sel_bg: 0x3a3a3d,
            sel_fg: None,
            ansi: [
                0x1c1c1e, 0xb87878, 0x83a18b, 0xb5a47e, 0x819eb5, 0xa390b0, 0x83a9a7, 0xd0d0d0,
                0x5a5a60, 0xc98a8a, 0x95b39d, 0xc7b690, 0x93b0c7, 0xb5a2c2, 0x95bbb9, 0xf0f0f0,
            ],
            url: 0x819eb5,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub font: Option<String>,
    pub font_size: f32,
    pub padding: i32,
    pub line_height: f32,
    pub shell: Option<String>,
    pub scrollback: usize,
    pub copy_on_select: bool,
    pub cursor_blink: bool,
    pub cursor: Cursor,
    /// Seconds a command must run for a notification when it finishes
    /// while the window is not focused; 0 is never.
    pub notify_after: u64,
    /// The editor for `path:line` hints (else $VISUAL or $EDITOR).
    pub editor: Option<String>,
    /// Ask before a paste that would run at once or looks dangerous.
    pub paste_protection: bool,
    /// Programming ligatures (`->`, `!=`, `=>`) from the font.
    pub ligatures: bool,
    /// The scratchpad (class `scratchpad`) starts in its last folder.
    pub remember_folder: bool,
    pub overrides: Vec<(String, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cursor {
    Beam,
    Block,
    Underline,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_STATE_HOME`, else `~/.local/state`.
pub fn state_home() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/state"))
}

pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
}

fn hex(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() == 6 {
        u32::from_str_radix(s, 16).ok()
    } else {
        None
    }
}

/// kitty.conf-style `key value` lines.
fn apply_kitty(p: &mut Palette, text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(k), Some(v)) = (it.next(), it.next()) else {
            continue;
        };
        apply(p, k, v);
    }
}

fn apply(p: &mut Palette, k: &str, v: &str) {
    let Some(c) = hex(v) else { return };
    match k {
        "background" => p.bg = c,
        "foreground" => p.fg = c,
        "cursor" => p.cursor = c,
        "cursor_text_color" => p.cursor_text = c,
        "selection_background" => p.sel_bg = c,
        "selection_foreground" => p.sel_fg = Some(c),
        "url_color" => p.url = c,
        _ => {
            if let Some(n) = k
                .strip_prefix("color")
                .and_then(|n| n.parse::<usize>().ok())
            {
                if n < 16 {
                    p.ansi[n] = c;
                }
            }
        }
    }
}

pub fn theme_file() -> PathBuf {
    config_home().join("nirux/theme/kitty.conf")
}

pub fn load(settings: &Settings) -> Palette {
    let mut p = Palette::default();
    if let Ok(text) = std::fs::read_to_string(theme_file()) {
        apply_kitty(&mut p, &text);
    }
    for (k, v) in &settings.overrides {
        apply(&mut p, k, &format!("{v:06x}"));
    }
    p
}

fn read_toml(path: &Path) -> Option<toml::Value> {
    std::fs::read_to_string(path)
        .ok()?
        .parse::<toml::Value>()
        .ok()
}

/// The font size of kitty's configuration when NIRUTERM has none, so the
/// switch keeps the same look.
fn kitty_font_size() -> Option<f32> {
    let text = std::fs::read_to_string(config_home().join("kitty/kitty.conf")).ok()?;
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("font_size"))
        .filter_map(|v| v.trim().parse::<f32>().ok())
        .next_back()
}

/// kitty's `shell` when NIRUTERM has none: NIRUX starts fish in kitty while
/// the login shell is often bash, and the switch should keep the same shell.
/// `.` (kitty's "the login shell"), a command line with arguments, or a shell
/// that is not installed leaves the login shell.
fn kitty_shell() -> Option<String> {
    let text = std::fs::read_to_string(config_home().join("kitty/kitty.conf")).ok()?;
    let value = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("shell "))
        .map(str::trim)
        .next_back()?
        .to_string();
    kitty_shell_choice(&value, |name| {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
    })
}

fn kitty_shell_choice(value: &str, installed: impl Fn(&str) -> bool) -> Option<String> {
    if value.is_empty() || value == "." || value.contains(char::is_whitespace) {
        return None;
    }
    let present = if value.contains('/') {
        std::path::Path::new(value).is_file()
    } else {
        installed(value)
    };
    present.then(|| value.to_string())
}

pub fn settings() -> Settings {
    let mut s = base(kitty_font_size().unwrap_or(12.0), kitty_shell());
    if let Some(v) = read_toml(&config_home().join("niruterm/config.toml")) {
        apply_config(&mut s, &v);
    }
    s
}

/// The defaults, with kitty's font size and shell.
fn base(font_size: f32, shell: Option<String>) -> Settings {
    Settings {
        font: None,
        font_size,
        padding: 10,
        line_height: 1.0,
        shell,
        scrollback: 10_000,
        copy_on_select: true,
        cursor_blink: false,
        cursor: Cursor::Beam,
        notify_after: 10,
        editor: None,
        paste_protection: true,
        ligatures: false,
        remember_folder: true,
        overrides: Vec::new(),
    }
}

/// The keys of config.toml on top of the defaults.
fn apply_config(s: &mut Settings, v: &toml::Value) {
    if let Some(f) = v.get("font").and_then(|x| x.as_str()) {
        s.font = Some(f.to_string());
    }
    if let Some(f) = v
        .get("font_size")
        .and_then(|x| x.as_float().or_else(|| x.as_integer().map(|i| i as f64)))
    {
        s.font_size = (f as f32).clamp(6.0, 48.0);
    }
    if let Some(n) = v.get("padding").and_then(|x| x.as_integer()) {
        s.padding = n.clamp(0, 80) as i32;
    }
    if let Some(f) = v.get("line_height").and_then(|x| x.as_float()) {
        s.line_height = (f as f32).clamp(0.8, 2.0);
    }
    if let Some(sh) = v.get("shell").and_then(|x| x.as_str()) {
        s.shell = Some(sh.to_string());
    }
    if let Some(n) = v.get("scrollback").and_then(|x| x.as_integer()) {
        s.scrollback = n.clamp(0, 1_000_000) as usize;
    }
    if let Some(b) = v.get("copy_on_select").and_then(|x| x.as_bool()) {
        s.copy_on_select = b;
    }
    if let Some(b) = v.get("cursor_blink").and_then(|x| x.as_bool()) {
        s.cursor_blink = b;
    }
    if let Some(n) = v.get("notify_after").and_then(|x| x.as_integer()) {
        s.notify_after = n.max(0) as u64;
    }
    if let Some(e) = v.get("editor").and_then(|x| x.as_str()) {
        s.editor = Some(e.to_string()).filter(|e| !e.trim().is_empty());
    }
    for (key, value) in [
        ("paste_protection", &mut s.paste_protection),
        ("ligatures", &mut s.ligatures),
        ("remember_folder", &mut s.remember_folder),
    ] {
        if let Some(b) = v.get(key).and_then(|x| x.as_bool()) {
            *value = b;
        }
    }
    if let Some(c) = v.get("cursor").and_then(|x| x.as_str()) {
        s.cursor = match c {
            "block" => Cursor::Block,
            "underline" => Cursor::Underline,
            _ => Cursor::Beam,
        };
    }
    if let Some(colors) = v.get("colors").and_then(|x| x.as_table()) {
        for (k, val) in colors {
            if let Some(c) = val.as_str().and_then(hex) {
                s.overrides.push((k.clone(), c));
            }
        }
    }
}

/// The xterm 256-colour palette above the 16 theme colours.
pub fn indexed(p: &Palette, i: u8) -> u32 {
    match i {
        0..=15 => p.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v as u32 * 40 };
            (step(i / 36) << 16) | (step((i / 6) % 6) << 8) | step(i % 6)
        }
        _ => {
            let v = 8 + (i as u32 - 232) * 10;
            (v << 16) | (v << 8) | v
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_follows_kitty_when_it_is_installed() {
        let yes = |_: &str| true;
        let no = |_: &str| false;
        assert_eq!(kitty_shell_choice("fish", yes).as_deref(), Some("fish"));
        assert_eq!(kitty_shell_choice("fish", no), None);
        assert_eq!(kitty_shell_choice(".", yes), None);
        assert_eq!(kitty_shell_choice("fish --login", yes), None);
        assert_eq!(kitty_shell_choice("/nonexistent/fish", yes), None);
    }

    #[test]
    fn a_nirux_theme_file_sets_the_palette() {
        let mut p = Palette::default();
        apply_kitty(
            &mut p,
            "# theme\nbackground #101010\nforeground #eeeeee\ncolor1 #ff0000\ncolor15 #ffffff\nselection_background #333333\n",
        );
        assert_eq!(p.bg, 0x101010);
        assert_eq!(p.ansi[1], 0xff0000);
        assert_eq!(p.ansi[15], 0xffffff);
        assert_eq!(p.sel_bg, 0x333333);
    }

    #[test]
    fn notification_time_and_editor_from_the_config() {
        let mut s = base(12.0, None);
        assert_eq!(s.notify_after, 10);
        let v: toml::Value = "notify_after = 0\neditor = \"nvim\"\nscrollback = 500\n"
            .parse()
            .unwrap();
        apply_config(&mut s, &v);
        assert_eq!(s.notify_after, 0);
        assert_eq!(s.editor.as_deref(), Some("nvim"));
        assert_eq!(s.scrollback, 500);
    }

    #[test]
    fn paste_protection_ligatures_and_the_scratchpad_folder_from_the_config() {
        let mut s = base(12.0, None);
        assert!(s.paste_protection && !s.ligatures && s.remember_folder);
        let v: toml::Value =
            "paste_protection = false\nligatures = true\nremember_folder = false\n"
                .parse()
                .unwrap();
        apply_config(&mut s, &v);
        assert!(!s.paste_protection && s.ligatures && !s.remember_folder);
    }

    #[test]
    fn the_256_colour_cube_and_grey_ramp() {
        let p = Palette::default();
        assert_eq!(indexed(&p, 16), 0x000000);
        assert_eq!(indexed(&p, 196), 0xff0000);
        assert_eq!(indexed(&p, 231), 0xffffff);
        assert_eq!(indexed(&p, 232), 0x080808);
        assert_eq!(indexed(&p, 255), 0xeeeeee);
    }
}

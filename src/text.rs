//! The text of the grid as logical lines (rows joined where they wrapped),
//! for the history search and the keyboard hints. Read only when the user
//! asks: nothing here runs on the output path.
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use std::path::{Path, PathBuf};

/// One logical line: its characters, where each one is, and the OSC 8 link
/// of each when asked for.
#[derive(Default)]
pub struct LineText {
    pub chars: Vec<char>,
    pub points: Vec<Point>,
    pub links: Vec<Option<String>>,
}

impl LineText {
    fn clear(&mut self) {
        self.chars.clear();
        self.points.clear();
        self.links.clear();
    }
}

/// Calls `f` for every logical line that has a row between `first` and
/// `last` (grid lines, negative in the history).
pub fn for_each_line<T>(
    term: &Term<T>,
    first: i32,
    last: i32,
    links: bool,
    mut f: impl FnMut(&LineText),
) {
    let grid = term.grid();
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32 - 1;
    let wraps = |l: i32| {
        grid[Line(l)][Column(cols - 1)]
            .flags
            .contains(Flags::WRAPLINE)
    };
    let mut l = first.max(top);
    while l > top && wraps(l - 1) {
        l -= 1;
    }
    let mut lt = LineText::default();
    while l <= bottom {
        let row = &grid[Line(l)];
        for c in 0..cols {
            let cell = &row[Column(c)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            lt.chars.push(cell.c);
            lt.points.push(Point::new(Line(l), Column(c)));
            if links {
                lt.links.push(cell.hyperlink().map(|h| h.uri().to_string()));
            }
        }
        if !wraps(l) {
            f(&lt);
            lt.clear();
            if l >= last {
                return;
            }
        }
        l += 1;
    }
    if !lt.chars.is_empty() {
        f(&lt);
    }
}

/// The places `query` is found between the lines `first` and `last`, first
/// to last, at most `limit`. Lower case finds either case; a capital letter
/// makes the search exact (smart case).
pub fn find<T>(
    term: &Term<T>,
    query: &str,
    first: i32,
    last: i32,
    limit: usize,
) -> Vec<(Point, Point)> {
    let fold = !query.chars().any(char::is_uppercase);
    let low = |c: char| {
        if fold {
            c.to_lowercase().next().unwrap_or(c)
        } else {
            c
        }
    };
    let q: Vec<char> = query.chars().map(low).collect();
    let mut found = Vec::new();
    if q.is_empty() {
        return found;
    }
    let mut hay = Vec::new();
    for_each_line(term, first, last, false, |lt| {
        if found.len() >= limit || lt.chars.len() < q.len() {
            return;
        }
        hay.clear();
        hay.extend(lt.chars.iter().map(|&c| low(c)));
        let mut i = 0;
        while i + q.len() <= hay.len() {
            if hay[i] == q[0] && hay[i..i + q.len()] == q[..] {
                found.push((lt.points[i], lt.points[i + q.len() - 1]));
                if found.len() >= limit {
                    return;
                }
                i += q.len();
            } else {
                i += 1;
            }
        }
    });
    found
}

/// What a hint opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Url(String),
    /// A file or folder as written, and the line after `path:`.
    Path(String, Option<u32>),
}

/// A link or path on the screen.
#[derive(Clone, Debug)]
pub struct Hint {
    pub start: Point,
    pub end: Point,
    pub text: String,
    pub target: Target,
}

const SCHEMES: [&str; 7] = [
    "https://", "http://", "file://", "ftp://", "ssh://", "git://", "mailto:",
];

fn url_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !"<>\"'`|{}^\\".contains(c)
}

/// Drops what follows a link in prose: a full stop, a comma, a closing
/// bracket that was not opened in it.
fn trim_url(s: &[char]) -> usize {
    let mut end = s.len();
    while let Some(&c) = s[..end].last() {
        let unbalanced = |open: char, close: char| {
            let o = s[..end].iter().filter(|&&x| x == open).count();
            let c = s[..end].iter().filter(|&&x| x == close).count();
            c > o
        };
        if ".,;:!?*".contains(c)
            || (c == ')' && unbalanced('(', ')'))
            || (c == ']' && unbalanced('[', ']'))
        {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

fn starts_with(chars: &[char], at: usize, s: &str) -> bool {
    (at..)
        .zip(s.chars())
        .all(|(k, c)| chars.get(k).map(|x| x.to_ascii_lowercase()) == Some(c))
}

/// The links and paths in one logical line, as character ranges `[s, e)`.
/// Paths are only candidates here: the caller keeps those that exist.
pub fn candidates(chars: &[char]) -> Vec<(usize, usize, Target)> {
    let mut out = Vec::new();
    let mut i = 0;
    // Links first; what they cover is not looked at again.
    let mut taken = vec![false; chars.len()];
    while i < chars.len() {
        let boundary = i == 0 || !chars[i - 1].is_alphanumeric();
        let scheme = if boundary {
            SCHEMES
                .iter()
                .find(|s| starts_with(chars, i, s))
                .map(|s| s.len())
                .or_else(|| starts_with(chars, i, "www.").then_some(0))
        } else {
            None
        };
        if let Some(len) = scheme {
            let mut e = i;
            while e < chars.len() && url_char(chars[e]) {
                e += 1;
            }
            let e = i + trim_url(&chars[i..e]);
            if e > i + len.max(4) {
                let s: String = chars[i..e].iter().collect();
                let url = if len == 0 { format!("https://{s}") } else { s };
                out.push((i, e, Target::Url(url)));
                taken[i..e].iter_mut().for_each(|t| *t = true);
                i = e;
                continue;
            }
        }
        i += 1;
    }
    // Paths: words with a slash or a file name, perhaps with :LINE(:COL).
    let sep = |c: char| c.is_whitespace() || "\"'`<>|()[]{}=,;".contains(c);
    let mut i = 0;
    while i < chars.len() {
        if sep(chars[i]) || taken[i] {
            i += 1;
            continue;
        }
        let s = i;
        while i < chars.len() && !sep(chars[i]) && !taken[i] {
            i += 1;
        }
        let mut e = i;
        while e > s && ".:!?".contains(chars[e - 1]) {
            e -= 1;
        }
        let word: String = chars[s..e].iter().collect();
        if let Some((path, line)) = path_word(&word) {
            let end = s
                + path.chars().count()
                + line.map_or(0, |_| {
                    word[path.len()..]
                        .chars()
                        .take_while(|c| *c == ':' || c.is_ascii_digit())
                        .count()
                });
            out.push((s, end.min(e), Target::Path(path, line)));
        }
    }
    out.sort_by_key(|c| c.0);
    out
}

/// A word that may be a path: `src/app.rs`, `./x`, `~/notes.md`,
/// `main.rs:12:5`, `/etc/fstab`.
fn path_word(word: &str) -> Option<(String, Option<u32>)> {
    if word.starts_with('-') || word.contains("://") {
        return None;
    }
    let mut parts = word.split(':');
    let first = parts.next()?;
    let line = parts.next().and_then(|n| n.parse::<u32>().ok());
    let path = if line.is_some() { first } else { word };
    if path.chars().count() < 2 || path.chars().all(|c| c == '.' || c == '/' || c == '~') {
        return None;
    }
    let named = |p: &str| {
        let name = p.rsplit('/').next().unwrap_or(p);
        match name.rsplit_once('.') {
            Some((stem, ext)) => {
                !stem.is_empty()
                    && (1..=10).contains(&ext.len())
                    && ext.chars().all(|c| c.is_ascii_alphanumeric())
                    && ext.chars().any(|c| c.is_ascii_alphabetic())
            }
            None => false,
        }
    };
    (path.contains('/') || named(path)).then(|| (path.to_string(), line))
}

/// Where a path from the screen is: relative to the shell's folder, `~/`
/// in the home folder; `a/` and `b/` of a git diff are tried without.
pub fn resolve(path: &str, cwd: &Path, home: &Path) -> Option<PathBuf> {
    let full = |p: &str| -> PathBuf {
        if let Some(rest) = p.strip_prefix("~/") {
            home.join(rest)
        } else if p == "~" {
            home.to_path_buf()
        } else {
            cwd.join(p)
        }
    };
    let p = full(path);
    if p.exists() {
        return Some(p);
    }
    let rest = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))?;
    let p = full(rest);
    p.exists().then_some(p)
}

/// Labels for `n` hints: one letter while there are few, else two.
pub fn labels(n: usize) -> Vec<String> {
    const KEYS: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";
    if n <= KEYS.len() {
        return KEYS[..n].iter().map(|&c| (c as char).to_string()).collect();
    }
    (0..n.min(KEYS.len() * KEYS.len()))
        .map(|i| {
            let a = KEYS[i / KEYS.len()] as char;
            let b = KEYS[i % KEYS.len()] as char;
            format!("{a}{b}")
        })
        .collect()
}

/// The hints between the lines `first` and `last`. `exists` turns a path as
/// written into one that is there, or drops it.
pub fn hints<T>(
    term: &Term<T>,
    first: i32,
    last: i32,
    exists: impl Fn(&str) -> Option<PathBuf>,
) -> Vec<Hint> {
    let mut out: Vec<Hint> = Vec::new();
    for_each_line(term, first, last, true, |lt| {
        let mut covered = vec![false; lt.chars.len()];
        // OSC 8 links: runs of cells with the same link.
        let mut i = 0;
        while i < lt.links.len() {
            let Some(uri) = &lt.links[i] else {
                i += 1;
                continue;
            };
            let s = i;
            while i < lt.links.len() && lt.links[i].as_ref() == Some(uri) {
                covered[i] = true;
                i += 1;
            }
            out.push(Hint {
                start: lt.points[s],
                end: lt.points[i - 1],
                text: lt.chars[s..i].iter().collect(),
                target: Target::Url(uri.clone()),
            });
        }
        for (s, e, target) in candidates(&lt.chars) {
            if e <= s || covered[s..e].iter().any(|c| *c) {
                continue;
            }
            let target = match target {
                Target::Path(p, line) => match exists(&p) {
                    Some(full) => Target::Path(full.to_string_lossy().into_owned(), line),
                    None => continue,
                },
                t => t,
            };
            out.push(Hint {
                start: lt.points[s],
                end: lt.points[e - 1],
                text: lt.chars[s..e].iter().collect(),
                target,
            });
        }
    });
    out.retain(|h| h.end.line.0 >= first && h.start.line.0 <= last);
    out
}

/// A file a text editor should open: no NUL byte in its first 8 kB.
pub fn is_text(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    path.is_file() && !buf[..n].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::GridSize;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    fn term(cols: usize, lines: usize, text: &str) -> Term<VoidListener> {
        let config = Config {
            scrolling_history: 100,
            ..Default::default()
        };
        let mut t = Term::new(config, &GridSize { cols, lines }, VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, text.as_bytes());
        t
    }

    fn c(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn search_finds_in_the_history_across_wrapped_lines_with_smart_case() {
        // 10 columns: "hello world" wraps after "hello worl".
        let t = term(
            10,
            3,
            "Error one\r\nok\r\nhello world\r\nerror two\r\nlast\r\n",
        );
        let all = find(&t, "error", -100, 2, 100);
        assert_eq!(all.len(), 2);
        assert!(all[0].0.line.0 < 0, "the first is in the history");
        assert_eq!(find(&t, "Error", -100, 2, 100).len(), 1);
        let w = find(&t, "world", -100, 2, 100);
        assert_eq!(w.len(), 1);
        assert_ne!(w[0].0.line, w[0].1.line, "found across the wrap");
        assert!(find(&t, "nothing", -100, 2, 100).is_empty());
        assert_eq!(find(&t, "o", -100, 2, 2).len(), 2, "the limit");
    }

    #[test]
    fn urls_are_found_and_trimmed_like_prose() {
        let got = candidates(&c(
            "see https://example.com/a_(b). or (http://x.org/p) and www.nirux.se, mailto:a@b.c",
        ));
        let urls: Vec<_> = got
            .into_iter()
            .filter_map(|(_, _, t)| match t {
                Target::Url(u) => Some(u),
                _ => None,
            })
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://example.com/a_(b)",
                "http://x.org/p",
                "https://www.nirux.se",
                "mailto:a@b.c"
            ]
        );
    }

    #[test]
    fn paths_with_lines_are_candidates() {
        let line = "error --> src/app.rs:12:5: in ./run.sh and ~/notes.md, not e.g. or -o or 1.5";
        let got: Vec<Target> = candidates(&c(line)).into_iter().map(|x| x.2).collect();
        assert!(got.contains(&Target::Path("src/app.rs".into(), Some(12))));
        assert!(got.contains(&Target::Path("./run.sh".into(), None)));
        assert!(got.contains(&Target::Path("~/notes.md".into(), None)));
        assert!(!got
            .iter()
            .any(|t| matches!(t, Target::Path(p, _) if p == "1.5" || p == "-o")));
        let (s, e, _) = candidates(&c("at main.rs:7 x"))[0].clone();
        assert_eq!((s, e), (3, 12), "the hint covers main.rs:7");
    }

    #[test]
    fn paths_resolve_from_the_shell_folder_home_and_git_diffs() {
        let dir = std::env::temp_dir().join(format!("niruterm-text-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("bin.dat"), [0u8, 1, 2]).unwrap();
        assert_eq!(resolve("src/a.rs", &dir, &dir), Some(dir.join("src/a.rs")));
        assert_eq!(
            resolve("b/src/a.rs", &dir, &dir),
            Some(dir.join("src/a.rs"))
        );
        assert_eq!(resolve("~/src", &dir, &dir), Some(dir.join("src")));
        assert_eq!(resolve("src/none.rs", &dir, &dir), None);
        assert!(is_text(&dir.join("src/a.rs")));
        assert!(!is_text(&dir.join("bin.dat")));
        assert!(!is_text(&dir.join("src")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hints_on_screen_with_osc8_links_and_labels() {
        let t = term(
            40,
            4,
            "\x1b]8;;https://nirux.se\x07site\x1b]8;;\x07 and http://a.b/c\r\nCargo.toml:3\r\n",
        );
        let hs = hints(&t, 0, 3, |p| {
            (p == "Cargo.toml").then(|| PathBuf::from("/x/Cargo.toml"))
        });
        let targets: Vec<Target> = hs.iter().map(|h| h.target.clone()).collect();
        assert_eq!(
            targets,
            vec![
                Target::Url("https://nirux.se".into()),
                Target::Url("http://a.b/c".into()),
                Target::Path("/x/Cargo.toml".into(), Some(3)),
            ]
        );
        assert_eq!(hs[0].text, "site");
        assert_eq!(labels(3), vec!["a", "s", "d"]);
        let many = labels(30);
        assert_eq!(many.len(), 30);
        assert!(many.iter().all(|l| l.len() == 2));
        let set: std::collections::HashSet<_> = many.iter().collect();
        assert_eq!(set.len(), 30);
    }
}

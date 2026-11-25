//! Shell integration: the semantic prompt marks of OSC 133 (FinalTerm, as
//! kitty, foot and WezTerm read them). The shell sends `A` where the prompt
//! starts, `B` where the typed command starts, `C` where its output starts
//! and `D;STATUS` when it has finished. NIRUTERM keeps where they are, to
//! jump between prompts, to copy the output of the last command and to tell
//! when a long command has finished.
//!
//! The marks are grid lines. Output moves the lines into the history, so the
//! marks follow a row of the grid: alacritty_terminal rotates its rows
//! without moving their cells, so the address of a row's first cell is the
//! same while the row scrolls. After a read, finding where the bottom row
//! went says how far everything moved. Nothing of this runs until the shell
//! has sent a mark.
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::Term;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A: the prompt starts.
    Prompt,
    /// B: the command line starts.
    Input,
    /// C: the command's output starts.
    Output,
    /// D: the command has finished.
    End,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    pub kind: Kind,
    pub line: i32,
    pub col: usize,
}

/// A command that has finished.
#[derive(Debug)]
pub struct Finished {
    pub took: Duration,
    pub status: Option<i32>,
    pub command: String,
}

#[derive(Default)]
pub struct Marks {
    pub list: VecDeque<Mark>,
    /// The address of the bottom row when the marks were last placed.
    anchor: Option<usize>,
    running: Option<Instant>,
    command: String,
}

/// More marks than this and the oldest go (a prompt makes about four).
const MAX: usize = 4000;

fn row_id(grid: &Grid<Cell>, line: i32) -> usize {
    &grid[Line(line)][Column(0)] as *const Cell as usize
}

fn top(grid: &Grid<Cell>) -> i32 {
    -(grid.history_size() as i32)
}

fn bottom(grid: &Grid<Cell>) -> i32 {
    grid.screen_lines() as i32 - 1
}

/// The first row of the logical line `line` is on (rows joined by wrapping).
fn logical_start(grid: &Grid<Cell>, mut line: i32) -> i32 {
    let last = Column(grid.columns() - 1);
    let top = top(grid);
    while line > top && grid[Line(line - 1)][last].flags.contains(Flags::WRAPLINE) {
        line -= 1;
    }
    line
}

/// The text from `start` up to, not including, `end`.
pub fn text_between<T>(term: &Term<T>, start: Point, end: Point) -> String {
    let grid = term.grid();
    let end = if end.column.0 == 0 {
        Point::new(end.line - 1, Column(grid.columns() - 1))
    } else {
        Point::new(end.line, end.column - 1)
    };
    if end < start || start.line.0 < top(grid) {
        return String::new();
    }
    term.bounds_to_string(start, end)
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn clear(&mut self) {
        self.list.clear();
        self.anchor = None;
    }

    /// Moves the marks with the output since the last call. The primary
    /// grid only: on the alternate screen nothing of it moves.
    pub fn sync(&mut self, grid: &Grid<Cell>) {
        if self.list.is_empty() {
            self.anchor = None;
            return;
        }
        let (top, bottom) = (top(grid), bottom(grid));
        let shift = self
            .anchor
            .and_then(|a| (top..=bottom).rev().find(|&l| row_id(grid, l) == a))
            .map(|l| bottom - l);
        match shift {
            Some(0) => {}
            Some(k) => {
                for m in &mut self.list {
                    m.line -= k;
                }
                self.list.retain(|m| m.line >= top);
            }
            // The grid was rebuilt (a reset): the marks are gone.
            None => self.list.clear(),
        }
        self.anchor = (!self.list.is_empty()).then(|| row_id(grid, bottom));
    }

    /// A read of `bytes` with `newlines` in it may scroll every row of the
    /// grid out and back: the rows could then not tell how far they moved.
    pub fn flood(&mut self, bytes: usize, newlines: usize, cols: usize, total_lines: usize) {
        if newlines + bytes / cols.max(1) >= total_lines {
            self.clear();
        }
    }

    /// A mark from the shell (what follows `133;`), at the cursor.
    pub fn record<T>(&mut self, params: &[u8], term: &Term<T>) -> Option<Finished> {
        let grid = term.grid();
        self.sync(grid);
        let kind = match params.first() {
            Some(b'A') => Kind::Prompt,
            Some(b'B') => Kind::Input,
            Some(b'C') => Kind::Output,
            Some(b'D') => Kind::End,
            _ => return None,
        };
        let at = grid.cursor.point;
        let (line, col) = (at.line.0, at.column.0);
        let mut done = None;
        match kind {
            Kind::Prompt => {
                // A prompt without D (a shell that sends only A and C).
                if let Some(t) = self.running.take() {
                    done = Some(Finished {
                        took: t.elapsed(),
                        status: None,
                        command: std::mem::take(&mut self.command),
                    });
                }
                // A prompt drawn again (fish repaints it) replaces the old one.
                self.list
                    .retain(|m| !(matches!(m.kind, Kind::Prompt | Kind::Input) && m.line >= line));
            }
            Kind::Input => {
                // Only one command line per prompt.
                if let Some(i) = self.list.iter().rposition(|m| m.kind == Kind::Prompt) {
                    let tail = self.list.split_off(i + 1);
                    self.list
                        .extend(tail.into_iter().filter(|m| m.kind != Kind::Input));
                }
            }
            Kind::Output => {
                if self.running.is_some() {
                    return None;
                }
                let input = self
                    .list
                    .iter()
                    .rev()
                    .take_while(|m| m.kind != Kind::Prompt)
                    .find(|m| m.kind == Kind::Input)
                    .copied();
                self.command = input
                    .map(|b| text_between(term, Point::new(Line(b.line), Column(b.col)), at))
                    .map(|s| s.trim().chars().take(200).collect())
                    .unwrap_or_default();
                self.running = Some(Instant::now());
            }
            Kind::End => {
                let t = self.running.take()?;
                let status = std::str::from_utf8(&params[1..])
                    .ok()
                    .and_then(|s| s.trim_start_matches(';').split(';').next())
                    .and_then(|s| s.trim().parse().ok());
                done = Some(Finished {
                    took: t.elapsed(),
                    status,
                    command: std::mem::take(&mut self.command),
                });
            }
        }
        self.list.push_back(Mark { kind, line, col });
        while self.list.len() > MAX {
            self.list.pop_front();
        }
        self.anchor = Some(row_id(grid, bottom(grid)));
        done
    }

    /// The lines where prompts start, oldest first.
    pub fn prompts(&self) -> impl Iterator<Item = i32> + '_ {
        self.list
            .iter()
            .filter(|m| m.kind == Kind::Prompt)
            .map(|m| m.line)
    }

    /// The output of the last command: of the last finished one, or of the
    /// one still running when none has finished.
    pub fn last_output<T>(&self, term: &Term<T>) -> Option<String> {
        let marks: Vec<&Mark> = self.list.iter().collect();
        let mut running = None;
        for (i, m) in marks.iter().enumerate().rev() {
            if m.kind != Kind::Output {
                continue;
            }
            let start = Point::new(Line(m.line), Column(m.col));
            match marks[i + 1..]
                .iter()
                .find(|e| matches!(e.kind, Kind::End | Kind::Prompt))
            {
                Some(e) => {
                    let end = Point::new(Line(e.line), Column(e.col));
                    return Some(text_between(term, start, end).trim_end().to_string());
                }
                None if running.is_none() => running = Some(start),
                None => {}
            }
        }
        let start = running?;
        let cursor = term.grid().cursor.point;
        Some(text_between(term, start, cursor).trim_end().to_string())
    }

    /// Where the marks are relative to the cursor, in logical lines, before
    /// a resize rewraps the lines.
    pub fn before_resize(&mut self, grid: &Grid<Cell>) -> Vec<(usize, usize)> {
        self.sync(grid);
        if self.list.is_empty() {
            return Vec::new();
        }
        let cols = grid.columns();
        let lowest = self.list.iter().map(|m| m.line).min().unwrap_or(0);
        let mut starts = Vec::new();
        let mut l = logical_start(grid, grid.cursor.point.line.0);
        loop {
            starts.push(l);
            if l <= lowest || l <= top(grid) {
                break;
            }
            l = logical_start(grid, l - 1);
        }
        self.list
            .iter()
            .map(|m| {
                let s = logical_start(grid, m.line);
                // `starts` goes up the screen: the logical lines above the cursor.
                let d = starts.iter().position(|&x| x <= s).unwrap_or(usize::MAX);
                (d, (m.line - s) as usize * cols + m.col)
            })
            .collect()
    }

    /// The marks again after the resize, from what `before_resize` kept.
    pub fn after_resize(&mut self, grid: &Grid<Cell>, saved: Vec<(usize, usize)>) {
        if saved.is_empty() {
            return;
        }
        let cols = grid.columns();
        let far = saved
            .iter()
            .map(|s| s.0)
            .filter(|&d| d != usize::MAX)
            .max()
            .unwrap_or(0);
        let mut starts = Vec::new();
        let mut l = logical_start(grid, grid.cursor.point.line.0);
        loop {
            starts.push(l);
            if starts.len() > far || l <= top(grid) {
                break;
            }
            l = logical_start(grid, l - 1);
        }
        let marks = std::mem::take(&mut self.list);
        for (m, (d, offset)) in marks.into_iter().zip(saved) {
            if let Some(&s) = starts.get(d) {
                let line = s + (offset / cols) as i32;
                if line <= bottom(grid) {
                    self.list.push_back(Mark {
                        line,
                        col: offset % cols,
                        ..m
                    });
                }
            }
        }
        self.anchor = (!self.list.is_empty()).then(|| row_id(grid, bottom(grid)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::GridSize;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    /// A terminal fed through the same scanner as the reader thread.
    struct T {
        term: Term<VoidListener>,
        parser: Processor,
        marks: Marks,
        done: Vec<Finished>,
    }

    impl T {
        fn new(cols: usize, lines: usize, history: usize) -> T {
            let config = Config {
                scrolling_history: history,
                ..Default::default()
            };
            T {
                term: Term::new(config, &GridSize { cols, lines }, VoidListener),
                parser: Processor::new(),
                marks: Marks::default(),
                done: Vec::new(),
            }
        }
        fn feed(&mut self, s: &str) {
            let mut sc = crate::graphics::Scanner::default();
            let mut pieces = Vec::new();
            sc.feed(s.as_bytes(), |p| match p {
                crate::graphics::Piece::Text(t) => pieces.push((false, t.to_vec())),
                crate::graphics::Piece::Owned(t) => pieces.push((false, t)),
                crate::graphics::Piece::Mark(m) => pieces.push((true, m)),
                crate::graphics::Piece::Command(_) => {}
            });
            for (mark, p) in pieces {
                if mark {
                    if let Some(f) = self.marks.record(&p, &self.term) {
                        self.done.push(f);
                    }
                } else {
                    self.parser.advance(&mut self.term, &p);
                }
            }
            self.marks.sync(self.term.grid());
        }
        /// One prompt, a command and its output, as bash with niruprompt.
        fn command(&mut self, cmd: &str, output: &str, status: i32) {
            self.feed(&format!(
                "\x1b]133;A\x07$ \x1b]133;B\x07{cmd}\r\n\x1b]133;C\x07{output}\x1b]133;D;{status}\x07"
            ));
        }
        fn prompt_text(&self, line: i32) -> String {
            let g = self.term.grid();
            (0..2).map(|c| g[Line(line)][Column(c)].c).collect()
        }
    }

    #[test]
    fn a_command_its_output_and_its_status() {
        let mut t = T::new(40, 10, 100);
        t.command("ls", "a\r\nb\r\n", 3);
        t.feed("\x1b]133;A\x07$ ");
        assert_eq!(t.marks.last_output(&t.term).as_deref(), Some("a\nb"));
        assert_eq!(t.done.len(), 1);
        assert_eq!(t.done[0].status, Some(3));
        assert_eq!(t.done[0].command, "ls");
        assert_eq!(t.marks.prompts().collect::<Vec<_>>(), vec![0, 3]);
    }

    #[test]
    fn marks_follow_the_lines_into_the_history() {
        let mut t = T::new(40, 5, 1000);
        t.command("one", "1\r\n", 0);
        t.command("two", &"x\r\n".repeat(30), 0);
        t.feed("\x1b]133;A\x07$ ");
        let p: Vec<i32> = t.marks.prompts().collect();
        assert_eq!(p.len(), 3);
        for l in &p {
            assert_eq!(t.prompt_text(*l), "$ ", "prompt at line {l}");
        }
        assert_eq!(
            t.marks.last_output(&t.term).unwrap(),
            "x\n".repeat(30).trim_end()
        );
    }

    #[test]
    fn marks_survive_a_full_history_and_leave_with_it() {
        let mut t = T::new(20, 4, 10);
        t.command("old", "o\r\n", 0);
        // Enough output to push the first prompt out of the 10-line history.
        t.command("many", &"m\r\n".repeat(25), 0);
        t.command("last", "z\r\n", 0);
        t.feed("\x1b]133;A\x07$ ");
        let p: Vec<i32> = t.marks.prompts().collect();
        assert_eq!(p.len(), 2, "{p:?}");
        for l in &p {
            assert_eq!(t.prompt_text(*l), "$ ");
        }
        assert_eq!(t.marks.last_output(&t.term).as_deref(), Some("z"));
    }

    #[test]
    fn clearing_the_screen_and_history_drops_the_marks() {
        let mut t = T::new(20, 4, 100);
        t.command("a", "1\r\n", 0);
        t.feed("\x1b[H\x1b[2J\x1b[3J");
        assert!(t.marks.prompts().next().is_none());
    }

    #[test]
    fn a_repainted_prompt_is_one_prompt_and_a_d_without_c_is_ignored() {
        let mut t = T::new(20, 4, 100);
        t.feed("\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        t.feed("\r\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(t.marks.prompts().count(), 1);
        assert!(t.done.is_empty());
        assert_eq!(t.marks.last_output(&t.term), None);
    }

    #[test]
    fn marks_keep_their_lines_when_a_resize_rewraps_them() {
        let mut t = T::new(20, 8, 100);
        t.command("w", &format!("{}\r\n", "y".repeat(30)), 0);
        t.feed("\x1b]133;A\x07$ ");
        let saved = t.marks.before_resize(t.term.grid());
        t.term.resize(GridSize { cols: 40, lines: 8 });
        t.marks.after_resize(t.term.grid(), saved);
        let p: Vec<i32> = t.marks.prompts().collect();
        assert_eq!(p.len(), 2);
        for l in &p {
            assert_eq!(t.prompt_text(*l), "$ ");
        }
        assert_eq!(t.marks.last_output(&t.term).unwrap(), "y".repeat(30));
    }

    #[test]
    fn a_flood_that_could_wrap_the_grid_clears_the_marks() {
        let mut m = Marks::default();
        m.list.push_back(Mark {
            kind: Kind::Prompt,
            line: 0,
            col: 0,
        });
        m.flood(65536, 65000, 80, 10_040);
        assert!(m.is_empty());
        m.list.push_back(Mark {
            kind: Kind::Prompt,
            line: 0,
            col: 0,
        });
        m.flood(65536, 100, 80, 10_040);
        assert!(!m.is_empty());
    }
}

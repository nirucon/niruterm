//! NIRUPROMPT: the NIRUX prompt, aliases and history set-up for bash and fish.
//!
//!   niruprompt init bash|fish        the shell code (eval/source it)
//!   niruprompt install bash|fish     add that line to ~/.bashrc or config.fish
//!   niruprompt style [NAME]          show or choose the prompt style
//!   niruprompt styles                preview every style
//!   niruprompt alias list|add NAME COMMAND|rm NAME
//!   niruprompt prompt --shell S --status N --duration MS --jobs N [--marks]
//!
//! With --marks the prompt carries the semantic prompt marks of OSC 133
//! (`A` before it, `B` after it), and the shell code sends `C` when a
//! command starts and `D;STATUS` when it ends, so NIRUTERM, kitty, foot and
//! WezTerm know where prompts and output are. Not on the Linux console or a
//! dumb terminal, which would print them.
//!
//! Colours are the terminal's own ANSI colours, so the prompt follows the
//! NIRUX theme in NIRUTERM and kitty alike. Git is read from .git directly;
//! only the "changed" mark runs git, with a short deadline.
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const STYLES: [(&str, &str); 6] = [
    (
        "nirux",
        "two lines: folder, branch and time, then ❯ (the NIRUX default)",
    ),
    (
        "runes",
        "two lines with Elder Futhark marks: ᚾ folder ᛫ branch, then ᛉ",
    ),
    ("ascii", "one retro line: [user@host ~/folder] (branch) $"),
    (
        "powerline",
        "coloured segments with arrows, then ❯ on the next line",
    ),
    ("minimal", "one line: folder ❯"),
    ("c64", "READY. and a block cursor, the Commodore way"),
];

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("nirux")
}

fn read_table(file: &str) -> toml::Table {
    std::fs::read_to_string(config_dir().join(file))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

fn write_table(file: &str, t: &toml::Table, header: &str) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let body = format!("{header}{}", toml::to_string(t).unwrap_or_default());
    let tmp = dir.join(format!(".{file}.tmp"));
    std::fs::write(&tmp, body)?;
    std::fs::rename(tmp, dir.join(file))
}

fn style() -> String {
    std::env::var("NIRUPROMPT_STYLE")
        .ok()
        .or_else(|| {
            read_table("prompt.toml")
                .get("style")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .filter(|s| STYLES.iter().any(|(n, _)| n == s))
        .unwrap_or_else(|| "nirux".into())
}

fn git_dirty_enabled() -> bool {
    read_table("prompt.toml")
        .get("git_changes")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

// ------------------------------------------------------------------ facts

struct Facts {
    path: String,
    branch: Option<String>,
    dirty: bool,
    status: i32,
    duration_ms: u64,
    jobs: u32,
    user: String,
    host: String,
    ssh: bool,
    root: bool,
    venv: Option<String>,
}

fn short_path(cwd: &Path) -> String {
    let home = home();
    let s = match cwd.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => return "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => cwd.display().to_string(),
    };
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() > 5 {
        format!("{}/…/{}", parts[0], parts[parts.len() - 3..].join("/"))
    } else {
        s
    }
}

/// The branch from .git/HEAD, without running git.
fn git_branch(cwd: &Path) -> Option<(String, PathBuf)> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let g = d.join(".git");
        let gitdir = if g.is_dir() {
            g
        } else if g.is_file() {
            // A worktree or submodule: "gitdir: PATH".
            let t = std::fs::read_to_string(&g).ok()?;
            let p = PathBuf::from(t.trim().strip_prefix("gitdir: ")?);
            if p.is_absolute() {
                p
            } else {
                d.join(p)
            }
        } else {
            dir = d.parent();
            continue;
        };
        let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
        let head = head.trim();
        let name = match head.strip_prefix("ref: refs/heads/") {
            Some(b) => b.to_string(),
            None => head.chars().take(7).collect(),
        };
        return Some((name, d.to_path_buf()));
    }
    None
}

fn git_dirty(top: &Path) -> bool {
    let Ok(mut child) = std::process::Command::new("git")
        .args(["-C"])
        .arg(top)
        .args([
            "status",
            "--porcelain=v1",
            "--untracked-files=no",
            "--ignore-submodules",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    // A huge repository must not slow the prompt down: 150 ms at most.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > Duration::from_millis(150) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return false,
        }
    }
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut out);
    }
    !out.trim().is_empty()
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "host".into())
}

fn facts(status: i32, duration_ms: u64, jobs: u32) -> Facts {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let (branch, dirty) = match git_branch(&cwd) {
        Some((b, top)) => {
            let d = git_dirty_enabled() && git_dirty(&top);
            (Some(b), d)
        }
        None => (None, false),
    };
    Facts {
        path: short_path(&cwd),
        branch,
        dirty,
        status,
        duration_ms,
        jobs,
        user: std::env::var("USER").unwrap_or_else(|_| "user".into()),
        host: hostname(),
        ssh: std::env::var_os("SSH_CONNECTION").is_some(),
        root: unsafe { libc::geteuid() } == 0,
        venv: std::env::var("VIRTUAL_ENV").ok().and_then(|v| {
            Path::new(&v)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        }),
    }
}

// --------------------------------------------------------------- drawing

struct P {
    out: String,
    bash: bool,
}

impl P {
    /// An escape sequence, marked invisible for bash's line editor.
    fn esc(&mut self, code: &str) {
        if self.bash {
            self.out.push('\x01');
        }
        self.out.push_str("\x1b[");
        self.out.push_str(code);
        self.out.push('m');
        if self.bash {
            self.out.push('\x02');
        }
    }
    fn text(&mut self, code: &str, s: &str) {
        self.esc(code);
        self.out.push_str(s);
        self.esc("0");
    }
    fn raw(&mut self, s: &str) {
        self.out.push_str(s);
    }
}

fn duration(ms: u64) -> Option<String> {
    if ms < 2000 {
        return None;
    }
    let s = ms / 1000;
    Some(if s >= 3600 {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    })
}

// ANSI colours: 31 red, 32 green, 33 yellow, 34 blue, 35 magenta, 36 cyan, 90 grey.
fn render(f: &Facts, style: &str, bash: bool) -> String {
    let mut p = P {
        out: String::new(),
        bash,
    };
    let ok = f.status == 0;
    let mark_col = if ok { "32" } else { "31" };
    let branch = f
        .branch
        .as_ref()
        .map(|b| format!("{b}{}", if f.dirty { "*" } else { "" }));
    let dur = duration(f.duration_ms);
    let who = if f.ssh || f.root {
        Some(format!("{}@{}", f.user, f.host))
    } else {
        None
    };
    match style {
        "ascii" => {
            p.text("90", "[");
            p.text(
                if f.root { "31" } else { "36" },
                &format!("{}@{}", f.user, f.host),
            );
            p.raw(" ");
            p.text("1;34", &f.path);
            p.text("90", "]");
            if let Some(b) = &branch {
                p.text("33", &format!(" ({b})"));
            }
            if let Some(d) = &dur {
                p.text("90", &format!(" {d}"));
            }
            if !ok {
                p.text("31", &format!(" [{}]", f.status));
            }
            p.text(mark_col, if f.root { " # " } else { " $ " });
        }
        "minimal" => {
            p.text("34", &f.path);
            if let Some(b) = &branch {
                p.text("90", &format!(" {b}"));
            }
            p.text(mark_col, " ❯ ");
        }
        "runes" => {
            p.text("35", "ᚾ ");
            if let Some(w) = &who {
                p.text("36", &format!("{w} ᛫ "));
            }
            p.text("1;34", &f.path);
            if let Some(b) = &branch {
                p.text("90", " ᛫ ");
                p.text("33", b);
            }
            if let Some(v) = &f.venv {
                p.text("90", " ᛫ ");
                p.text("36", v);
            }
            if let Some(d) = &dur {
                p.text("90", &format!(" ᛫ {d}"));
            }
            if f.jobs > 0 {
                p.text("90", &format!(" ᛫ ᛃ{}", f.jobs));
            }
            p.raw("\n");
            p.text(mark_col, if ok { "ᛉ " } else { "ᛣ " });
        }
        "powerline" => {
            // Segments: who (cyan), folder (blue), branch (yellow), time (grey).
            let mut segs: Vec<(&str, &str, String)> = Vec::new();
            if let Some(w) = &who {
                segs.push(("46", "36", format!(" {w} ")));
            }
            segs.push(("44", "34", format!(" {} ", f.path)));
            if let Some(b) = &branch {
                segs.push(("43", "33", format!(" \u{e0a0} {b} ")));
            }
            if let Some(d) = &dur {
                segs.push(("100", "90", format!(" {d} ")));
            }
            if !ok {
                segs.push(("41", "31", format!(" {} ", f.status)));
            }
            for (k, (bg, fgc, txt)) in segs.iter().enumerate() {
                p.esc(&format!("30;{bg}"));
                p.raw(txt);
                let next_bg = segs.get(k + 1).map(|s| s.0);
                match next_bg {
                    Some(nb) => p.esc(&format!("{fgc};{nb}")),
                    None => p.esc(&format!("0;{fgc}")),
                }
                p.raw("\u{e0b0}");
            }
            p.esc("0");
            p.raw("\n");
            p.text(mark_col, if f.root { "# " } else { "❯ " });
        }
        "c64" => {
            if !ok {
                p.text("31", &format!("?ERROR {}\n", f.status));
            }
            p.text("1;36", "READY.");
            p.raw(" ");
            p.text("34", &f.path);
            if let Some(b) = &branch {
                p.text("33", &format!(" {b}"));
            }
            if let Some(d) = &dur {
                p.text("90", &format!(" {d}"));
            }
            p.raw("\n");
            p.text("36", "▌");
            p.raw(" ");
        }
        _ => {
            // nirux: as the NIRUX fish prompt, for bash too.
            if let Some(w) = &who {
                p.text("36", &format!("{w} "));
            }
            p.text("34", &f.path);
            if let Some(b) = &branch {
                p.text("33", &format!(" {b}"));
            }
            if let Some(v) = &f.venv {
                p.text("36", &format!(" ({v})"));
            }
            if let Some(d) = &dur {
                p.text("90", &format!(" {d}"));
            }
            if f.jobs > 0 {
                p.text("90", &format!(" ✦{}", f.jobs));
            }
            p.raw("\n");
            p.text(mark_col, if f.root { "# " } else { "❯ " });
        }
    }
    p.out
}

/// The prompt between OSC 133 `A` and `B`, hidden from bash's line editor.
fn with_marks(prompt: &str, bash: bool) -> String {
    let (open, close) = if bash { ("\x01", "\x02") } else { ("", "") };
    format!("{open}\x1b]133;A\x07{close}{prompt}{open}\x1b]133;B\x07{close}")
}

// -------------------------------------------------------------- aliases

/// Defaults everybody gets; the user's aliases.toml wins.
const DEFAULT_ALIASES: [(&str, &str); 7] = [
    ("ls", "ls --color=auto"),
    ("ll", "ls -lh --color=auto"),
    ("la", "ls -lAh --color=auto"),
    ("grep", "grep --color=auto"),
    ("..", "cd .."),
    ("...", "cd ../.."),
    ("icat", "niruterm icat"),
];

fn aliases() -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = DEFAULT_ALIASES
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let t = read_table("aliases.toml");
    if let Some(a) = t.get("aliases").and_then(|v| v.as_table()) {
        for (k, v) in a {
            match v.as_str() {
                Some("") => {
                    m.remove(k);
                }
                Some(s) => {
                    m.insert(k.clone(), s.to_string());
                }
                None => {}
            }
        }
    }
    m
}

fn valid_alias(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "._-+".contains(c))
}

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn alias_cmd(args: &[String]) -> i32 {
    let header = "# NIRUX aliases for bash and fish (niruprompt alias add|rm|list).\n# An empty value removes a default alias.\n\n";
    match args.first().map(String::as_str) {
        None | Some("list") => {
            for (k, v) in aliases() {
                println!("{k:<10} {v}");
            }
            0
        }
        Some("add") if args.len() >= 3 => {
            let name = &args[1];
            if !valid_alias(name) {
                eprintln!("niruprompt: an alias name is letters, digits and . _ - +");
                return 2;
            }
            let cmd = args[2..].join(" ");
            let mut t = read_table("aliases.toml");
            let a = t
                .entry("aliases")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(tab) = a.as_table_mut() {
                tab.insert(name.clone(), toml::Value::String(cmd.clone()));
            }
            match write_table("aliases.toml", &t, header) {
                Ok(()) => {
                    println!("{name} = {cmd} (new shells; in this one: reload with `exec $SHELL`)");
                    0
                }
                Err(e) => {
                    eprintln!("niruprompt: {e}");
                    1
                }
            }
        }
        Some("rm") | Some("remove") if args.len() == 2 => {
            let name = &args[1];
            let mut t = read_table("aliases.toml");
            let a = t
                .entry("aliases")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(tab) = a.as_table_mut() {
                if DEFAULT_ALIASES.iter().any(|(k, _)| k == name) {
                    tab.insert(name.clone(), toml::Value::String(String::new()));
                } else {
                    tab.remove(name);
                }
            }
            match write_table("aliases.toml", &t, header) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("niruprompt: {e}");
                    1
                }
            }
        }
        _ => {
            eprintln!("usage: niruprompt alias list | add NAME COMMAND... | rm NAME");
            2
        }
    }
}

// ----------------------------------------------------------------- init

fn init_bash(me: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        r#"# NIRUPROMPT for bash. Generated by: {me} init bash
case $- in *i*) ;; *) return 0 2>/dev/null ;; esac
# History: large, shared between terminals, no duplicates.
HISTSIZE=100000
HISTFILESIZE=200000
HISTCONTROL=ignoreboth:erasedups
HISTTIMEFORMAT='%F %T '
shopt -s histappend cmdhist checkwinsize autocd 2>/dev/null
# Completion and history search the fish way.
if [ -z "${{BASH_COMPLETION_VERSINFO-}}" ]; then
  for __niru_c in /usr/share/bash-completion/bash_completion /etc/bash_completion; do
    [ -r "$__niru_c" ] && . "$__niru_c" && break
  done
  unset __niru_c
fi
bind 'set completion-ignore-case on' 2>/dev/null
bind 'set show-all-if-ambiguous on' 2>/dev/null
bind 'set colored-stats on' 2>/dev/null
bind 'set colored-completion-prefix on' 2>/dev/null
bind 'set mark-symlinked-directories on' 2>/dev/null
bind '"\e[A": history-search-backward' 2>/dev/null
bind '"\e[B": history-search-forward' 2>/dev/null
bind '"\eOA": history-search-backward' 2>/dev/null
bind '"\eOB": history-search-forward' 2>/dev/null
if command -v fzf >/dev/null 2>&1; then
  __niru_history() {{
    local c
    c=$(HISTTIMEFORMAT= history | sed 's/^ *[0-9]* *//' | tac | awk '!seen[$0]++' |
        fzf --height 40% --reverse --no-sort --query "$READLINE_LINE" --prompt 'history ❯ ') || return
    READLINE_LINE=$c
    READLINE_POINT=${{#c}}
  }}
  bind -x '"\C-r": __niru_history' 2>/dev/null
fi
# Shell integration (OSC 133): where prompts, commands and output are.
__niru_marks=
case "${{TERM-}}" in linux|dumb|'') ;; *)
  __niru_marks=--marks
  case "${{PS0-}}" in *'133;C'*) ;; *) PS0='\e]133;C\a'"${{PS0-}}" ;; esac ;;
esac
# The prompt, with the time of the last command.
__niru_start=
__niru_mark() {{ [ -z "$__niru_start" ] && __niru_start=${{EPOCHREALTIME/./}}; }}
trap '__niru_mark' DEBUG
__niru_prompt() {{
  local s=$? d=0
  [ -n "$__niru_marks" ] && printf '\e]133;D;%s\a' "$s"
  [ -n "$__niru_start" ] && d=$(( (${{EPOCHREALTIME/./}} - __niru_start) / 1000 ))
  history -a
  PS1=$({me} prompt --shell bash --status "$s" --duration "$d" --jobs "$(jobs -p | wc -l)" $__niru_marks)
  __niru_start=
}}
case ";${{PROMPT_COMMAND-}};" in *";__niru_prompt;"*) ;; *) PROMPT_COMMAND="__niru_prompt${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}" ;; esac
"#
    ));
    s.push_str("# Aliases (~/.config/nirux/aliases.toml).\n");
    for (k, v) in aliases() {
        if k.starts_with('-') {
            continue;
        }
        s.push_str(&format!("alias {k}={}\n", sq(&v)));
    }
    s
}

fn init_fish(me: &str) -> String {
    let mut s = format!(
        r#"# NIRUPROMPT for fish. Generated by: {me} init fish
status is-interactive; or exit 0
# Shell integration (OSC 133); fish 4 sends the marks itself.
set -g __niru_marks
if string match -qr '^[0-3]\.' -- $version; and not contains -- "$TERM" linux dumb ''
    set -g __niru_marks --marks
    function __niru_preexec --on-event fish_preexec
        printf '\e]133;C\a'
    end
    function __niru_postexec --on-event fish_postexec
        printf '\e]133;D;%s\a' $status
    end
end
function fish_prompt
    set -l s $status
    set -l d 0
    set -q CMD_DURATION; and set d $CMD_DURATION
    {me} prompt --shell fish --status $s --duration $d --jobs (count (jobs -p)) $__niru_marks
end
# Aliases as abbreviations: they expand while you type.
"#
    );
    for (k, v) in aliases() {
        if k == ".." || k == "..." {
            // fish already has .. and cd -; an abbreviation must not start with a dot.
            continue;
        }
        s.push_str(&format!("abbr -a -- {k} {}\n", sq(&v)));
    }
    s
}

const MARK: &str = "# niruprompt (niruprompt install)";

fn install(shell: &str, me: &str) -> i32 {
    let (file, line) = match shell {
        "bash" => (
            home().join(".bashrc"),
            format!("eval \"$({me} init bash)\""),
        ),
        "fish" => (
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".config"))
                .join("fish/config.fish"),
            format!("{me} init fish | source"),
        ),
        _ => {
            eprintln!("usage: niruprompt install bash|fish");
            return 2;
        }
    };
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if text.contains(MARK) {
        println!("{} already loads niruprompt", file.display());
        return 0;
    }
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut body = text;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    // Last in the file, so it wins over earlier prompts.
    body.push_str(&format!("\n{MARK}\n{line}\n"));
    match std::fs::write(&file, body) {
        Ok(()) => {
            println!("added to {}; open a new terminal", file.display());
            0
        }
        Err(e) => {
            eprintln!("niruprompt: {}: {e}", file.display());
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let me = std::env::current_exe()
        .ok()
        .filter(|p| p.is_file())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "niruprompt".into());
    let me = if me.contains(char::is_whitespace) {
        "niruprompt".to_string()
    } else {
        me
    };
    let code = match args.first().map(String::as_str) {
        Some("prompt") => {
            let mut shell = "fish".to_string();
            let (mut status, mut dur, mut jobs) = (0i32, 0u64, 0u32);
            let mut marks = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--shell" => shell = it.next().cloned().unwrap_or_default(),
                    "--status" => status = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    "--duration" => dur = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    "--jobs" => jobs = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0),
                    "--marks" => marks = true,
                    _ => {}
                }
            }
            let f = facts(status, dur, jobs);
            let mut out = std::io::stdout().lock();
            let bash = shell == "bash";
            let mut p = render(&f, &style(), bash);
            if marks {
                p = with_marks(&p, bash);
            }
            let _ = out.write_all(p.as_bytes());
            0
        }
        Some("init") => match args.get(1).map(String::as_str) {
            Some("bash") => {
                print!("{}", init_bash(&me));
                0
            }
            Some("fish") => {
                print!("{}", init_fish(&me));
                0
            }
            _ => {
                eprintln!("usage: niruprompt init bash|fish");
                2
            }
        },
        Some("install") => install(args.get(1).map(String::as_str).unwrap_or(""), &me),
        Some("style") => {
            match args.get(1) {
                None => {
                    println!("{}", style());
                    0
                }
                Some(n) if STYLES.iter().any(|(s, _)| s == n) => {
                    let mut t = read_table("prompt.toml");
                    t.insert("style".into(), toml::Value::String(n.clone()));
                    match write_table("prompt.toml", &t, "# NIRUX prompt (niruprompt style NAME; niruprompt styles shows them all).\n") {
                    Ok(()) => {
                        println!("prompt style: {n}");
                        0
                    }
                    Err(e) => {
                        eprintln!("niruprompt: {e}");
                        1
                    }
                }
                }
                Some(n) => {
                    eprintln!("niruprompt: no style {n}; see niruprompt styles");
                    2
                }
            }
        }
        Some("styles") => {
            let f = facts(0, 4200, 0);
            for (name, what) in STYLES {
                println!("\x1b[1m{name}\x1b[0m: {what}");
                println!("{}ls", render(&f, name, false));
                println!();
            }
            0
        }
        Some("alias") | Some("aliases") => alias_cmd(&args[1..]),
        Some("--version") | Some("-V") => {
            println!("NIRUPROMPT {}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => {
            println!(
                "NIRUPROMPT: the NIRUX prompt, aliases and history for bash and fish\n\n  niruprompt install bash|fish   load it in new shells\n  niruprompt style [NAME]        show or choose: nirux, runes, ascii, powerline, minimal, c64\n  niruprompt styles              preview every style\n  niruprompt alias list|add NAME COMMAND|rm NAME\n  niruprompt init bash|fish      the shell code"
            );
            0
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> Facts {
        Facts {
            path: "~/Git/nirux".into(),
            branch: Some("main".into()),
            dirty: true,
            status: 1,
            duration_ms: 65_000,
            jobs: 0,
            user: "niru".into(),
            host: "othala".into(),
            ssh: false,
            root: false,
            venv: None,
        }
    }

    #[test]
    fn bash_marks_every_escape_as_invisible() {
        let s = render(&f(), "nirux", true);
        assert_eq!(s.matches('\x01').count(), s.matches('\x1b').count());
        assert_eq!(s.matches('\x02').count(), s.matches('\x1b').count());
        assert!(s.contains("main*") && s.contains("1m5s") && s.contains("❯"));
        assert!(!render(&f(), "nirux", false).contains('\x01'));
    }

    #[test]
    fn prompt_marks_wrap_the_prompt_and_bash_hides_them() {
        let p = with_marks(&render(&f(), "nirux", true), true);
        assert!(p.starts_with("\x01\x1b]133;A\x07\x02"));
        assert!(p.ends_with("\x01\x1b]133;B\x07\x02"));
        assert_eq!(p.matches('\x01').count(), p.matches('\x1b').count());
        let fish = with_marks("❯ ", false);
        assert_eq!(fish, "\x1b]133;A\x07❯ \x1b]133;B\x07");
    }

    #[test]
    fn the_shell_code_sends_the_command_marks() {
        let bash = init_bash("niruprompt");
        assert!(bash.contains("PS0='\\e]133;C\\a'"));
        assert!(bash.contains("printf '\\e]133;D;%s\\a' \"$s\""));
        assert!(bash.contains("$__niru_marks)"));
        assert!(bash.contains("linux|dumb"));
        let fish = init_fish("niruprompt");
        assert!(fish.contains("--on-event fish_preexec"));
        assert!(fish.contains("--on-event fish_postexec"));
        assert!(fish.contains("$__niru_marks\nend"));
    }

    #[test]
    fn every_style_renders_the_folder() {
        for (name, _) in STYLES {
            assert!(render(&f(), name, false).contains("~/Git/nirux"), "{name}");
        }
    }

    #[test]
    fn long_paths_keep_the_last_folders() {
        let home = home();
        assert_eq!(short_path(&home), "~");
        assert_eq!(short_path(&home.join("a/b/c/d/e/f")), "~/…/d/e/f");
        assert_eq!(short_path(Path::new("/etc")), "/etc");
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(duration(1500), None);
        assert_eq!(duration(4200).as_deref(), Some("4s"));
        assert_eq!(duration(3_725_000).as_deref(), Some("1h2m"));
    }
}

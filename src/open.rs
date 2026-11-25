//! Programs NIRUTERM starts for the user: links and files (xdg-open), the
//! editor at a line, and desktop notifications (notify-send, else gdbus, to
//! the org.freedesktop.Notifications service NIRUNOTIFY provides). Each runs
//! detached in its own session; nothing waits for it.
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A program in PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Starts `c` on its own and forgets it (a thread reaps it).
pub fn detach(mut c: Command) {
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Ok(mut child) = c.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// A link or a file in the program the desktop has for it.
pub fn xdg_open(target: &str) {
    let mut c = Command::new("xdg-open");
    c.arg(target);
    detach(c);
}

/// Editors with a window of their own; the rest run in a terminal.
const GUI: [&str; 13] = [
    "code",
    "codium",
    "code-oss",
    "subl",
    "zed",
    "gedit",
    "gnome-text-editor",
    "kate",
    "mousepad",
    "xed",
    "pluma",
    "geany",
    "nirunote",
];

/// The command that opens `path` at `line` in `editor` (a command line,
/// such as "nvim" or "code --new-window"), and whether it has its own window.
pub fn editor_argv(editor: &str, path: &str, line: Option<u32>) -> Option<(Vec<String>, bool)> {
    let mut argv: Vec<String> = editor.split_whitespace().map(str::to_string).collect();
    let name = Path::new(argv.first()?)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    let gui = GUI.contains(&name.as_str());
    match (name.as_str(), line) {
        (_, None) => argv.push(path.into()),
        ("code" | "codium" | "code-oss", Some(n)) => {
            argv.push("--goto".into());
            argv.push(format!("{path}:{n}"));
        }
        ("subl" | "zed" | "hx" | "helix" | "micro", Some(n)) => argv.push(format!("{path}:{n}")),
        ("kate", Some(n)) => {
            argv.extend(["--line".into(), n.to_string(), path.into()]);
        }
        // vi, vim, nvim, nano, emacs, kak, gedit, mousepad and most others.
        (_, Some(n)) => {
            argv.push(format!("+{n}"));
            argv.push(path.into());
        }
    }
    Some((argv, gui))
}

/// The editor: the `editor` setting, else $VISUAL or $EDITOR.
pub fn editor(setting: Option<&str>) -> Option<String> {
    setting
        .map(str::to_string)
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|e| !e.trim().is_empty())
}

/// Opens a file from the screen: a text file in the editor at its line (in
/// a new NIRUTERM window when the editor runs in a terminal), anything else
/// with xdg-open.
pub fn file(path: &str, line: Option<u32>, editor_setting: Option<&str>) {
    let p = Path::new(path);
    let argv = editor(editor_setting)
        .filter(|_| crate::text::is_text(p))
        .and_then(|e| editor_argv(&e, path, line));
    let Some((argv, gui)) = argv else {
        return xdg_open(path);
    };
    let dir = p.parent().unwrap_or(Path::new("/"));
    let c = if gui {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]).current_dir(dir);
        c
    } else {
        let mut c = Command::new(std::env::current_exe().unwrap_or_else(|_| "niruterm".into()));
        c.arg("--working-directory").arg(dir).arg("-e").args(&argv);
        c
    };
    detach(c);
}

/// A desktop notification.
pub fn notify(summary: &str, body: &str) {
    let c = if let Some(p) = which("notify-send") {
        let mut c = Command::new(p);
        c.args([
            "-a",
            "NIRUTERM",
            "-i",
            "utilities-terminal",
            "--",
            summary,
            body,
        ]);
        c
    } else if let Some(p) = which("gdbus") {
        let mut c = Command::new(p);
        c.args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.Notifications",
            "--object-path",
            "/org/freedesktop/Notifications",
            "--method",
            "org.freedesktop.Notifications.Notify",
            "NIRUTERM",
            "0",
            "utilities-terminal",
            summary,
            body,
            "[]",
            "{}",
            "-1",
        ]);
        c
    } else {
        return;
    };
    detach(c);
}

/// "1m 5s", "12s", "2h 3m".
pub fn took(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editors_get_the_line_their_way() {
        let a = |e: &str, l| editor_argv(e, "/p/a.rs", l).unwrap();
        assert_eq!(
            a("nvim", Some(12)),
            (vec!["nvim".into(), "+12".into(), "/p/a.rs".into()], false)
        );
        assert_eq!(
            a("vim", None),
            (vec!["vim".into(), "/p/a.rs".into()], false)
        );
        assert_eq!(
            a("/usr/bin/hx", Some(3)).0,
            vec!["/usr/bin/hx", "/p/a.rs:3"]
        );
        let (code, gui) = a("code --new-window", Some(4));
        assert_eq!(code, vec!["code", "--new-window", "--goto", "/p/a.rs:4"]);
        assert!(gui);
        assert_eq!(a("kate", Some(5)).0, vec!["kate", "--line", "5", "/p/a.rs"]);
        assert!(editor_argv("  ", "/p", None).is_none());
    }

    #[test]
    fn durations_for_notifications() {
        use std::time::Duration;
        assert_eq!(took(Duration::from_secs(12)), "12s");
        assert_eq!(took(Duration::from_secs(65)), "1m 5s");
        assert_eq!(took(Duration::from_secs(7380)), "2h 3m");
    }
}

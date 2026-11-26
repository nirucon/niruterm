//! Paste protection: a paste that would run at once (a line break while the
//! program has no bracketed paste) or that looks dangerous (sudo, su, a
//! download piped into a shell) is shown first and waits for Enter.
//! Bracketed paste lets the shell hold a multi-line paste until Enter, so it
//! is only asked about for the dangerous patterns, and a full-screen program
//! with bracketed paste (an editor) is never asked about.

/// Why a paste waits for Enter.
#[derive(Debug, PartialEq)]
pub struct Reason {
    /// What makes it dangerous, as a short phrase ("runs sudo").
    pub danger: Option<String>,
    /// It has a line break and would run at once.
    pub runs_at_once: bool,
}

impl Reason {
    /// The sentence the confirmation shows.
    pub fn sentence(&self) -> String {
        match (&self.danger, self.runs_at_once) {
            (Some(d), true) => format!("It {d} and runs at once when pasted."),
            (Some(d), false) => format!("It {d}. Check it before you paste."),
            (None, _) => "Its line breaks run it at once, without Enter.".to_string(),
        }
    }
}

/// Whether a paste must wait for Enter, and why.
pub fn check(text: &str, bracketed: bool, alt_screen: bool) -> Option<Reason> {
    if bracketed && alt_screen {
        return None;
    }
    let runs_at_once = !bracketed && text.contains(['\n', '\r']);
    let danger = danger(text);
    (runs_at_once || danger.is_some()).then_some(Reason {
        danger,
        runs_at_once,
    })
}

/// Commands that run something as another user.
const ELEVATE: [&str; 5] = ["sudo", "su", "doas", "pkexec", "run0"];
/// Shells and interpreters a download should not be piped into.
const SHELLS: [&str; 10] = [
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "python", "python3", "perl",
];
/// Words that only start another command (`env FOO=1 sudo`, `nohup`).
const WRAPPERS: [&str; 7] = ["env", "nohup", "time", "command", "exec", "nice", "xargs"];

/// The first dangerous pattern in the text, as a phrase for the sentence.
pub fn danger(text: &str) -> Option<String> {
    let fetch = text.contains("curl") || text.contains("wget");
    for (piped, words) in commands(text) {
        let mut words = words.iter().map(|w| w.rsplit('/').next().unwrap_or(w));
        // Skip variable assignments and wrappers to reach the command.
        let cmd = words.find(|w| !w.contains('=') && !WRAPPERS.contains(w) && !w.starts_with('-'));
        let Some(cmd) = cmd else { continue };
        if ELEVATE.contains(&cmd) {
            return Some(if piped {
                format!("pipes text into {cmd}")
            } else {
                format!("runs {cmd}")
            });
        }
        if SHELLS.contains(&cmd) && (piped || fetch && (text.contains("<(") || text.contains("$(")))
        {
            return Some(if fetch {
                format!("runs a download with {cmd}")
            } else {
                format!("pipes text into {cmd}")
            });
        }
    }
    None
}

/// The simple commands of a shell text: their words, and whether output was
/// piped into them. Quotes are respected well enough for a warning: a word
/// inside quotes never starts a command, `$(`, backticks, `(`, `{`, `;`,
/// `&`, `|` and line breaks do.
fn commands(text: &str) -> Vec<(bool, Vec<String>)> {
    let mut out: Vec<(bool, Vec<String>)> = vec![(false, Vec::new())];
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    let end_word = |out: &mut Vec<(bool, Vec<String>)>, word: &mut String| {
        if !word.is_empty() {
            out.last_mut().unwrap().1.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') => quote = None,
            (Some('\''), _) => word.push(c),
            (Some('"'), '"') => quote = None,
            (Some('"'), '$') if chars.peek() == Some(&'(') => {
                // A command inside double quotes: "$(curl ...)".
                chars.next();
                end_word(&mut out, &mut word);
                out.push((false, Vec::new()));
                quote = None;
            }
            (Some(_), _) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    if n != '\n' {
                        word.push(n);
                    }
                }
            }
            (None, ' ' | '\t') => end_word(&mut out, &mut word),
            (None, '|') => {
                end_word(&mut out, &mut word);
                // `||` is "or", not a pipe.
                let piped = chars.peek() != Some(&'|');
                if !piped {
                    chars.next();
                }
                out.push((piped, Vec::new()));
            }
            (None, ';' | '&' | '\n' | '\r' | '(' | ')' | '{' | '}' | '`') => {
                end_word(&mut out, &mut word);
                out.push((false, Vec::new()));
            }
            (None, '$') if chars.peek() == Some(&'(') => {
                chars.next();
                end_word(&mut out, &mut word);
                out.push((false, Vec::new()));
            }
            (None, '<') if chars.peek() == Some(&'(') => {
                chars.next();
                end_word(&mut out, &mut word);
                out.push((false, Vec::new()));
            }
            _ => word.push(c),
        }
    }
    end_word(&mut out, &mut word);
    out.retain(|(_, w)| !w.is_empty());
    out
}

/// The lines of a paste as the confirmation shows them: tabs as spaces,
/// other control characters in caret notation (`^[`), at most `max` lines.
pub fn preview(text: &str, max: usize) -> (Vec<String>, usize) {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let all: Vec<&str> = text.split('\n').collect();
    let shown = all
        .iter()
        .take(max)
        .map(|l| {
            let mut s = String::new();
            for c in l.chars() {
                match c {
                    '\t' => s.push_str("    "),
                    c if (c as u32) < 0x20 => {
                        s.push('^');
                        s.push((c as u8 + 0x40) as char);
                    }
                    '\x7f' => s.push_str("^?"),
                    c if c.is_control() => s.push('?'),
                    c => s.push(c),
                }
            }
            s
        })
        .collect();
    (shown, all.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_single_line_paste_is_never_asked_about() {
        for text in [
            "ls -la",
            "git commit -m 'fix'",
            "echo sudo is a word",
            "https://x.org/a|b",
        ] {
            assert_eq!(check(text, false, false), None, "{text}");
            assert_eq!(check(text, true, false), None, "{text}");
        }
        assert_eq!(check("pseudo make", false, false), None);
        assert_eq!(check("a || b", false, false), None);
    }

    #[test]
    fn a_line_break_asks_only_without_bracketed_paste() {
        let r = check("echo one\necho two\n", false, false).unwrap();
        assert!(r.runs_at_once && r.danger.is_none());
        assert_eq!(check("echo one\necho two\n", true, false), None);
        assert!(check("ls\r", false, false).unwrap().runs_at_once);
    }

    #[test]
    fn sudo_su_and_downloads_into_a_shell_always_ask_except_in_an_editor() {
        let d = |t: &str| danger(t);
        assert_eq!(d("sudo apt upgrade").as_deref(), Some("runs sudo"));
        assert_eq!(
            d("cd /tmp && /usr/bin/sudo rm x").as_deref(),
            Some("runs sudo")
        );
        assert_eq!(d("FOO=1 env su -").as_deref(), Some("runs su"));
        assert_eq!(d("echo $(doas id)").as_deref(), Some("runs doas"));
        assert_eq!(
            d("curl -fsSL https://x.sh | sh").as_deref(),
            Some("runs a download with sh")
        );
        assert_eq!(
            d("wget -qO- https://x | sudo bash").as_deref(),
            Some("pipes text into sudo")
        );
        assert_eq!(
            d("bash <(curl -s https://x)").as_deref(),
            Some("runs a download with bash")
        );
        assert_eq!(
            d("sh -c \"$(curl -fsSL https://x)\"").as_deref(),
            Some("runs a download with sh")
        );
        assert_eq!(d("curl -O https://x/file.tar.gz"), None);
        assert_eq!(
            d("cat notes | bash").as_deref(),
            Some("pipes text into bash")
        );
        assert_eq!(d("echo 'sudo rm -rf /'"), None);
        assert_eq!(d("grep sudo /var/log/auth.log"), None);
        assert!(check("sudo reboot", true, false).is_some());
        assert_eq!(check("sudo reboot\n", true, true), None);
        let r = check("sudo reboot\n", false, false).unwrap();
        assert!(r.runs_at_once);
        assert_eq!(r.sentence(), "It runs sudo and runs at once when pasted.");
    }

    #[test]
    fn the_preview_shows_control_characters_and_counts_the_lines() {
        let (lines, n) = preview("a\tb\nc\x1b[0m\nd\ne\n", 2);
        assert_eq!(lines, vec!["a    b".to_string(), "c^[[0m".to_string()]);
        assert_eq!(n, 4);
        assert_eq!(preview("one", 5), (vec!["one".to_string()], 1));
    }
}

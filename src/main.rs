//! NIRUTERM: the terminal of the NIRU suite.
mod app;
mod canvas;
mod folder;
mod font;
mod graphics;
mod icat;
mod image;
mod marks;
mod open;
mod palette;
mod paste;
mod term;
mod text;
mod x;

const HELP: &str = "NIRUTERM: the NIRU terminal

  niruterm [OPTIONS] [-e] [PROGRAM [ARGS...]]
    --working-directory, --directory, -d DIR   start in DIR
    --title, -T TITLE        the window title
    --class NAME             the window class (for window rules)
    --hold                   keep the window after the program ends
  kitty's options are understood, so `kitty` can be replaced by `niruterm`
  in a command; options NIRUTERM has no use for (-o, --single-instance) are
  ignored.
  niruterm icat [--width COLS] FILE...   show pictures in the terminal
  niruterm --version

Keys: Ctrl+Shift+C/V copy/paste · Shift+Insert paste the selection
      Ctrl+Shift+N new window in the same folder
      Ctrl+Shift+Plus/Minus/0 font size
      Shift+PgUp/PgDn/Home/End history · Ctrl+click open a link
      Ctrl+Shift+F search the history (Enter older, Shift+Enter newer, Esc)
      Ctrl+Shift+E hints: type a label to open a link or path:line
      Ctrl+Shift+Up/Down previous/next prompt · Ctrl+Shift+G copy the
      last command's output (prompt marks from niruprompt, OSC 133)
Mouse: select to copy · double-click a word · triple-click a line
       middle-click pastes · Shift overrides programs that use the mouse
Paste: a paste that would run at once or uses sudo, su or a download
       piped into a shell is shown first: Enter pastes, Esc cancels
Config: ~/.config/niruterm/config.toml (font, font_size, padding, shell,
        scrollback, cursor, cursor_blink, copy_on_select, notify_after,
        editor, paste_protection, ligatures, remember_folder, [colors]).
        Colours follow the NIRUX theme.";

fn main() {
    let started = std::time::Instant::now();
    let mut args = std::env::args().skip(1).peekable();
    let mut spawn = term::Spawn {
        shell_env: None,
        program: None,
        args: Vec::new(),
        dir: None,
        scrollback: 10_000,
    };
    let mut opts = app::Options::default();
    if args.peek().map(String::as_str) == Some("icat") {
        args.next();
        std::process::exit(icat::run(args.collect()));
    }
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" | "-V" => {
                println!("NIRUTERM {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!("{HELP}");
                return;
            }
            "-e" | "--" => {
                spawn.program = args.next();
                spawn.args = args.by_ref().collect();
            }
            "--working-directory" | "-d" | "--directory" => spawn.dir = args.next().map(Into::into),
            "--title" | "-T" => opts.title = args.next(),
            "--class" => opts.class = args.next(),
            "--name" => {
                args.next();
            }
            "--hold" => opts.hold = true,
            "-o" | "--override" | "--config" | "-c" | "--instance-group" => {
                args.next();
            }
            "--single-instance" | "-1" | "--detach" => {}
            other => {
                if let Some((key, value)) = other.split_once('=') {
                    match key {
                        "--working-directory" | "--directory" => spawn.dir = Some(value.into()),
                        "--title" => opts.title = Some(value.into()),
                        "--class" => opts.class = Some(value.into()),
                        "--name" | "--override" | "--config" | "--instance-group" => {}
                        _ => {
                            eprintln!("niruterm: unknown option {other} (see --help)");
                            std::process::exit(2);
                        }
                    }
                } else if other.starts_with('-') {
                    eprintln!("niruterm: unknown option {other} (see --help)");
                    std::process::exit(2);
                } else {
                    // kitty style: the first word that is not an option is
                    // the program, everything after it its arguments.
                    spawn.program = Some(other.to_string());
                    spawn.args = args.by_ref().collect();
                }
            }
        }
    }
    let settings = palette::settings();
    spawn.scrollback = settings.scrollback;
    if spawn.dir.is_none()
        && settings.remember_folder
        && opts.class.as_deref() == Some(folder::SCRATCHPAD)
    {
        // The scratchpad opens in the folder it had last.
        spawn.dir = folder::load(&folder::file());
    }
    if spawn.program.is_none() {
        spawn.program = settings.shell.clone();
        // The shell inside is the one NIRUTERM chose (kitty's fish), so
        // programs that ask $SHELL (nirufetch, vim's :shell) see that one
        // and not the login shell, as in kitty.
        spawn.shell_env = spawn.program.as_deref().and_then(shell_path);
    }
    match app::App::new(spawn, opts, started).and_then(|mut a| a.run()) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("niruterm: {e}");
            std::process::exit(1);
        }
    }
}

/// The full path of a shell given by name or path.
fn shell_path(name: &str) -> Option<String> {
    if name.contains('/') {
        return std::path::Path::new(name)
            .is_file()
            .then(|| name.to_string());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

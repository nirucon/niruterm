//! NIRUTERM: the terminal of the NIRU suite.
mod app;
mod canvas;
mod font;
mod graphics;
mod icat;
mod image;
mod palette;
mod term;
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
      Ctrl+Shift+N new window here · Ctrl+Shift+Plus/Minus/0 font size
      Shift+PgUp/PgDn/Home/End history · Ctrl+click open a link
Mouse: select to copy · double-click a word · triple-click a line
       middle-click pastes · Shift overrides programs that use the mouse
Config: ~/.config/niruterm/config.toml (font, font_size, padding, shell,
        scrollback, cursor, cursor_blink, copy_on_select, [colors]).
        Colours follow the NIRUX theme.";

fn main() {
    let started = std::time::Instant::now();
    let mut args = std::env::args().skip(1).peekable();
    let mut spawn = term::Spawn {
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
    if spawn.program.is_none() {
        spawn.program = settings.shell.clone();
    }
    match app::App::new(spawn, opts, started).and_then(|mut a| a.run()) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("niruterm: {e}");
            std::process::exit(1);
        }
    }
}

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

  niruterm [-e PROGRAM [ARGS...]] [--working-directory DIR] [--title TITLE]
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
    let mut title = None;
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
            "--title" | "-T" => title = args.next(),
            "--class" | "--name" => {
                args.next();
            }
            "--hold" | "--single-instance" | "-1" => {}
            other if other.starts_with("--working-directory=") => {
                spawn.dir = Some(other["--working-directory=".len()..].into());
            }
            other => {
                eprintln!("niruterm: unknown option {other} (see --help)");
                std::process::exit(2);
            }
        }
    }
    let settings = palette::settings();
    spawn.scrollback = settings.scrollback;
    if spawn.program.is_none() {
        spawn.program = settings.shell.clone();
    }
    match app::App::new(spawn, title, started).and_then(|mut a| a.run()) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("niruterm: {e}");
            std::process::exit(1);
        }
    }
}

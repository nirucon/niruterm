# NIRUTERM

The terminal of the NIRU suite: fast, light, with pictures and the NIRUX theme.

**Status: beta** · Part of the [NIRUX](https://github.com/nirucon/nirux) suite · [MIT License](LICENSE)

## About

NIRUTERM opens in a few hundredths of a second and uses 12 to 25 MB of
memory. It is built on `alacritty_terminal`, the well-tested terminal engine
of Alacritty, so programs such as vim, htop, tmux, less, OpenCode and Claude
Code behave as they do in any good terminal. Everything on screen is drawn by
NIRUTERM itself on X11, like the other NIRU programs: the colours are the
NIRUX theme (the palette NIRUX writes for kitty), and it changes with the
theme live.

| | NIRUTERM | kitty |
|---|---|---|
| Window shown | about 40 ms | Python and OpenGL start first |
| 2,000,000 lines of output | 763 ms | 1,432 ms |
| Memory | 12 to 25 MB | more than 100 MB |
| Idle CPU | none | none |

Measured on the same machine in the same X server (Debian 13).

## Features

- **Pictures** through the kitty graphics protocol, the one `icat`, yazi,
  ranger, timg and chafa use: `niruterm icat photo.jpg` (PNG, JPEG, WebP, GIF,
  SVG, the first page of a PDF, a frame of a video).
- **Exact box drawing:** lines, blocks, shades and powerline arrows are drawn
  as pixels, so frames join without gaps at every size.
- **Text:** bold, italic, every underline style (curly too), strike, dim,
  wide characters (CJK), combining marks, a fallback font for any missing
  character, true colour and the 256 colours.
- **Keyboard:** the xterm encoding programs expect, and the Swedish dead keys
  (`~` `^` `´` `` ` `` `¨`) work as on the desktop.
- **Mouse:** select to copy, double-click a word, triple-click a line,
  Shift+click to extend, middle-click to paste, the wheel for the history;
  programs that use the mouse (vim, htop) get it, and Shift overrides them.
- **Clipboard** shared with every program (CLIPBOARD and the selection), safe
  bracketed paste, OSC 52 from programs over SSH.
- **Links:** Ctrl+click opens a URL or an OSC 8 link.
- **Drop files** on the window to type their paths, quoted for the shell.
- **History** of 10,000 lines (configurable), with its position shown while
  scrolled back.

## Using it

```text
niruterm [-e PROGRAM ARGS...] [--working-directory DIR] [--title TITLE]
niruterm icat [--width COLS] FILE...
```

| Keys | |
|---|---|
| Ctrl+Shift+C, Ctrl+Shift+V | copy, paste |
| Shift+Insert, middle-click | paste the selection |
| Ctrl+Shift+N | new window in the same folder |
| Ctrl+Shift+Plus, Minus, 0 | font size |
| Shift+PgUp, PgDn, Home, End | history |
| Ctrl+click | open a link |

## Configuration

`~/.config/niruterm/config.toml` (every key is optional):

```toml
font = "JetBrainsMono Nerd Font"   # a fontconfig family
font_size = 12.0                    # points; kitty's size when unset
padding = 10
line_height = 1.0
shell = "fish"                      # the login shell when unset
scrollback = 10000
cursor = "beam"                     # beam, block or underline
cursor_blink = false
copy_on_select = true

[colors]                            # on top of the NIRUX theme
background = "#101010"
color4 = "#88aacc"
```

## NIRUPROMPT: prompt, aliases and history

`niruprompt` comes with NIRUTERM and works in any terminal, for bash and fish:

```text
niruprompt install bash|fish        load it in new shells
niruprompt style [NAME]             nirux, runes, ascii, powerline, minimal, c64
niruprompt styles                   preview every style
niruprompt alias list|add NAME COMMAND|rm NAME
```

- **Prompt styles** in the theme's own colours: the folder, the git branch
  (and `*` for changes, with a 150 ms limit so big repositories never slow it
  down), the time of the last long command, background jobs, the Python
  virtual environment, and `user@host` over SSH or as root.
- **Aliases** from one file, `~/.config/nirux/aliases.toml`: aliases in
  bash, abbreviations in fish (they expand as you type).
- **bash** gets a large history shared between terminals without
  duplicates, prefix search on the arrow keys, case-insensitive completion,
  and Ctrl+R through fzf when it is installed. fish keeps its own
  autosuggestions and history.

## Building

```sh
cargo build --release
sh tests/xvfb.sh target/release/niruterm   # end-to-end test in a private Xvfb
```

Needs Rust 1.82 or later. At run time: an X server and fontconfig
(`fc-match`); for `icat` of formats other than PNG, the system thumbnailers
(gdk-pixbuf, ffmpegthumbnailer) and poppler-utils for PDF. NIRUX installs
them.

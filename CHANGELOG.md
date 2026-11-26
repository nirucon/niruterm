# Changelog

## 0.3.0

- **Paste protection.** A paste that would run at once (a line break while
  the program has no bracketed paste) or that uses `sudo`, `su`, `doas`,
  `pkexec`, `run0` or pipes text or a download into a shell (`curl ... | sh`,
  `bash <(curl ...)`) waits in a box in the middle of the window, with its
  first six lines: Enter pastes it, Esc (or Ctrl+C) cancels. Ordinary single
  lines are pasted at once; with bracketed paste only the dangerous kinds
  ask, and full-screen programs with bracketed paste (editors) never ask.
  Middle-click, Shift+Insert and dropped files are checked the same way.
  `paste_protection = false` turns it off.
- **New window in the same folder** (Ctrl+Shift+N) now uses the folder the
  shell reports with OSC 7 while the shell is in the foreground, else the
  folder of the program in the foreground, else the shell's. Reports from
  another machine (a shell over SSH) are ignored.
- **NIRUPROMPT** sends OSC 7 with the prompt marks, so NIRUTERM, kitty, foot
  and WezTerm know the shell's folder.
- **The scratchpad remembers its folder.** A window of class `scratchpad`
  (the NIRUWM scratchpad) keeps the folder of its shell in
  `$XDG_STATE_HOME/niruterm/scratchpad-folder` (when the shell reports a new
  one, when the scratchpad is hidden, and on exit) and opens there next time
  unless `--working-directory` is given. Only the path is stored.
  `remember_folder = false` turns it off.
- **Ligatures** for programming fonts, off by default (`ligatures = true`).
  Runs of ASCII symbols in one colour and style are shaped with rustybuzz
  (the font's `calt` and `liga` rules) and drawn on their own cells, so the
  grid never moves; letters are never joined.
- The end-to-end test takes its screenshots with ImageMagick or, when it is
  missing, ffmpeg, and checks the new features.

## 0.2.1

- Commands in fish 4 are named from its C mark (`cmdline_url`), so
  notifications and the copied output know the command line.

## 0.2.0

- History search, shell integration (OSC 133), notifications, keyboard
  hints, urgent bell.

## 0.1.5

- A new picture under a reused id replaces the old one (NPLAY album covers).

## 0.1.4

- SHELL is the shell NIRUTERM runs.

## 0.1.3

- kitty's options (`--class`, `--hold`, a program after the options).

## 0.1.2

- kitty's shell is used when none is set.

## 0.1.1

- The keyboard map is read again when it changes (layout switch, xdotool).

## 0.1.0

- The first release: a terminal on alacritty_terminal with its own
  rendering, the NIRUX palette, the kitty graphics protocol, `icat` and
  NIRUPROMPT.

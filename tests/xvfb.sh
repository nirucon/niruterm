#!/bin/sh
# End-to-end checks in a private Xvfb: first frame, typing through the PTY,
# a picture through the kitty graphics protocol (icat), resizing and exit.
# Usage: tests/xvfb.sh PATH/TO/niruterm
set -eu
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SB=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null || true; done; rm -rf "$SB"; }
trap cleanup EXIT INT TERM
export HOME=$SB/home XDG_CONFIG_HOME=$SB/config
mkdir -p "$HOME" "$XDG_CONFIG_HOME"
D=:$((40 + $$ % 20))
Xvfb "$D" -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 &
PIDS="$PIDS $!"
export DISPLAY=$D
sleep 1
fail() { echo "FAIL: $*" >&2; exit 1; }
# A 64x64 red PNG, written without any image tool.
python3 - "$SB/red.png" <<'PY'
import struct, sys, zlib
w = h = 64
raw = b"".join(b"\x00" + b"\xff\x00\x00\xff" * w for _ in range(h))
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
PY
NIRUTERM_TIMING=1 "$BIN" -e bash --norc 2>"$SB/err" &
T=$!
PIDS="$PIDS $T"
sleep 1.2
grep -q 'first frame after' "$SB/err" || fail "no first frame: $(cat "$SB/err")"
rss=$(awk '/VmRSS/ {print $2}' /proc/$T/status)
[ "$rss" -lt 80000 ] || fail "memory $rss kB"
echo "$(sed -n 's/.*first frame/first frame/p' "$SB/err"), ${rss} kB"
W=$(xdotool search --sync --onlyvisible --class niruterm | head -1)
xdotool windowmove "$W" 0 0
xdotool mousemove 600 400
k() { xdotool type --delay 15 "$1"; xdotool key Return; sleep 0.6; }
k "printf pass > $SB/typed"
[ "$(cat "$SB/typed" 2>/dev/null)" = pass ] || fail 'typing did not reach the shell'
# The picture: drawn at the top left after clear.
k "clear; $BIN icat $SB/red.png"
sleep 0.6
import -window "$W" -crop 1x1+20+20 txt:- > "$SB/px" 2>/dev/null || import -window root -crop 1x1+20+20 txt:- > "$SB/px"
grep -qi 'FF0000\|255,0,0' "$SB/px" || fail "no picture at the top left: $(cat "$SB/px")"
# Resizing reaches the program as a new size.
k "stty size > $SB/size1"
xdotool windowsize "$W" 700 400
xdotool mousemove 300 200
sleep 0.6
k "stty size > $SB/size2"
[ -s "$SB/size2" ] && [ "$(cat "$SB/size1")" != "$(cat "$SB/size2")" ] || fail "the size did not change: $(cat "$SB/size1")"
# exit ends the window.
k "exit"
sleep 0.8
if kill -0 "$T" 2>/dev/null; then fail 'the window stayed after exit'; fi
echo "typing, picture, resize and exit ok"
# kitty's options: --class names the window, --hold keeps it after the
# program ends until a key, and the program may follow the options directly.
"$BIN" --class probe-niru --hold -o remember_window_size=no sh -c 'printf held > '"$SB"'/held' 2>>"$SB/err" &
H=$!
PIDS="$PIDS $H"
sleep 1.2
HW=$(xdotool search --class probe-niru | head -1)
[ -n "$HW" ] || fail 'no window with the --class name'
xprop -id "$HW" WM_CLASS | grep -q '"probe-niru", "probe-niru"' || fail "WM_CLASS: $(xprop -id "$HW" WM_CLASS)"
[ "$(cat "$SB/held" 2>/dev/null)" = held ] || fail 'the program after the options did not run'
kill -0 "$H" 2>/dev/null || fail '--hold closed the window when the program ended'
xdotool windowmove "$HW" 0 0
xdotool mousemove 100 100
xdotool key Return
sleep 0.6
if kill -0 "$H" 2>/dev/null; then fail 'a key did not close the held window'; fi
echo "class, hold and kitty-style command ok"
# The chosen shell is also SHELL inside (nirufetch and others read it).
mkdir -p "$XDG_CONFIG_HOME/niruterm"
printf 'shell = "sh"\n' > "$XDG_CONFIG_HOME/niruterm/config.toml"
SHELL=/bin/false "$BIN" 2>>"$SB/err" &
S=$!
PIDS="$PIDS $S"
sleep 1.2
xdotool mousemove 300 300
k 'printf %s "$SHELL" > '"$SB"'/shellvar'
case "$(cat "$SB/shellvar" 2>/dev/null)" in */sh) ;; *) fail "SHELL inside is '$(cat "$SB/shellvar" 2>/dev/null)'" ;; esac
k exit
echo "chosen shell is SHELL ok"
echo PASS

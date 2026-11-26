#!/bin/sh
# End-to-end checks in a private Xvfb: first frame, typing through the PTY,
# a picture through the kitty graphics protocol (icat), resizing and exit;
# shell integration with NIRUPROMPT (prompt jumps, the last output), the
# history search, keyboard hints, notifications and the urgency flag; the
# paste confirmation, a new window in the same folder and the scratchpad's
# folder across restarts.
# Everything runs with HOME and the XDG folders in a temporary folder.
# Usage: tests/xvfb.sh PATH/TO/niruterm (niruprompt next to it)
set -eu
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SB=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null || true; done; rm -rf "$SB"; }
trap cleanup EXIT INT TERM
export HOME=$SB/home XDG_CONFIG_HOME=$SB/config XDG_CACHE_HOME=$SB/cache \
    XDG_STATE_HOME=$SB/state XDG_RUNTIME_DIR=$SB/run
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
# A free display: other tests may run their own Xvfb at the same time.
n=$((40 + $$ % 20))
while [ -e "/tmp/.X$n-lock" ] || [ -e "/tmp/.X11-unix/X$n" ]; do n=$((n + 1)); done
D=:$n
Xvfb "$D" -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 &
PIDS="$PIDS $!"
export DISPLAY=$D
sleep 1
fail() { echo "FAIL: $*" >&2; exit 1; }
# The root window (1280x800) as raw RGB, with ImageMagick or else ffmpeg.
grab() {
    if command -v import >/dev/null 2>&1; then
        import -window root -depth 8 "rgb:$1"
    else
        ffmpeg -loglevel error -y -f x11grab -video_size 1280x800 -i "$DISPLAY" \
            -frames:v 1 -f rawvideo -pix_fmt rgb24 "$1"
    fi
}
# The colour of one pixel of the screen, as rrggbb.
pixel() {
    grab "$SB/px.rgb"
    python3 -c 'import sys; d = open(sys.argv[1], "rb").read(); i = (int(sys.argv[3]) * 1280 + int(sys.argv[2])) * 3; print(d[i:i + 3].hex())' "$SB/px.rgb" "$1" "$2"
}
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
# The same picture in blue, and a sender that works like NPLAY's album
# cover: delete image 31, then send the new picture under the same id and
# the same cell size.
python3 - "$SB/blue.png" <<'PY'
import struct, sys, zlib
w = h = 64
raw = b"".join(b"\x00" + b"\x00\x00\xff\xff" * w for _ in range(h))
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
PY
cat > "$SB/cover.py" <<'PY'
import base64, sys
data = base64.b64encode(open(sys.argv[1], "rb").read()).decode()
out = "\x1b[H\x1b_Ga=d,d=I,i=31,q=2\x1b\\"
out += "\x1b_Ga=T,f=100,t=d,i=31,q=2,C=1,c=8,r=4;" + data + "\x1b\\"
sys.stdout.write(out)
sys.stdout.flush()
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
px=$(pixel 20 20)
[ "$px" = ff0000 ] || fail "no picture at the top left: $px"
# A new picture under the same id and size replaces the old one (NPLAY
# showed the previous album cover after a track change).
k "clear; python3 $SB/cover.py $SB/red.png; sleep 0.3; python3 $SB/cover.py $SB/blue.png"
sleep 0.8
px=$(pixel 20 20)
[ "$px" = 0000ff ] || fail "the old picture stayed under a reused id: $px"
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
# ---------------------------------------------------------------- 0.2.0
# Shell integration: bash with NIRUPROMPT sends the OSC 133 marks. Programs
# NIRUTERM starts (notify-send, xdg-open, the editor) are stand-ins that
# write down how they were called.
NP=$(dirname "$BIN")/niruprompt
[ -x "$NP" ] || fail "no niruprompt next to $BIN"
mkdir -p "$SB/bin"
for p in notify-send:notified xdg-open:opened fake-editor:edited; do
    printf '#!/bin/sh\nprintf "%%s\\n" "$@" >> %s/%s\n' "$SB" "${p#*:}" > "$SB/bin/${p%%:*}"
done
chmod +x "$SB"/bin/*
printf 'notify_after = 1\neditor = "fake-editor"\n' > "$XDG_CONFIG_HOME/niruterm/config.toml"
"$NP" init bash > "$SB/niru.bash"
printf 'one\ntwo\nthree\n' > "$SB/target.txt"
# A solid run of a colour (24x2 pixels, wider than any glyph stroke) in a
# screenshot of the root window (1280x800).
cat > "$SB/block.py" <<'PY'
import sys
data, colour, w, h = open(sys.argv[1], "rb").read(), bytes.fromhex(sys.argv[2]), 24, 2
row, pat = 1280 * 3, colour * w
for y in range(800 - h):
    line = data[y * row:(y + 1) * row]
    i = line.find(pat)
    while i != -1:
        if i % 3 == 0 and all(data[(y + k) * row + i:(y + k) * row + i + len(pat)] == pat for k in range(1, h)):
            sys.exit(0)
        i = line.find(pat, i + 1)
sys.exit(1)
PY
block() { grab "$SB/shot.rgb" && python3 "$SB/block.py" "$SB/shot.rgb" "$1"; }
clip() { timeout 3 xclip -o -selection clipboard 2>/dev/null || true; }
PATH="$SB/bin:$PATH" NIRUPROMPT_STYLE=ascii "$BIN" --class probe-int --working-directory "$HOME" \
    -e bash --rcfile "$SB/niru.bash" -i 2>>"$SB/err" &
I=$!
PIDS="$PIDS $I"
"$BIN" --class probe-other -e sleep 60 2>>"$SB/err" &
PIDS="$PIDS $!"
sleep 2
IW=$(xdotool search --sync --onlyvisible --class probe-int | head -1)
OW=$(xdotool search --sync --onlyvisible --class probe-other | head -1)
xdotool windowmove "$OW" 600 400
xdotool windowmove "$IW" 0 0
xdotool windowfocus --sync "$IW"
# Ctrl+Shift+G copies the output of the last command.
k "seq 1 5"
xdotool key ctrl+shift+g
sleep 0.3
[ "$(clip)" = "$(seq 1 5)" ] || fail "the last output was not copied: '$(clip)'"
# Ctrl+Shift+Up shows the prompt above the view at the top: a triple-click
# on the first row copies the prompt line with its command.
k "seq 1 300"
xdotool key ctrl+shift+Up
sleep 0.4
xdotool mousemove 40 16 click --repeat 3 --delay 60 1
sleep 0.3
case "$(clip)" in *'$ seq 1 300'*) ;; *) fail "Ctrl+Shift+Up did not reach the prompt: '$(clip)'" ;; esac
xdotool key ctrl+shift+Down
echo "shell integration (last output, prompt jumps) ok"
# Ctrl+Shift+F finds text in the history and shows the matches: the current
# one in the foreground colour, the others in yellow (colour 3); Esc closes.
k "clear; for i in 1 2 3; do echo needle-\$i; done; seq 1 300"
if block e5e5e5 || block b5a47e; then fail 'highlight colours before the search'; fi
xdotool key ctrl+shift+f
xdotool type --delay 15 needle
sleep 0.5
block e5e5e5 || fail 'no current match in view'
block b5a47e || fail 'no other matches in view'
xdotool key Return Return
sleep 0.3
block e5e5e5 || fail 'Enter lost the current match'
xdotool key Escape
sleep 0.4
if block b5a47e; then fail 'matches stayed after Esc'; fi
echo "history search ok"
# Ctrl+Shift+E labels links and paths from the bottom up: a opens the path
# at its line in the editor (in a new window), s the link with xdg-open,
# capital S types the link, Esc goes back to the shell.
k "f=$SB/target.txt"
k "clear; echo visit https://example.com/x; echo at \$f:3 here"
xdotool key ctrl+shift+e
sleep 0.3
xdotool key a
sleep 2
[ "$(cat "$SB/edited" 2>/dev/null)" = "$(printf '+3\n%s' "$SB/target.txt")" ] || fail "editor: '$(cat "$SB/edited" 2>/dev/null)'"
xdotool windowfocus --sync "$IW"
xdotool key ctrl+shift+e
sleep 0.3
xdotool key s
sleep 0.8
[ "$(cat "$SB/opened" 2>/dev/null)" = https://example.com/x ] || fail "xdg-open: '$(cat "$SB/opened" 2>/dev/null)'"
xdotool type --delay 15 'printf %s '
xdotool key ctrl+shift+e
sleep 0.3
xdotool key S
xdotool type --delay 15 " > $SB/typed-link"
xdotool key Return
sleep 0.6
[ "$(cat "$SB/typed-link" 2>/dev/null)" = https://example.com/x ] || fail "capital label: '$(cat "$SB/typed-link" 2>/dev/null)'"
xdotool key ctrl+shift+e
sleep 0.3
xdotool key Escape
k "printf back > $SB/after-hints"
[ "$(cat "$SB/after-hints" 2>/dev/null)" = back ] || fail 'keys did not reach the shell after Esc'
echo "keyboard hints ok"
# A command longer than notify_after (1 s) that ends while the window is
# not focused is notified; a bell then sets the urgency flag until focus.
k "sleep 1.3"
sleep 1.2
[ ! -s "$SB/notified" ] || fail "notified while focused: $(cat "$SB/notified")"
xdotool type --delay 15 "sleep 1.3; printf '\\a'"
xdotool key Return
xdotool windowfocus --sync "$OW"
sleep 2.3
grep -q '^Command finished$' "$SB/notified" || fail "no notification: $(cat "$SB/notified" 2>/dev/null)"
grep -q "sleep 1.3; printf" "$SB/notified" || fail "the command is not in the notification: $(cat "$SB/notified")"
xprop -id "$IW" WM_HINTS | grep -qi urgen || fail "no urgency after a bell: $(xprop -id "$IW" WM_HINTS)"
xdotool windowfocus --sync "$IW"
sleep 0.4
if xprop -id "$IW" WM_HINTS | grep -qi urgen; then fail 'the urgency stayed after focus'; fi
: > "$SB/notified"
xdotool type --delay 15 "sleep 1.3; false"
xdotool key Return
xdotool windowfocus --sync "$OW"
sleep 2.3
grep -q '^Command failed (exit 1)$' "$SB/notified" || fail "no failure notification: $(cat "$SB/notified" 2>/dev/null)"
xdotool windowfocus --sync "$IW"
k exit
sleep 0.6
if kill -0 "$I" 2>/dev/null; then fail 'the window stayed after exit'; fi
echo "notifications and urgency ok"
# ---------------------------------------------------------------- 0.3.0
# Paste protection. sh has no bracketed paste: a paste with a line break
# waits in a box (yellow, colour 3) until Enter, Esc drops it, a single line
# goes straight through. bash has bracketed paste: several lines go straight
# through, sudo waits in a red box (colour 1).
printf '' > "$XDG_CONFIG_HOME/niruterm/config.toml"
"$BIN" --class probe-paste -e sh 2>>"$SB/err" &
P=$!
PIDS="$PIDS $P"
sleep 1.2
PW=$(xdotool search --sync --onlyvisible --class probe-paste | head -1)
xdotool windowmove "$PW" 0 0
xdotool windowfocus --sync "$PW"
setclip() { printf "$1" | xclip -selection "${2:-clipboard}" -i; sleep 0.3; }
setclip "printf one > $SB/p1\nprintf two > $SB/p2\n"
xdotool key ctrl+shift+v
sleep 0.5
[ ! -e "$SB/p1" ] || fail 'a paste with a line break ran without asking'
block b5a47e || fail 'no paste confirmation'
xdotool key Escape
sleep 0.5
if block b5a47e; then fail 'the confirmation stayed after Esc'; fi
[ ! -e "$SB/p1" ] || fail 'Esc pasted anyway'
xdotool key ctrl+shift+v
sleep 0.4
xdotool key Return
sleep 0.6
[ "$(cat "$SB/p1" "$SB/p2" 2>/dev/null)" = onetwo ] || fail 'Enter did not paste both lines'
setclip "printf three > $SB/p3"
xdotool key ctrl+shift+v
sleep 0.4
if block b5a47e; then fail 'a single line asked for confirmation'; fi
xdotool key Return
sleep 0.5
[ "$(cat "$SB/p3" 2>/dev/null)" = three ] || fail 'a single line was not pasted'
# Middle-click pastes the selection through the same check.
setclip "printf four > $SB/p4\n" primary
xdotool mousemove 300 300 click 2
sleep 0.5
block b5a47e || fail 'a middle-click paste with a line break did not ask'
xdotool key Return
sleep 0.5
[ "$(cat "$SB/p4" 2>/dev/null)" = four ] || fail 'the middle-click paste was lost'
xdotool type --delay 15 "exec bash --rcfile $SB/niru.bash -i"
xdotool key Return
sleep 0.8
setclip "printf five > $SB/p5\nprintf six > $SB/p6"
xdotool key ctrl+shift+v
sleep 0.4
if block b5a47e || block b87878; then fail 'bracketed paste asked about plain lines'; fi
xdotool key Return
sleep 0.5
[ "$(cat "$SB/p5" "$SB/p6" 2>/dev/null)" = fivesix ] || fail 'the bracketed paste did not run'
setclip "sudo -n true; printf seven > $SB/p7"
xdotool key ctrl+shift+v
sleep 0.4
block b87878 || fail 'sudo did not ask'
xdotool key Escape
sleep 0.3
xdotool key Return
sleep 0.4
[ ! -e "$SB/p7" ] || fail 'the sudo paste ran after Esc'
echo "paste protection ok"
# Ctrl+Shift+N opens a new window in the folder the shell is in (here
# reported by NIRUPROMPT with OSC 7, the folder has a space).
mkdir -p "$SB/my dir" "$SB/scratch dir"
xdotool type --delay 15 "cd '$SB/my dir'"
xdotool key Return
sleep 0.3
xdotool key ctrl+shift+n
sleep 1.5
NW=$(xdotool search --onlyvisible --class '^niruterm$' | head -1)
[ -n "$NW" ] || fail 'Ctrl+Shift+N opened no window'
xdotool windowmove "$NW" 0 0
xdotool windowraise "$NW"
xdotool windowfocus --sync "$NW"
k "pwd > '$SB/new-pwd'; exit"
[ "$(cat "$SB/new-pwd" 2>/dev/null)" = "$SB/my dir" ] || fail "the new window opened in '$(cat "$SB/new-pwd" 2>/dev/null)'"
xdotool windowfocus --sync "$PW"
k exit
echo "new window in the same folder ok"
# The scratchpad (class scratchpad) keeps the folder the shell reports and
# opens there after a restart; other windows keep nothing.
STATE=$XDG_STATE_HOME/niruterm/scratchpad-folder
[ ! -e "$STATE" ] || fail 'a window that is not the scratchpad kept its folder'
"$BIN" --class scratchpad -e bash --rcfile "$SB/niru.bash" -i 2>>"$SB/err" &
PIDS="$PIDS $!"
sleep 1.5
SW=$(xdotool search --sync --onlyvisible --class scratchpad | head -1)
xdotool windowmove "$SW" 0 0
xdotool windowfocus --sync "$SW"
k "cd '$SB/scratch dir'"
[ "$(cat "$STATE" 2>/dev/null)" = "$SB/scratch dir" ] || fail "kept folder: '$(cat "$STATE" 2>/dev/null)'"
k exit
"$BIN" --class scratchpad -e bash --norc 2>>"$SB/err" &
PIDS="$PIDS $!"
sleep 1.2
SW=$(xdotool search --sync --onlyvisible --class scratchpad | head -1)
xdotool windowmove "$SW" 0 0
xdotool windowfocus --sync "$SW"
k "pwd > '$SB/scratch-pwd'; exit"
[ "$(cat "$SB/scratch-pwd" 2>/dev/null)" = "$SB/scratch dir" ] || fail "the scratchpad opened in '$(cat "$SB/scratch-pwd" 2>/dev/null)'"
echo "scratchpad folder ok"
echo PASS

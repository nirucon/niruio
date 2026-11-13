#!/bin/sh
# Integration test against a private PipeWire and WirePlumber that only has
# null sinks (no sound card, no Bluetooth, no camera, its own state). It
# remembers a muted role the way it happens in real life, lets the doctor
# find it, and resets it through WirePlumber.
set -eu
BIN=$(CDPATH='' cd -- "$(dirname "${1:-target/release/niruio}")" && pwd)/$(basename "${1:-target/release/niruio}")
HERE=$(CDPATH='' cd -- "$(dirname "$0")" && pwd)
for tool in pipewire wireplumber pw-cli pw-cat pw-loopback pw-dump wpctl; do
  command -v "$tool" >/dev/null 2>&1 || { echo "sandbox: $tool missing, skipped"; exit 0; }
done
DIR=$(mktemp -d)
cleanup() { sh "$HERE/pw-sandbox.sh" stop "$DIR" || :; rm -rf "$DIR"; }
trap cleanup EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
set -- start "$DIR"
. "$HERE/pw-sandbox.sh"
set +e
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=test-speakers node.description="Test Speakers" media.class=Audio/Sink audio.channels=2 audio.position=[FL,FR] object.linger=true }' >/dev/null
sleep 1
# Ten seconds of silence as a WAV file (48 kHz, stereo, 16 bit).
{ printf 'RIFF\044\246\016\000WAVEfmt \020\000\000\000\001\000\002\000\200\273\000\000\000\356\002\000\004\000\020\000data\000\246\016\000'; head -c 960000 /dev/zero; } > "$DIR/silence.wav"
pw-cat -p --media-role Movie --target test-speakers -P '{ node.name=mpv-test application.name=mpv }' "$DIR/silence.wav" &
cat_pid=$!
i=0; id=
while [ -z "$id" ] && [ $i -lt 30 ]; do id=$(wpctl status | awk '/mpv/ && /\./ {sub(/\..*/, "", $1); print $1; exit}'); i=$((i+1)); sleep 0.1; done
[ -n "$id" ] || fail "the test stream did not appear: $(wpctl status)"
wpctl set-mute "$id" 1
sleep 2
"$BIN" doctor --brief > "$DIR/doctor.txt"; rc=$?
cat "$DIR/doctor.txt"
[ "$rc" = 1 ] || fail "doctor exit $rc with a muted stream"
grep -q 'mpv plays muted' "$DIR/doctor.txt" || fail "doctor did not see the muted stream"
kill "$cat_pid" 2>/dev/null; wait "$cat_pid" 2>/dev/null
sleep 1
"$BIN" saved | tee "$DIR/saved.txt"
grep -q 'media.role:Movie  <- silences' "$DIR/saved.txt" || fail "the muted role is not reported"
"$BIN" reset --silencing || fail "reset failed"
"$BIN" saved > "$DIR/saved2.txt"
grep -q 'media.role:Movie' "$DIR/saved2.txt" || fail "the role disappeared"
grep -q 'silences' "$DIR/saved2.txt" && fail "still silencing after reset: $(cat "$DIR/saved2.txt")"
pw-dump | grep -q niruio-reset && fail "reset left nodes behind"
"$BIN" status --json > "$DIR/status.json" || fail "status --json"
grep -q '"Test Speakers"' "$DIR/status.json" || fail "status does not list the sink"
echo "sandbox ok"

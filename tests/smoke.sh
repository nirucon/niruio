#!/bin/sh
# Headless smoke test of a built niruio: the command line against a fixture
# graph, no PipeWire, no terminal interface.
set -eu
BIN=${1:-target/release/niruio}
HERE=$(CDPATH='' cd -- "$(dirname "$0")" && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
export NIRUIO_PW_DUMP="$HERE/fixtures/laptop.json"
export XDG_STATE_HOME="$TMP/state" XDG_CONFIG_HOME="$TMP/config" NIRUIO_ROOT="$TMP/root"
mkdir -p "$XDG_STATE_HOME/wireplumber" "$NIRUIO_ROOT/sys/class/video4linux" "$NIRUIO_ROOT/proc"
cat > "$XDG_STATE_HOME/wireplumber/stream-properties" <<'STATE'
[stream-properties]
Output/Audio:media.role:Movie={"channelVolumes":[1.0, 1.0], "channelMap":["FL", "FR"], "volume":1.0, "mute":true}
Output/Audio:media.role:Music={"channelVolumes":[0.0, 0.0], "channelMap":["FL", "FR"], "volume":1.0, "mute":false}
Output/Audio:application.name:Firefox={"channelVolumes":[0.5, 0.5], "channelMap":["FL", "FR"], "volume":1.0, "mute":false}
STATE

version=$(grep -m1 '^version' "$HERE/../Cargo.toml" | cut -d'"' -f2)
[ "$("$BIN" --version)" = "niruio $version" ] || fail "--version"
"$BIN" --help | grep -q 'niruio doctor' || fail "--help"

out=$("$BIN" status)
printf '%s\n' "$out" | grep -q '^Output  Family 17h/19h HD Audio Controller Speaker  40%' || fail "status output: $out"
printf '%s\n' "$out" | grep -q '^App     mpv plays on .*muted' || fail "status app: $out"

"$BIN" status --json > "$TMP/status.json"
grep -q '"default_output"' "$TMP/status.json" || fail "status --json"
grep -q '"Output/Audio:media.role:Movie"' "$TMP/status.json" || fail "status --json saved"

set +e
"$BIN" doctor > "$TMP/doctor.txt"; rc=$?
set -e
[ "$rc" = 1 ] || fail "doctor exit $rc, expected 1 (problems)"
grep -q 'mpv plays muted' "$TMP/doctor.txt" || fail "doctor mpv"
grep -q 'Role Movie' "$TMP/doctor.txt" || fail "doctor role"
grep -q 'Remembered: Role Music' "$TMP/doctor.txt" || fail "doctor saved music"
set +e
brief=$("$BIN" doctor --brief)
set -e
[ "$(printf '%s\n' "$brief" | grep -c '^problem: ')" -ge 1 ] || fail "doctor --brief: $brief"
set +e
"$BIN" doctor --json > "$TMP/doctor.json"
set -e
grep -q '"kind": "reset-saved"' "$TMP/doctor.json" || fail "doctor --json fix"

"$BIN" saved | grep -q 'Output/Audio:media.role:Movie  <- silences' || fail "saved"
"$BIN" saved --json | grep -q '"silencing": true' || fail "saved --json"

set +e
"$BIN" nonsense >/dev/null 2>&1; rc=$?
"$BIN" --window nonsense >/dev/null 2>&1; rc2=$?
set -e
[ "$rc" = 2 ] || fail "unknown command exit $rc"
[ "$rc2" = 2 ] || fail "unknown window tab exit $rc2"

# --fix in dry-run records the commands instead of running them.
export NIRUIO_DRY_RUN="$TMP/commands"
set +e
"$BIN" doctor --fix > "$TMP/fix.txt"
set -e
grep -q '^niruio-reset Output/Audio:media.role:Movie$' "$TMP/commands" || fail "fix commands: $(cat "$TMP/commands")"
grep -q '^niruio-reset Output/Audio:media.role:Music$' "$TMP/commands" || fail "fix music"
"$BIN" reset --silencing > /dev/null
[ "$(grep -c '^niruio-reset' "$TMP/commands")" -ge 4 ] || fail "reset --silencing"
echo "smoke ok"

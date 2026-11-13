#!/bin/sh
# A private PipeWire + WirePlumber with no hardware: only null sinks that the
# tests create. Nothing reaches a sound card, and the real session's state is
# never touched.
#   set -- start DIR; . tests/pw-sandbox.sh   exports XDG_RUNTIME_DIR etc. for DIR
#   sh tests/pw-sandbox.sh stop DIR
set -eu
cmd=${1:?start|stop}
dir=${2:?directory}
case "$cmd" in
start)
  mkdir -p "$dir/run" "$dir/state" "$dir/config/wireplumber/wireplumber.conf.d" "$dir/config/pipewire"
  chmod 700 "$dir/run"
  cat > "$dir/config/wireplumber/wireplumber.conf.d/90-niruio-sandbox.conf" <<'CONF'
wireplumber.profiles = {
  main = {
    monitor.alsa = disabled
    monitor.alsa-midi = disabled
    monitor.bluez = disabled
    monitor.bluez.midi = disabled
    monitor.libcamera = disabled
    monitor.v4l2 = disabled
    support.reserve-device = disabled
    support.portal-permissionstore = disabled
    support.dbus = disabled
  }
}
CONF
  # WirePlumber 0.4 reads Lua files instead.
  mkdir -p "$dir/config/wireplumber/main.lua.d" "$dir/config/wireplumber/bluetooth.lua.d"
  printf 'alsa_monitor.enabled = false\nv4l2_monitor.enabled = false\nlibcamera_monitor.enabled = false\n' \
    > "$dir/config/wireplumber/main.lua.d/51-niruio-sandbox.lua"
  printf 'bluez_monitor.enabled = false\n' > "$dir/config/wireplumber/bluetooth.lua.d/51-niruio-sandbox.lua"
  export XDG_RUNTIME_DIR="$dir/run" PIPEWIRE_RUNTIME_DIR="$dir/run" XDG_STATE_HOME="$dir/state" \
    XDG_CONFIG_HOME="$dir/config" DBUS_SESSION_BUS_ADDRESS=disabled: PIPEWIRE_LOG_SYSTEMD=false
  unset DISPLAY WAYLAND_DISPLAY PULSE_SERVER
  setsid pipewire > "$dir/pipewire.log" 2>&1 < /dev/null &
  echo $! > "$dir/pipewire.pid"
  i=0; while [ ! -S "$dir/run/pipewire-0" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i+1)); done
  setsid wireplumber > "$dir/wireplumber.log" 2>&1 < /dev/null &
  echo $! > "$dir/wireplumber.pid"
  i=0; while ! wpctl status >/dev/null 2>&1 && [ $i -lt 50 ]; do sleep 0.1; i=$((i+1)); done
  sleep 0.5
  ;;
stop)
  for p in wireplumber pipewire; do
    [ -f "$dir/$p.pid" ] && kill "$(cat "$dir/$p.pid")" 2>/dev/null || :
  done
  sleep 0.3
  ;;
esac

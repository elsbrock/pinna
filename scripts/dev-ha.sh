#!/usr/bin/env bash
# A Home Assistant for the virtual Echo Dot (ADR 0014), on this computer:
# Home Assistant, Wyoming Whisper (speech to text) and Piper (text to speech)
# in containers, set up with an Assist pipeline and the virtual Dot, which
# it reaches at 127.0.0.1 (no firewall, no VLAN in between).
#
#   just dev-ha              # http://localhost:8123, no login from this computer
#   just dev-ha stop         # remove the containers (build/dev-ha/ stays)
#   FRESH=1 just dev-ha      # start over with an empty Home Assistant
#
# Needs podman or docker (CONTAINER=docker to choose). State lives in
# build/dev-ha/. Run it again after `just dev-vm` if the Dot was not up the
# first time: it adds whatever is missing.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=${WORK:-$root/build/dev-ha}
engine=${CONTAINER:-$(command -v podman || command -v docker || true)}
[ -n "$engine" ] || { echo "needs podman or docker" >&2; exit 1; }
esphome_port=${ESPHOME_PORT:-6053}

ha_image=ghcr.io/home-assistant/home-assistant:2026.11.1
whisper_image=docker.io/rhasspy/wyoming-whisper:3.8.1
piper_image=docker.io/rhasspy/wyoming-piper:2.5.3
names=(pinna-dev-ha pinna-dev-whisper pinna-dev-piper)

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }

if [ "${1:-}" = stop ]; then
  "$engine" rm -f "${names[@]}" >/dev/null
  log "stopped (state kept in $work)"
  exit 0
fi

if [ -n "${FRESH:-}" ]; then
  "$engine" rm -f "${names[@]}" >/dev/null 2>&1 || true
  rm -rf "${work:?}"
fi
mkdir -p "$work/config" "$work/whisper" "$work/piper"

# Logins from this computer need no password. The satellite fetches replies
# from Home Assistant at 10.0.2.2, QEMU's address for this computer, which
# reaches the loopback-only listener.
tz=$(timedatectl show -p Timezone --value 2>/dev/null || echo UTC)
if [ ! -s "$work/config/configuration.yaml" ]; then
  cat >"$work/config/configuration.yaml" <<EOF
# Written by scripts/dev-ha.sh: a Home Assistant for the virtual Echo Dot.
default_config:

homeassistant:
  name: Pinna dev
  time_zone: "$tz"
  internal_url: "http://10.0.2.2:8123"
  auth_providers:
    - type: trusted_networks
      trusted_networks:
        - 127.0.0.1/32
        - ::1/128
      allow_bypass_login: true
    - type: homeassistant

http:
  server_host:
    - 127.0.0.1
    - ::1
EOF
fi

run() {
  local name=$1
  shift
  "$engine" rm -f "$name" >/dev/null 2>&1 || true
  "$engine" run -d --name "$name" --network host "$@" >/dev/null
}
log "starting Home Assistant, Whisper and Piper ($(basename "$engine"))"
# The images listen on every interface unless told otherwise (the last --uri wins).
run pinna-dev-whisper -v "$work/whisper:/data:Z" "$whisper_image" --model base-int8 --language en --uri tcp://127.0.0.1:10300
run pinna-dev-piper -v "$work/piper:/data:Z" "$piper_image" --voice en_US-lessac-medium --uri tcp://127.0.0.1:10200
run pinna-dev-ha -v "$work/config:/config:Z" -e TZ="$tz" "$ha_image"

log "setting up (onboarding, Whisper, Piper, the Assist pipeline, the virtual Dot)"
uv run -q --with websockets==15.0.1 python "$root/scripts/dev-ha-setup.py" "$work" "$esphome_port"

cat <<EOF

  Home Assistant is up: http://localhost:8123 (no login from this computer)
  Logs     $(basename "$engine") logs -f pinna-dev-ha
  Stop     just dev-ha stop
EOF

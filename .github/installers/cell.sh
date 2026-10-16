#!/usr/bin/env bash
# One install cell of installers.yml on macOS or Linux: installs a component
# selection with the Qt IFW installer, then checks that exactly the selected
# components are present and that each present one runs.
#
# Usage: cell.sh <installer> <selection> <version>
#   <installer>  the .dmg on macOS, the .run on Linux
#   <selection>  all, workshop, gateway, or gateway-stt
#   <version>    the version every installed executable must print
#
# Environment:
#   CELL_ROOT    install here (IFW --root) instead of the default location,
#                for local runs with a temporary HOME
#   CELL_LOGS    write the logs here (default: a new temporary directory)
#
# Written for bash 3.2, the bash macOS ships.
set -euo pipefail

fail() {
  echo "cell: $*" >&2
  exit 1
}

[ "$#" -eq 3 ] || fail "usage: cell.sh <installer> <all|workshop|gateway|gateway-stt> <version>; got $# arguments"
installer=$1
selection=$2
version=$3
[ -f "$installer" ] || fail "installer $installer does not exist"
case "$installer" in
  /*) ;;
  *) installer="$PWD/$installer" ;;
esac

want_workshop=0
want_gateway=0
want_stt=0
case "$selection" in
  all) want_workshop=1 want_gateway=1 want_stt=1 ;;
  workshop) want_workshop=1 ;;
  gateway) want_gateway=1 ;;
  gateway-stt) want_gateway=1 want_stt=1 ;;
  *) fail "selection must be all, workshop, gateway, or gateway-stt; got '$selection'" ;;
esac
ids=""
[ "$want_workshop" = 0 ] || ids="$ids com.promptforge.workshop"
[ "$want_gateway" = 0 ] || ids="$ids com.promptforge.gateway"
[ "$want_stt" = 0 ] || ids="$ids com.promptforge.stt"

system=$(uname -s)
profile="$HOME/.promptforge"
applications="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
case "$system" in
  Darwin)
    root=${CELL_ROOT:-/Applications/PromptForge}
    workshop_item="$root/PromptForge.app"
    workshop_exe="$workshop_item/Contents/MacOS/promptforge-workshop"
    gateway_item="$root/PromptForge Gateway.app"
    gateway_exe="$gateway_item/Contents/MacOS/promptforge-gateway"
    whisper_library=libwhisper.dylib
    ;;
  Linux)
    root=${CELL_ROOT:-$HOME/PromptForge}
    workshop_item="$root/PromptForge.AppImage"
    workshop_exe=$workshop_item
    gateway_item="$root/promptforge-gateway"
    gateway_exe=$gateway_item
    whisper_library=libwhisper.so
    ;;
  *) fail "cell.sh runs on macOS or Linux; uname -s printed $system" ;;
esac

logs=${CELL_LOGS:-$(mktemp -d)}
mkdir -p "$logs"
echo "cell: $selection on $system, logs in $logs"

mount=""
started=""
cleanup() {
  for pid in $started; do
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  if [ -n "$mount" ]; then
    hdiutil detach "$mount" -quiet 2>/dev/null || true
  fi
  if [ -d "$profile/logs" ]; then
    rm -rf "$logs/promptforge-logs"
    cp -R "$profile/logs" "$logs/promptforge-logs" 2>/dev/null || true
  fi
}
trap cleanup EXIT

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

canonical() {
  (cd "$(dirname "$1")" && printf '%s/%s\n' "$(pwd -P)" "$(basename "$1")")
}

# The executable of a running process, as an absolute path.
process_exe() {
  case "$system" in
    Linux) readlink "/proc/$1/exe" 2>/dev/null || true ;;
    Darwin) ps -o comm= -p "$1" 2>/dev/null || true ;;
  esac
}

# A process and all of its descendants, space separated. Workshop's
# listener may sit in a child: the AppImage runtime and xvfb-run both start
# the app below the process the cell launched.
process_tree() {
  local queue=$1 all="" pid children
  while [ -n "$queue" ]; do
    pid=${queue%% *}
    if [ "$queue" = "$pid" ]; then queue=""; else queue=${queue#* }; fi
    all="$all $pid"
    children=$(pgrep -P "$pid" 2>/dev/null | tr '\n' ' ' || true)
    queue=$(echo "$queue $children" | xargs)
  done
  echo "$all" | xargs
}

# The 127.0.0.1 ports any of the given processes listens on.
loopback_ports() {
  local pids
  pids=$(echo "$*" | tr ' ' ',')
  lsof -nP -a -p "$pids" -iTCP -sTCP:LISTEN -Fn 2>/dev/null |
    sed -n 's/^n127\.0\.0\.1:\([0-9][0-9]*\)$/\1/p' | sort -u || true
}

json_field() {
  python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"
}

# "source digest" for every [[stt_model]] entry in a gateway.toml.
stt_models() {
  awk '
    function flush() { if (in_stt) print source, digest; source = ""; digest = "" }
    /^\[/ { flush(); in_stt = ($0 == "[[stt_model]]"); next }
    in_stt && $1 == "source" { source = $3; gsub(/"/, "", source) }
    in_stt && $1 == "sha256" { digest = $3; gsub(/"/, "", digest) }
    END { flush() }
  ' "$1"
}

# 1. Install at the default location, or under CELL_ROOT.
case "$system" in
  Darwin)
    mount=$(mktemp -d)
    hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$installer" >"$logs/hdiutil.log" 2>&1 ||
      fail "hdiutil attach $installer failed; see $logs/hdiutil.log"
    setup="$mount/PromptForge Installer.app/Contents/MacOS/PromptForge Installer"
    ;;
  Linux)
    chmod +x "$installer"
    setup=$installer
    ;;
esac
[ -x "$setup" ] || fail "the installer executable $setup is missing"
set -- --accept-licenses --default-answer --no-default-installations --confirm-command
if [ -n "${CELL_ROOT:-}" ]; then
  set -- --root "$CELL_ROOT" "$@"
fi
# shellcheck disable=SC2086 # the component ids are separate words
set -- "$@" install $ids
echo "cell: installing$ids into $root"
status=0
"$setup" "$@" >"$logs/installer.log" 2>&1 || status=$?
if [ "$status" -ne 0 ]; then
  tail -n 40 "$logs/installer.log" >&2
  fail "the installer exited $status, required 0; full output in $logs/installer.log"
fi
if [ -n "$mount" ]; then
  hdiutil detach "$mount" -quiet || true
  mount=""
fi

# 2. Exactly the selected components are present, with Speech to Text's
# models at their pinned digests.
check_presence() {
  local name=$1 wanted=$2 path=$3
  if [ "$wanted" = 1 ] && [ ! -e "$path" ]; then
    fail "$name was selected, but $path is missing"
  fi
  if [ "$wanted" = 0 ] && [ -e "$path" ]; then
    fail "$name was not selected, but $path exists"
  fi
}
check_presence Workshop "$want_workshop" "$workshop_item"
check_presence Gateway "$want_gateway" "$gateway_item"
if [ "$system" = Linux ]; then
  check_presence "Workshop's desktop entry" "$want_workshop" "$applications/com.promptforge.workshop.desktop"
  check_presence "the Gateway's desktop entry" "$want_gateway" "$applications/com.promptforge.gateway.desktop"
  for entry in "$applications"/com.promptforge.*.desktop; do
    [ -e "$entry" ] || continue
    desktop-file-validate "$entry" || fail "$entry fails desktop-file-validate"
    exec_path=$(sed -n 's/^Exec="\([^"]*\)".*/\1/p' "$entry")
    [ -n "$exec_path" ] || fail "$entry has no quoted Exec path"
    [ -x "$exec_path" ] || fail "$entry runs $exec_path, which is not an executable file"
    icon=$(sed -n 's/^Icon=//p' "$entry")
    [ -f "$icon" ] || fail "$entry names the icon $icon, which does not exist"
  done
fi

libraries=$(find "$profile" -name "$whisper_library" \( -type f -o -type l \) 2>/dev/null || true)
library_count=$(printf '%s' "$libraries" | grep -c . || true)
config="$profile/gateway.toml"
if [ "$want_stt" = 1 ]; then
  [ "$library_count" = 1 ] || fail "Speech to Text needs exactly one $whisper_library under $profile, found $library_count: $libraries"
  [ -f "$config" ] || fail "Speech to Text was selected, but $config is missing"
  models=$(stt_models "$config")
  model_count=$(printf '%s' "$models" | grep -c . || true)
  [ "$model_count" = 2 ] || fail "Speech to Text needs both models, so $config must declare two [[stt_model]] entries; it declares $model_count"
  # Checked before anything starts the Gateway, whose boot would download
  # a model the install left out.
  while read -r source digest; do
    name=${source##*/}
    files=$(find "$profile" -type f -name "$name")
    count=$(printf '%s' "$files" | grep -c . || true)
    [ "$count" = 1 ] || fail "the model $name needs exactly one file under $profile, found $count"
    actual=$(sha256 "$files")
    [ "$actual" = "$digest" ] || fail "$files has SHA-256 $actual, required $digest"
    echo "cell: $name matches its pinned digest"
  done <<EOF
$models
EOF
else
  [ "$library_count" = 0 ] || fail "Speech to Text was not selected, but $profile holds $libraries"
  # Without init the Gateway's first boot would write its default config,
  # which declares the speech models, so the config must exist already.
  if [ "$want_gateway" = 1 ] && [ ! -f "$config" ]; then
    fail "Gateway was selected, so init --no-stt must have written $config; it is missing"
  fi
  if [ -f "$config" ] && grep -q '^\[\[stt_model\]\]' "$config"; then
    fail "Speech to Text was not selected, but $config declares an [[stt_model]]"
  fi
  model_files=$(find "$profile/models" -type f 2>/dev/null || true)
  [ -z "$model_files" ] || fail "Speech to Text was not selected, but models were downloaded: $model_files"
fi

# 3. Every present executable prints the version.
check_version() {
  local exe=$1 expected=$2 output
  if ! output=$("$exe" --version 2>"$logs/version.stderr.log"); then
    tail -n 20 "$logs/version.stderr.log" >&2
    fail "$exe --version failed; full output in $logs/version.stderr.log"
  fi
  [ "$output" = "$expected" ] || fail "$exe --version printed '$output', required '$expected'"
}
[ "$want_workshop" = 0 ] || check_version "$workshop_exe" "promptforge-workshop $version"
[ "$want_gateway" = 0 ] || check_version "$gateway_exe" "promptforge-gateway $version"

# 4. Workshop serves its page, attached to the installed Gateway when there
# is one and to the fixture endpoint otherwise.
gateway_origin=""
if [ "$want_workshop" = 1 ]; then
  mkdir -p "$profile"
  if [ "$want_gateway" = 0 ]; then
    cat >"$profile/workshop.toml" <<'EOF'
[gateway]
base_url = "http://127.0.0.1:1"
api_key = "release-smoke"
EOF
  fi
  if [ "$system" = Linux ]; then
    pid_file=$(mktemp)
    # shellcheck disable=SC2016 # the inner sh expands them
    xvfb-run -a sh -c 'echo $$ > "$1"; exec "$2"' sh "$pid_file" "$workshop_exe" \
      >"$logs/workshop.stdout.log" 2>"$logs/workshop.stderr.log" &
    started="$started $!"
    for _ in $(seq 1 30); do
      [ -s "$pid_file" ] && break
      sleep 1
    done
    workshop_pid=$(cat "$pid_file")
    [ -n "$workshop_pid" ] || fail "xvfb-run never started Workshop; see $logs/workshop.stderr.log"
  else
    "$workshop_exe" >"$logs/workshop.stdout.log" 2>"$logs/workshop.stderr.log" &
    workshop_pid=$!
  fi
  started="$started $workshop_pid"
  workshop_port=""
  for _ in $(seq 1 120); do
    if ! kill -0 "$workshop_pid" 2>/dev/null; then
      tail -n 40 "$logs/workshop.stdout.log" "$logs/workshop.stderr.log" >&2
      fail "Workshop exited before serving its page"
    fi
    for port in $(loopback_ports "$(process_tree "$workshop_pid")"); do
      if curl -sf --max-time 2 "http://127.0.0.1:$port/" -o "$logs/workshop.html" &&
        grep -Fq "<title>PromptForge</title>" "$logs/workshop.html"; then
        workshop_port=$port
        break
      fi
    done
    [ -z "$workshop_port" ] || break
    sleep 1
  done
  [ -n "$workshop_port" ] || fail "Workshop never served <title>PromptForge</title> on a loopback port within 120 seconds"
  curl -sf --max-time 30 "http://127.0.0.1:$workshop_port/app.js" -o "$logs/app.js" ||
    fail "http://127.0.0.1:$workshop_port/app.js did not answer"
  size=$(wc -c <"$logs/app.js" | tr -d ' ')
  # The minified workbench is about 2.5 MB.
  [ "$size" -gt 0 ] && [ "$size" -lt 3000000 ] || fail "app.js must be under 3000000 bytes; it is $size"
  echo "cell: Workshop serves its page on port $workshop_port"

  if [ "$want_gateway" = 1 ]; then
    discovery="$profile/run/gateway.json"
    installed=$(canonical "$gateway_exe")
    gateway_port=""
    found="no discovery file"
    for _ in $(seq 1 120); do
      if [ -f "$discovery" ]; then
        gateway_pid=$(json_field "$discovery" pid 2>/dev/null || true)
        if [ -n "$gateway_pid" ] && kill -0 "$gateway_pid" 2>/dev/null; then
          exe=$(process_exe "$gateway_pid")
          if [ -n "$exe" ] && [ "$(canonical "$exe")" = "$installed" ]; then
            gateway_port=$(json_field "$discovery" port) || fail "$discovery names no port"
            started="$started $gateway_pid"
            break
          fi
          found="pid $gateway_pid running $exe"
        else
          found="pid ${gateway_pid:-none}, not running"
        fi
      fi
      sleep 1
    done
    [ -n "$gateway_port" ] || fail "$discovery must name a live Gateway running $installed; found $found"
    gateway_origin="http://127.0.0.1:$gateway_port"
    reported=""
    for _ in $(seq 1 30); do
      reported=$(curl -sf "http://127.0.0.1:$workshop_port/gateway/origin" | python3 -c 'import json, sys; print(json.load(sys.stdin)["origin"])' || true)
      [ "$reported" = "$gateway_origin" ] && break
      sleep 1
    done
    [ "$reported" = "$gateway_origin" ] || fail "Workshop's /gateway/origin must be $gateway_origin, the discovery file's; it is '$reported'"
    echo "cell: Workshop is attached to the installed Gateway at $gateway_origin"
  fi
fi

# 5. The Gateway serves its Settings page. Nothing launched it without
# Workshop, so the cell starts it, from a directory holding no config.
if [ "$want_gateway" = 1 ]; then
  if [ -z "$gateway_origin" ]; then
    workdir=$(mktemp -d)
    (cd "$workdir" && exec "$gateway_exe" --print-url) \
      >"$logs/gateway.stdout.log" 2>"$logs/gateway.stderr.log" &
    gateway_pid=$!
    started="$started $gateway_pid"
    # The log shares stdout with the URL, which is a line of its own.
    url=""
    for _ in $(seq 1 120); do
      url=$(grep -m 1 -E '^http://127\.0\.0\.1:[0-9]+/' "$logs/gateway.stdout.log" || true)
      [ -n "$url" ] && break
      kill -0 "$gateway_pid" 2>/dev/null || break
      sleep 1
    done
    if [ -z "$url" ]; then
      tail -n 40 "$logs/gateway.stdout.log" "$logs/gateway.stderr.log" >&2
      fail "the Gateway must print its http://127.0.0.1 URL under --print-url within 120 seconds; it printed none"
    fi
    gateway_origin=$(echo "$url" | sed -E 's#^(http://[^/]+).*#\1#')
  fi
  curl -sf --max-time 10 "$gateway_origin/config/" -o "$logs/gateway-config.html" ||
    fail "$gateway_origin/config/ did not answer"
  grep -Fq "<title>PromptForge Gateway Config</title>" "$logs/gateway-config.html" ||
    fail "$gateway_origin/config/ lacks <title>PromptForge Gateway Config</title>"
  echo "cell: the Gateway serves /config/ at $gateway_origin"
fi

# 6. Speech to Text: the library loads.
if [ "$want_stt" = 1 ]; then
  LIBRARY="$libraries" python3 - <<'PY' || fail "$libraries does not load"
import ctypes
import os
whisper = ctypes.CDLL(os.environ["LIBRARY"])
whisper.whisper_print_system_info.restype = ctypes.c_char_p
print(whisper.whisper_print_system_info().decode())
PY
fi

echo "cell: $selection passed"

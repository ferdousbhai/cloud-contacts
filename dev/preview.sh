#!/usr/bin/env bash
set -Eeuo pipefail

# Pictures of the Contacts window in a nested Hyprland with a private HOME, XDG directories and D-Bus, on the fake
# iCloud of examples/fake_contacts.rs and the fake gws of tests/fake-gws, so nothing of the desktop it is run from is
# read or shown on: not its accounts, its contacts, its theme, or its windows.
#
#   dev/preview.sh <out-dir> [theme-dir ...]
#
# Each scene is written as <out-dir>/<scene>-<theme>.png. Both an iCloud and a Google account are linked, and Google
# holds two cards for one person, so the offers to mirror and to merge duplicates show.
#   list        the window as it opens: the contacts, the first one's card (in a narrow window: the list alone)
#   card        Ann Lee found by her work number and opened (in a narrow window: her card in the list's place)
#   back        the same, then Escape: in a narrow window, back to the list
#   menu        the accounts menu open
#   new         New Contact: nothing typed, Save disabled
#   typed       New Contact with a first name typed: Save enabled
#   edit        Edit Contact on Ann Lee
#   duplicates  the duplicates dialog
#   review      Turn On… mirroring: what it would do, the doubtful pairs to call
#   welcome     Google unlinked, then iCloud from the window: the page that links one (taken last: it unlinks them)
# `theme-dir` holds a `colors.toml` (an Omarchy theme, such as /usr/share/omarchy/themes/catppuccin-latte) and is
# copied in as the scratch desktop's current theme; without one the pictures are named `-default`.
#
#   CLOUD_CONTACTS_PREVIEW_SCENES  the scenes to take (default: all)
#   CLOUD_CONTACTS_PREVIEW_SIZE    the size of the window and of the pictures (default 1000x720; 420x720 is narrow)
#   CLOUD_CONTACTS_PREVIEW_SETTLE  seconds to wait before each picture (default 2)
if (( $# < 1 )); then
  echo "usage: $0 <out-dir> [theme-dir ...]" >&2
  exit 2
fi
out=$(realpath -m -- "$1")
shift
themes=("$@")
(( ${#themes[@]} )) || themes=("")

here=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
target=$(realpath -m -- "$here/../target/debug")
fake_gws=$(realpath -- "$here/../crates/cloud-contacts/tests/fake-gws")
for binary in cloud-contacts cloud-contacts-app examples/fake_contacts; do
  [[ -x $target/$binary ]] || { echo "preview: build first (cargo build --bins --examples)" >&2; exit 1; }
done
for command_name in Hyprland hyprctl grim jq dbus-daemon gdbus setsid wtype python3; do
  command -v "$command_name" >/dev/null || { echo "preview: $command_name is required" >&2; exit 1; }
done
for theme in "${themes[@]}"; do
  [[ -z $theme || -f $theme/colors.toml ]] || { echo "preview: $theme has no colors.toml" >&2; exit 1; }
done
read -r -a scenes <<< "${CLOUD_CONTACTS_PREVIEW_SCENES:-list card back menu new typed edit duplicates review welcome}"
# `action:<name>[=<string>]` activates the window's action <name> over the scratch session bus, as its menus and
# shortcuts do; `text:<words>` types them (an underscore is a space); the rest are keys, with modifiers named before
# them held. The search keeps the keyboard after a search; New Contact opens with it on the account; the accounts
# menu is two stops before the search (shift+Tab is ISO_Left_Tab); Unlink is Alt+U.
declare -A keys=(
  [list]=""
  [card]="action:find text:0199 Return"
  [back]="action:find text:0199 Return Escape"
  [menu]="action:find shift+ISO_Left_Tab shift+ISO_Left_Tab space"
  [new]="action:new"
  [typed]="action:new Tab text:Ada"
  [edit]="action:find text:0199 Return action:edit"
  [duplicates]="action:duplicates"
  [review]="action:mirror-review"
  [welcome]="action:unlink=icloud alt+u"
)
for scene in "${scenes[@]}"; do
  [[ -v keys[$scene] ]] || { echo "preview: no scene named $scene" >&2; exit 2; }
done
parent_runtime=${XDG_RUNTIME_DIR:?preview: XDG_RUNTIME_DIR is required}
parent_display=${WAYLAND_DISPLAY:?preview: run this from a Wayland desktop}
parent_socket=$parent_runtime/$parent_display
settle=${CLOUD_CONTACTS_PREVIEW_SETTLE:-2}
size=${CLOUD_CONTACTS_PREVIEW_SIZE:-1000x720}
[[ $size =~ ^[0-9]+x[0-9]+$ ]] || { echo "preview: CLOUD_CONTACTS_PREVIEW_SIZE is <width>x<height>" >&2; exit 2; }

root=""
runtime=""
hypr_pid=""
app_pid=""
fake_pid=""
dbus_pid=""
# Only the PIDs this script started.
stop() {
  local pid
  for pid in "$@"; do
    [[ -z $pid ]] || kill -TERM "$pid" 2>/dev/null || true
  done
  for _ in {1..30}; do
    local alive=0
    for pid in "$@"; do
      [[ -z $pid ]] || ! kill -0 "$pid" 2>/dev/null || alive=1
    done
    (( alive == 0 )) && break
    sleep 0.1
  done
  for pid in "$@"; do
    [[ -z $pid ]] || kill -KILL "$pid" 2>/dev/null || true
  done
}
cleanup() {
  local status=$?
  trap - EXIT INT TERM
  stop "$app_pid" "$fake_pid" "$hypr_pid" "$dbus_pid"
  [[ -z $root || $root != /tmp/cloud-contacts-preview.* ]] || rm -rf -- "$root"
  [[ -z $runtime || $runtime != /tmp/ccp.* ]] || rm -rf -- "$runtime"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Wayland and Hyprland IPC use AF_UNIX paths capped at 108 bytes.
root=$(mktemp -d /tmp/cloud-contacts-preview.XXXXXX)
runtime=$(mktemp -d /tmp/ccp.XXXXXX)
chmod 700 "$root" "$runtime"

export HOME=$root/home
# The XDG directories where they are by default: Omarchy writes its current theme under ~/.local/state.
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state XDG_DATA_HOME=$HOME/.local/share
export XDG_CACHE_HOME=$HOME/.cache
export XDG_RUNTIME_DIR=$runtime
export LC_ALL=C TZ=UTC GTK_A11Y=none
unset GTK_THEME
# Google is the fake gws on an address book in a file; a sign-in opens no browser and mirroring touches no timer.
export CLOUD_CONTACTS_GWS_COMMAND=$fake_gws FAKE_GWS_STATE=$root/gws.json
export CLOUD_CONTACTS_BROWSER=true CLOUD_CONTACTS_SYSTEMCTL=true FAKE_CONTACTS_QUIET=1

# A fake iCloud with its seeded contacts on a port of its own (it says which), a Google address book with Ann Lee
# (in iCloud too) and Gus Grant twice, and both accounts linked, as a first run with the window would leave them.
start_accounts() {
  stop "$fake_pid"
  rm -rf -- "$HOME" "$FAKE_GWS_STATE"
  mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
  "$target/examples/fake_contacts" 127.0.0.1:0 > "$root/fake.log" 2>&1 &
  fake_pid=$!
  local fake=""
  for _ in {1..50}; do
    fake=$(sed -n 's/^Fake iCloud Contacts on //p' "$root/fake.log")
    [[ -z $fake ]] || break
    sleep 0.1
  done
  [[ -n $fake ]] || { echo "preview: the fake iCloud did not start" >&2; cat "$root/fake.log" >&2; exit 1; }
  export ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=$fake
  python3 - "$FAKE_GWS_STATE" <<'EOF_PEOPLE'
import json, sys
gus = lambda n: {
    "resourceName": f"people/c{n}", "etag": f"g{n}",
    "names": [{"givenName": "Gus", "familyName": "Grant"}],
    "phoneNumbers": [{"value": "+1 555 0101", "type": "mobile"}],
    "emailAddresses": [{"value": "gus@example.com", "type": "work"}] if n == 1 else [],
}
people = {
    "people/c1": gus(1),
    "people/c2": gus(2),
    "people/c3": {"resourceName": "people/c3", "etag": "g3", "names": [{"givenName": "Ann", "familyName": "Lee"}],
                  "organizations": [{"name": "Google Ann Co"}]},
}
json.dump({"people": people, "seq": 3, "calls": []}, open(sys.argv[1], "w"))
EOF_PEOPLE
  "$target/cloud-contacts" status > /dev/null
  "$target/cloud-contacts" account add google > /dev/null 2>&1
}

# A session bus with no service directories: nothing can be dbus-activated on it.
printf '%s\n' \
  '<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"' \
  ' "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">' \
  '<busconfig>' \
  '  <type>session</type>' \
  "  <listen>unix:abstract=cloud-contacts-preview-$BASHPID</listen>" \
  '  <keep_umask/>' \
  '  <policy context="default">' \
  '    <allow send_destination="*" eavesdrop="true"/>' \
  '    <allow eavesdrop="true"/>' \
  '    <allow own="*"/>' \
  '  </policy>' \
  '</busconfig>' > "$root/dbus.conf"
mapfile -t dbus_details < <(/usr/bin/dbus-daemon --config-file="$root/dbus.conf" --fork --print-address=1 --print-pid=1)
(( ${#dbus_details[@]} == 2 )) || { echo "preview: could not start the isolated session bus" >&2; exit 1; }
export DBUS_SESSION_BUS_ADDRESS=${dbus_details[0]}
dbus_pid=${dbus_details[1]}

printf '%s\n' \
  "hl.monitor({ output = \"\", mode = \"$size@60\", position = \"auto\", scale = 1 })" \
  'hl.config({' \
  '  general = { gaps_in = 0, gaps_out = 0, border_size = 0 },' \
  '  decoration = { rounding = 0, shadow = { enabled = false }, blur = { enabled = false } },' \
  '  animations = { enabled = false },' \
  '  misc = { disable_hyprland_logo = true, disable_splash_rendering = true },' \
  '})' \
  'hl.window_rule({ match = { class = ".*" }, monitor = "preview" })' > "$root/hyprland.lua"
unset HYPRLAND_INSTANCE_SIGNATURE
/usr/bin/setsid /usr/bin/env WAYLAND_DISPLAY="$parent_socket" /usr/bin/Hyprland --config "$root/hyprland.lua" > "$root/hyprland.log" 2>&1 &
hypr_pid=$!
signature=""
display=""
for _ in {1..120}; do
  kill -0 "$hypr_pid" 2>/dev/null || { echo "preview: nested Hyprland exited during startup" >&2; sed -n '1,80p' "$root/hyprland.log" >&2; exit 1; }
  for instance in "$XDG_RUNTIME_DIR"/hypr/*; do
    [[ -S $instance/.socket.sock && -f $instance/hyprland.lock ]] || continue
    candidate=$(sed -n '2p' "$instance/hyprland.lock")
    [[ -n $candidate && -S $XDG_RUNTIME_DIR/$candidate ]] || continue
    signature=$(basename "$instance")
    display=$candidate
    break 2
  done
  sleep 0.1
done
[[ -n $signature ]] || { echo "preview: nested Hyprland did not publish its sockets" >&2; exit 1; }
export HYPRLAND_INSTANCE_SIGNATURE=$signature WAYLAND_DISPLAY=$display

# The nested compositor's own output is a window on the desktop this runs from, as large as that desktop tiles it.
# The app goes on a headless output instead, whose size is the monitor rule's; the window rule sends it there.
hyprctl output create headless preview >/dev/null

on_output() {
  local output
  output=$(hyprctl monitors -j | jq 'first(.[] | select(.name == "preview") | .id)') &&
    hyprctl clients -j | jq -e --argjson output "${output:-null}" 'any(.[]; .mapped and .monitor == $output)' >/dev/null
}
press() {
  local key
  for key in $1; do
    if [[ $key == action:* ]]; then
      # The app's one window, as GtkApplication exports it on the session bus.
      local action=${key#action:} parameter='[]'
      if [[ $action == *=* ]]; then
        parameter="[<'${action#*=}'>]"
        action=${action%%=*}
      fi
      gdbus call --session --dest com.ferdousbhai.cloudcontacts --object-path /com/ferdousbhai/cloudcontacts/window/1 \
        --method org.gtk.Actions.Activate "$action" "$parameter" '{}' >/dev/null
    elif [[ $key == text:* ]]; then
      local text=${key#text:}
      wtype "${text//_/ }"
    else
      local -a held=()
      while [[ $key == *+* ]]; do
        held+=(-M "${key%%+*}")
        key=${key#*+}
      done
      wtype "${held[@]}" -k "$key"
    fi
    sleep 0.6
  done
}
# One scene: the app from its start, the scene's keys, the picture, and the app stopped again.
shoot() {
  local scene=$1 picture=$2
  if [[ $scene == welcome ]]; then
    "$target/cloud-contacts" account remove google --yes > /dev/null
  fi
  GDK_BACKEND=wayland "$target/cloud-contacts-app" > "$root/app.log" 2>&1 &
  app_pid=$!
  for _ in {1..100}; do
    on_output && break
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 0.1
  done
  # Once the window is drawn and its first fetch is shown. The nested compositor's keyboard is the app's: it is the
  # only client there.
  sleep 2
  press "${keys[$scene]}"
  sleep "$settle"
  kill -0 "$app_pid" 2>/dev/null || { echo "preview: the app exited" >&2; sed -n '1,40p' "$root/app.log" >&2; exit 1; }
  # A picture of an output the window is not on would be blank, and look like a result.
  on_output || { echo "preview: the app's window is not on the output being captured" >&2; hyprctl clients >&2; exit 1; }
  # Hyprland's own notices (it says it was started without its wrapper) would cover the header.
  hyprctl dismissnotify >/dev/null
  /usr/bin/grim -o preview "$picture"
  echo "preview: $picture"
  [[ ! -s $root/app.log ]] || sed -n '1,20p' "$root/app.log" >&2
  stop "$app_pid"
  app_pid=""
}

mkdir -p "$out"
for theme in "${themes[@]}"; do
  name=default
  start_accounts
  if [[ -n $theme ]]; then
    name=$(basename -- "$(realpath -- "$theme")")
    current=$XDG_STATE_HOME/omarchy/current
    mkdir -p "$current/theme"
    cp -- "$theme/colors.toml" "$current/theme/colors.toml"
    printf '%s\n' "$name" > "$current/theme.name"
  fi
  for scene in "${scenes[@]}"; do
    shoot "$scene" "$out/$scene-$name.png"
  done
done

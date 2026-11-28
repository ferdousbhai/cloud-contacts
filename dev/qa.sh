#!/usr/bin/env bash
set -Eeuo pipefail

# Runs the Contacts window, built with feature `qa`, through omui's scripted QA (the protocol is in omui's README): each
# script's JSON lines go to the app on stdin, and its answers come back on descriptor 3. Each script gets a broadway
# display of its own, bound to 127.0.0.1, a fresh private HOME and XDG directories, no session bus, and the stand-in
# accounts dev/preview.sh uses: the fake iCloud of examples/fake_contacts.rs and the fake gws of tests/fake-gws on
# dev/google-people.json, both linked. No window opens anywhere visible and no real account is read. Prints each
# command with its answer; fails when a command fails or GLib logs a warning or a critical. bin/check runs every
# script under dev/qa.
#
# A script line `@sibling <app>/gws/<account> <address>` stages a Google sign-in another Cloud app has (see `run`).
#
#   dev/qa.sh <app-binary> <script.jsonl>...
#   CLOUD_CONTACTS_QA_SHOTS=<dir>  where a script's `shot` paths are written (default: a scratch directory, removed)
#   OMUI_QA_TIMEOUT=120            seconds before one script's run is stopped
(( $# >= 2 )) || { echo "usage: dev/qa.sh <app-binary> <script.jsonl>..." >&2; exit 2; }
app=$(realpath -- "$1")
shift
here=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
fake_icloud=$(realpath -- "$(dirname -- "$app")/examples/fake_contacts")
fake_gws=$(realpath -- "$here/../crates/cloud-contacts/tests/fake-gws")
[[ -x $fake_icloud ]] || { echo "qa: build the fake iCloud first (cargo build --examples)" >&2; exit 1; }
command -v gtk4-broadwayd >/dev/null || { echo "qa: gtk4-broadwayd is required" >&2; exit 1; }

root=""
shots_root=""
broadway_pid=""
fake_pid=""
app_pid=""
stop() {
  # Only what this script started.
  local pid
  for pid in "$app_pid" "$fake_pid" "$broadway_pid"; do
    [[ -z $pid ]] || kill -TERM "$pid" 2>/dev/null || true
    [[ -z $pid ]] || wait "$pid" 2>/dev/null || true
  done
  app_pid="" fake_pid="" broadway_pid=""
  [[ -z $root ]] || rm -rf -- "$root"
  root=""
}
trap 'status=$?; stop; [[ -z $shots_root ]] || rm -rf -- "$shots_root"; exit "$status"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if [[ -n ${CLOUD_CONTACTS_QA_SHOTS:-} ]]; then
  mkdir -p "$CLOUD_CONTACTS_QA_SHOTS"
  shots=$(realpath -- "$CLOUD_CONTACTS_QA_SHOTS")
else
  shots_root=$(mktemp -d /tmp/cloud-contacts-qa-shots.XXXXXX)
  shots=$shots_root
fi

run() {
  local script=$1 index=$2
  root=$(mktemp -d /tmp/cloud-contacts-qa.XXXXXX)
  mkdir -p "$root"/{home,config,state,data,cache,runtime}
  chmod 700 "$root/runtime"

  # A display number and input port from this script's PID and the script's place, so that neither two runs at once
  # nor two scripts one after the other share one. broadwayd listens on that port on 127.0.0.1 alone, where omui sends
  # key presses as broadway's web page would.
  local display=$(( 100 + ($$ * 8 + index) % 800 ))
  local port=$(( 8080 + display ))
  XDG_RUNTIME_DIR=$root/runtime gtk4-broadwayd --address 127.0.0.1 --port "$port" ":$display" \
    >"$root/broadwayd.log" 2>&1 &
  broadway_pid=$!
  for _ in {1..50}; do
    [[ -S $root/runtime/broadway$((display + 1)).socket ]] && break
    kill -0 "$broadway_pid" 2>/dev/null || { cat "$root/broadwayd.log" >&2; return 1; }
    sleep 0.1
  done

  FAKE_CONTACTS_QUIET=1 "$fake_icloud" 127.0.0.1:0 >"$root/fake.log" 2>&1 &
  fake_pid=$!
  local fake=""
  for _ in {1..50}; do
    fake=$(sed -n 's/^Fake iCloud Contacts on //p' "$root/fake.log")
    [[ -z $fake ]] || break
    sleep 0.1
  done
  [[ -n $fake ]] || { echo "qa: the fake iCloud did not start" >&2; cat "$root/fake.log" >&2; return 1; }
  cp -- "$here/google-people.json" "$root/gws.json"

  # The app and its command line in the run's home: the fakes stand in for both accounts (the iCloud one linked by
  # the first run, as icloud-session's mock is signed in), a sign-in opens no browser and mirroring touches no timer.
  local -a env=(env -u WAYLAND_DISPLAY -u DISPLAY -u GTK_THEME -u HYPRLAND_INSTANCE_SIGNATURE
    DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent
    HOME="$root/home" XDG_CONFIG_HOME="$root/config" XDG_STATE_HOME="$root/state" XDG_DATA_HOME="$root/data"
    XDG_CACHE_HOME="$root/cache" XDG_RUNTIME_DIR="$root/runtime" LC_ALL=C TZ=UTC
    ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL="$fake"
    CLOUD_CONTACTS_GWS_COMMAND="$fake_gws" FAKE_GWS_STATE="$root/gws.json"
    CLOUD_CONTACTS_BROWSER=true CLOUD_CONTACTS_SYSTEMCTL=true)
  "${env[@]}" "$app" status >/dev/null
  "${env[@]}" "$app" account add google >/dev/null 2>&1
  # A script line `@sibling <app>/gws/<account> <address>` is a Google sign-in another Cloud app has, as the fake gws
  # keeps one, there once the accounts above are linked: what "Add Google Account…" can take over.
  local where address
  while read -r _ where address; do
    mkdir -p "$root/config/$where"
    : >"$root/config/$where/.encryption_key"
    printf '{"token_valid": true, "user": "%s", "scopes": ["https://www.googleapis.com/auth/contacts"]}\n' \
      "$address" >"$root/config/$where/credentials.enc"
  done < <(grep '^@sibling ' "$script" || true)

  # In the background, waited for: bash runs a trap only once the foreground command ends, and `timeout` puts the app
  # in a process group Ctrl-C does not reach. Killing `timeout` stops the app with it.
  # A `language` command's catalog is named from the repository's root, as a script is; the app runs in the shots
  # directory, from which omui would read a relative path, so the app is given the whole path.
  local repo=${here%/dev}
  sed -e '/^@sibling /d' -e "s|\"catalog\":\"dev/|\"catalog\":\"$repo/dev/|g" "$script" >"$root/script.jsonl"
  local status=0
  (
    cd "$shots"
    exec "${env[@]}" GDK_BACKEND=broadway BROADWAY_DISPLAY=":$display" \
      OMUI_QA=1 OMUI_QA_INPUT="127.0.0.1:$port" OMUI_QA_FD=3 \
      timeout "${OMUI_QA_TIMEOUT:-120}" "$app" <"$root/script.jsonl" 3>"$root/answers"
  ) &
  app_pid=$!
  wait "$app_pid" || status=$?
  app_pid=""
  # The display going away ends the app with no answer; what it said is why.
  kill -0 "$broadway_pid" 2>/dev/null || { echo "qa: broadwayd exited:" >&2; cat "$root/broadwayd.log" >&2; }

  # Each command beside its answer; `quit`'s answer, or the failure that ended the run, comes last.
  echo "qa: $script"
  grep -Ev '^[[:space:]]*(#|$|@sibling )' "$script" | awk -v answers="$root/answers" '
    { if ((getline answer < answers) > 0) { print "> " $0; print "< " answer } else { print "> " $0 " (not run)" } }
    END { while ((getline answer < answers) > 0) print "< " answer }'
  return "$status"
}

failed=0
index=0
for script in "$@"; do
  status=0
  run "$(realpath -- "$script")" "$index" || status=$?
  index=$(( index + 1 ))
  stop
  if (( status == 0 )); then echo "qa: passed"; else echo "qa: failed with status $status"; failed=1; fi
done
exit "$failed"

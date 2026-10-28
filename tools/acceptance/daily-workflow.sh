#!/usr/bin/env bash
# The daily workflow, by hand: two people do a day's work with the `javelin` CLI against a
# node, instead of Git and GitHub. Each step prints the command and its output.
#
#   tools/acceptance/daily-workflow.sh http://127.0.0.1:7878/REPOSITORY
#
# Who runs it. Each person is a JAVELIN_HOME that is signed in (`javelin login`), or a token:
#   OWNER_HOME or OWNER_TOKEN       Maintain on the repository: approves and publishes.
#   COLLAB_HOME or COLLAB_TOKEN     Review, a different person: authors and approves.
#   DESKTOP_HOME or DESKTOP_TOKEN   Optional: the collaborator on a second device, for
#                                   `layer take`. Without it that step is skipped.
# The node restart step needs one of:
#   NODE_SERVE="javelin-node serve DIR REPOSITORY 127.0.0.1:PORT"
#                                   The script starts this node, kills it mid-loop and starts
#                                   it again, then stops it at exit. Start nothing yourself.
#   NODE_STOP="..." NODE_START="..." Commands that stop and start a node you run elsewhere.
# Without either, the restart step is skipped.
# Optional: JAVELIN (the binary, default `javelin` on PATH), WORKDIR (default a new temporary
# directory; the clones stay there for inspection).
#
# Each run works under `daily-RUN/` with request IDs and operations of its own, so it can run
# again against the same repository. Exit status is 0 only when every step that ran passed.
set -u

url=${1:?usage: daily-workflow.sh http(s)://HOST[:PORT]/REPOSITORY}
javelin=${JAVELIN:-javelin}
run=$(date +%s)-$$
work=${WORKDIR:-$(mktemp -d "${TMPDIR:-/tmp}/javelin-daily.XXXXXX")}
mkdir -p "$work"
skipped=()
node_pid=

if [ -t 1 ]; then
  bold() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
else
  bold() { printf '\n== %s\n' "$*"; }
fi
fail() {
  printf '\nFAIL: %s\n' "$*" >&2
  exit 1
}

# as WHO ARGS...: runs javelin as owner, collab or desktop, printing the command and output.
# Leaves stdout in $out, stderr in $err and the exit status in $status.
as() {
  local who=$1 home token
  shift
  case $who in
    owner) home=${OWNER_HOME:-$work/owner-home} token=${OWNER_TOKEN:-} ;;
    collab) home=${COLLAB_HOME:-$work/collab-home} token=${COLLAB_TOKEN:-} ;;
    desktop) home=${DESKTOP_HOME:-$work/desktop-home} token=${DESKTOP_TOKEN:-} ;;
  esac
  printf '%s$ javelin %s\n' "$who" "$*"
  local o e
  o=$(mktemp) e=$(mktemp)
  if [ -n "$token" ]; then
    env JAVELIN_HOME="$home" JAVELIN_TOKEN="$token" "$javelin" "$@" >"$o" 2>"$e"
  else
    env -u JAVELIN_TOKEN JAVELIN_HOME="$home" "$javelin" "$@" >"$o" 2>"$e"
  fi
  status=$?
  out=$(cat "$o") err=$(cat "$e")
  rm -f "$o" "$e"
  [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/  /'
  [ -n "$err" ] && printf '%s\n' "$err" | sed 's/^/  ! /'
  return 0
}

# must WHO ARGS...: as, and the command must succeed.
must() {
  as "$@"
  [ "$status" -eq 0 ] || fail "javelin ${*:2} exited $status"
}

# refuses CODE EXIT WHO ARGS...: the command must exit EXIT with error code CODE (JSON output).
refuses() {
  local code=$1 exit=$2
  shift 2
  as "$1" --json "${@:2}"
  [ "$status" -eq "$exit" ] || fail "expected exit $exit ($code), got $status"
  printf '%s%s' "$out" "$err" | grep -q "\"code\":\"$code\"" || fail "expected $code"
}

has() { printf '%s' "$out" | grep -q -- "$1" || fail "expected output to mention: $1"; }

# sync_head WHO CLONE: syncs, and sets $id to the node head the clone now records.
sync_head() {
  as "$1" --json sync --repo "$2"
  [ "$status" -eq 0 ] || fail "sync exited $status"
  id=$(printf '%s' "$out" | grep -o '"head":{"id":"[0-9a-f]*"' | head -n 1 | cut -d'"' -f6)
  [ -n "$id" ] || fail "sync reported no head"
}

# required: the approvals Publish needs, from `request show`.
required() { printf '%s' "$out" | sed -n 's/.*Publish needs \([0-9]*\) approval.*/\1/p' | head -n 1; }

node_stop() {
  if [ -n "${NODE_SERVE:-}" ]; then
    kill "$node_pid" 2>/dev/null
    wait "$node_pid" 2>/dev/null
    node_pid=
  else
    eval "$NODE_STOP"
  fi
}

node_start() {
  if [ -n "${NODE_SERVE:-}" ]; then
    : >"$work/node.log"
    $NODE_SERVE 2>>"$work/node.log" &
    node_pid=$!
    local tries=0
    until grep -q serving "$work/node.log"; do
      tries=$((tries + 1))
      [ "$tries" -lt 200 ] || fail "the node did not start: $(cat "$work/node.log")"
      kill -0 "$node_pid" 2>/dev/null || fail "the node exited: $(cat "$work/node.log")"
      sleep 0.05
    done
  else
    eval "$NODE_START"
  fi
}

restart=no
if [ -n "${NODE_SERVE:-}" ]; then
  restart=yes
  trap '[ -n "$node_pid" ] && kill "$node_pid" 2>/dev/null' EXIT
  node_start
elif [ -n "${NODE_STOP:-}" ] && [ -n "${NODE_START:-}" ]; then
  restart=yes
fi

owner=$work/owner collab=$work/collab desktop=$work/desktop
dir=daily-$run
lib() { printf 'pub fn greet(name: &str) -> String {\n    %s\n}\n' "$1"; }

bold "1. owner: attach a clone and import a small project"
must owner attach "$url" "$owner"
must owner workspace create "import-$run" --repo "$owner"
import=$owner/.javelin/layers/import-$run
mkdir -p "$import/$dir/src"
printf '# app\n\nGreets people.\n' >"$import/$dir/README.md"
lib 'format!("hello {name}")' >"$import/$dir/src/lib.rs"
must owner request open "$dir-import" --layer "import-$run" --operation "$run-import-open" --repo "$owner"
must owner request show "$dir-import" --repo "$owner"
need=$(required)
echo "Publish needs ${need:-?} approval(s) on this repository."
if [ "${need:-0}" -gt 0 ]; then
  must collab attach "$url" "$collab"
  must collab request approve "$dir-import" --revision 1 --operation "$run-import-approve" --repo "$collab"
fi
must owner request publish "$dir-import" --revision 1 --operation "$run-import-publish" --repo "$owner"
must owner sync --repo "$owner"

bold "2. collaborator: attach a second clone, edit a Layer as a plain directory, open a request"
must collab attach "$url" "$collab"
must collab sync --repo "$collab"
must collab workspace create "feature-$run" --repo "$collab"
feature=$collab/.javelin/layers/feature-$run
[ -f "$feature/$dir/src/lib.rs" ] || fail "the import is not in the collaborator's Layer"
lib 'format!("hello, {name}!")' >"$feature/$dir/src/lib.rs"
printf 'Run app.\n' >"$feature/$dir/USAGE.md"
must collab request open "$dir-feature" --layer "feature-$run" --operation "$run-open" --repo "$collab"

bold "3. owner: read the request, comment, approve revision 1"
must owner request list --repo "$owner"
has "$dir-feature"
must owner request show "$dir-feature" --repo "$owner"
has "$dir/USAGE.md"
must owner request comment "$dir-feature" --revision 1 --body "Say what app does" --operation "$run-comment" --repo "$owner"
must owner request approve "$dir-feature" --revision 1 --operation "$run-approve-1" --repo "$owner"
refuses self_approval 1 collab request approve "$dir-feature" --revision 1 --operation "$run-self" --repo "$collab"

bold "4. collaborator: read the comment, revise"
must collab request show "$dir-feature" --repo "$collab"
has "Say what app does"
printf 'Run app to greet the world.\n' >"$feature/$dir/USAGE.md"
must collab request revise "$dir-feature" --revision 1 --operation "$run-revise" --repo "$collab"

bold "5. owner: the approval of revision 1 no longer counts; approve revision 2"
must owner request show "$dir-feature" --repo "$owner"
printf '%s' "$out" | grep -q "revision 2 over remote World [0-9]*: checkpoint [0-9a-f]*, no approvals" ||
  fail "revision 2 should start with no approvals"
if [ "${need:-0}" -gt 0 ]; then
  refuses not_enough_approvals 1 owner request publish "$dir-feature" --revision 2 --operation "$run-early" --repo "$owner"
fi
refuses stale_revision 3 owner request publish "$dir-feature" --revision 1 --operation "$run-stale" --repo "$owner"
must owner request approve "$dir-feature" --revision 2 --operation "$run-approve-2" --repo "$owner"

bold "6. the node restarts mid-loop: the owner's Publish queues, then replays"
if [ "$restart" = yes ]; then
  node_stop
  # The owner's clone saw revision 2's base in `request show`, so no --base is needed.
  must owner request publish "$dir-feature" --revision 2 --operation "$run-publish" --repo "$owner"
  has "queued"
  refuses node_unavailable 1 collab request show "$dir-feature" --repo "$collab"
  node_start
  must owner sync --repo "$owner"
  must collab request show "$dir-feature" --repo "$collab"
  has "published"
  has "Say what app does"
else
  skipped+=("node restart (set NODE_SERVE, or NODE_STOP and NODE_START)")
  echo "SKIP: no way to restart the node"
  must owner request publish "$dir-feature" --revision 2 --operation "$run-publish" --repo "$owner"
fi

bold "7. both clones sync to the same World, byte for byte"
sync_head owner "$owner"
ours=$id
sync_head collab "$collab"
theirs=$id
[ "$ours" = "$theirs" ] || fail "the clones hold different heads: $ours, $theirs"
must owner workspace create "check-$run" --repo "$owner"
must collab workspace create "check-$run" --repo "$collab"
diff -r "$owner/.javelin/layers/check-$run" "$collab/.javelin/layers/check-$run" ||
  fail "the clones' trees differ"
echo "Both clones hold $ours with identical files."

bold "8. a concurrent change to the same lines conflicts cleanly"
must owner workspace create "o-tweak-$run" --repo "$owner"
must collab workspace create "c-tweak-$run" --repo "$collab"
lib 'format!("Hello, {name}!")' >"$owner/.javelin/layers/o-tweak-$run/$dir/src/lib.rs"
lib 'format!("hi {name}")' >"$collab/.javelin/layers/c-tweak-$run/$dir/src/lib.rs"
must owner request open "$dir-o-tweak" --layer "o-tweak-$run" --operation "$run-o-open" --repo "$owner"
must collab request open "$dir-c-tweak" --layer "c-tweak-$run" --operation "$run-c-open" --repo "$collab"
if [ "${need:-0}" -gt 0 ]; then
  must collab request approve "$dir-o-tweak" --revision 1 --operation "$run-c-approve" --repo "$collab"
fi
must owner request approve "$dir-c-tweak" --revision 1 --operation "$run-o-approve" --repo "$owner"
must owner request publish "$dir-o-tweak" --revision 1 --operation "$run-o-publish" --repo "$owner"
refuses conflict 3 owner request publish "$dir-c-tweak" --revision 1 --operation "$run-c-publish" --repo "$owner"
has "$dir/src/lib.rs"

bold "9. collaborator: move the Layer onto the new World, resolve the markers, revise"
must collab sync --repo "$collab"
must collab layer radar "c-tweak-$run" --repo "$collab"
has "$dir/src/lib.rs"
as collab layer update "c-tweak-$run" --repo "$collab"
[ "$status" -eq 3 ] || fail "layer update should report the conflict (exit 3), got $status"
has "c-tweak-$run.2"
updated=$collab/.javelin/layers/c-tweak-$run.2
grep -q '^<<<<<<< Layer c-tweak' "$updated/$dir/src/lib.rs" || fail "the conflict was not marked"
[ ! -e "$collab/.javelin/layers/c-tweak-$run" ] || fail "the old Layer's directory is still there"
refuses unresolved_conflict 1 collab request revise "$dir-c-tweak" --revision 1 --operation "$run-c-revise" --repo "$collab"
lib 'format!("Hello, {name}! hi!")' >"$updated/$dir/src/lib.rs"
must collab request revise "$dir-c-tweak" --revision 1 --operation "$run-c-revise" --repo "$collab"
must owner request approve "$dir-c-tweak" --revision 2 --operation "$run-o-approve-2" --repo "$owner"
must owner request publish "$dir-c-tweak" --revision 2 --operation "$run-c-publish-2" --repo "$owner"

bold "10. collaborator: layer take moves a Layer to their other device"
clones=("owner $owner" "collab $collab")
if [ -n "${DESKTOP_TOKEN:-}${DESKTOP_HOME:-}" ]; then
  must collab sync --repo "$collab"
  must collab workspace create "notes-$run" --repo "$collab"
  printf -- '- started on the laptop\n' >"$collab/.javelin/layers/notes-$run/$dir/NOTES.md"
  must collab request open "$dir-notes" --layer "notes-$run" --operation "$run-n-open" --repo "$collab"
  must desktop attach "$url" "$desktop"
  refuses forbidden 1 owner layer take "notes-$run" --operation "$run-o-take" --repo "$owner"
  must desktop layer take "notes-$run" --operation "$run-take" --repo "$desktop"
  moved=$desktop/.javelin/layers/notes-$run
  grep -q "started on the laptop" "$moved/$dir/NOTES.md" || fail "the Layer did not move"
  printf -- '- started on the laptop\n- late laptop edit\n' >"$collab/.javelin/layers/notes-$run/$dir/NOTES.md"
  refuses layer_stale 1 collab request revise "$dir-notes" --revision 1 --operation "$run-late" --repo "$collab"
  keep=$(printf '%s' "$err" | sed -n 's/.*--checkpoint \([0-9a-f]*\).*/\1/p')
  must collab layer clone "notes-laptop-$run" --checkpoint "$keep" --repo "$collab"
  must collab workspace open "notes-laptop-$run" --repo "$collab"
  grep -q "late laptop edit" "$collab/.javelin/layers/notes-laptop-$run/$dir/NOTES.md" ||
    fail "the laptop's late edit was lost"
  printf -- '- started on the laptop\n- finished on the desktop\n' >"$moved/$dir/NOTES.md"
  must desktop request revise "$dir-notes" --revision 1 --layer "notes-$run" --operation "$run-d-revise" --repo "$desktop"
  must owner request approve "$dir-notes" --revision 2 --operation "$run-n-approve" --repo "$owner"
  must owner request publish "$dir-notes" --revision 2 --operation "$run-n-publish" --repo "$owner"
  clones+=("desktop $desktop")
else
  skipped+=("layer take (set DESKTOP_TOKEN or DESKTOP_HOME)")
  echo "SKIP: no second device for the collaborator"
fi

bold "11. every clone syncs and holds the node's World, byte for byte"
first=
for clone in "${clones[@]}"; do
  who=${clone%% *} path=${clone#* }
  sync_head "$who" "$path"
  [ -z "$first" ] && first=$id
  [ "$id" = "$first" ] || fail "$who holds $id, not $first"
  must "$who" workspace create "final-$run" --repo "$path"
  diff -r "$owner/.javelin/layers/final-$run" "$path/.javelin/layers/final-$run" ||
    fail "$who's tree differs from the owner's"
done

must owner history --repo "$owner"
bold "PASS"
echo "Every clone holds $first with identical files. Clones: $work"
for item in "${skipped[@]+"${skipped[@]}"}"; do echo "skipped: $item"; done
exit 0

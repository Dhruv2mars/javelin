#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

if [[ -n "${JAVELIN_BIN:-}" ]]; then
  javelin_bin="$JAVELIN_BIN"
else
  cargo install --quiet --root "$test_root/prefix" --path "$project_root"
  javelin_bin="$test_root/prefix/bin/javelin"
fi

mkdir -p "$test_root/fake-bin" "$test_root/world"
cat >"$test_root/fake-bin/git" <<'FAKE_GIT'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${JAVELIN_FAKE_GIT_LOG:?}"
exit 97
FAKE_GIT
chmod +x "$test_root/fake-bin/git"
: >"$test_root/git.log"

export PATH="$test_root/fake-bin:$PATH"
export JAVELIN_FAKE_GIT_LOG="$test_root/git.log"

"$javelin_bin" version
"$javelin_bin" init "$test_root/world"
printf 'base\n' >"$test_root/world/shared.txt"
printf 'export const contract = 1;\n' >"$test_root/world/contract.ts"
"$javelin_bin" --project "$test_root/world" publish --idempotency-key acceptance-base

for layer in alpha beta conflict; do
  "$javelin_bin" --project "$test_root/world" layer create "$layer" --from world >/dev/null
done
alpha_path="$("$javelin_bin" --project "$test_root/world" layer path alpha)"
beta_path="$("$javelin_bin" --project "$test_root/world" layer path beta)"
conflict_path="$("$javelin_bin" --project "$test_root/world" layer path conflict)"
printf 'alpha\n' >"$alpha_path/alpha.txt"
printf 'accepted target\n' >"$alpha_path/shared.txt"
printf 'beta\n' >"$beta_path/beta.txt"
printf 'private conflict\n' >"$conflict_path/shared.txt"
"$javelin_bin" --project "$alpha_path" checkpoint --reason acceptance-alpha
"$javelin_bin" --project "$beta_path" checkpoint --reason acceptance-beta
"$javelin_bin" --project "$conflict_path" checkpoint --reason acceptance-conflict
"$javelin_bin" --project "$test_root/world" publish alpha --idempotency-key acceptance-alpha
"$javelin_bin" --project "$test_root/world" publish beta --idempotency-key acceptance-beta
set +e
"$javelin_bin" --project "$test_root/world" publish conflict --idempotency-key acceptance-conflict
conflict_exit=$?
set -e
test "$conflict_exit" -eq 4
conflict_id="$("$javelin_bin" --project "$test_root/world" conflict list conflict | awk 'NR == 1 { print $1 }')"
test -n "$conflict_id"
"$javelin_bin" --project "$test_root/world" conflict show "$conflict_id" --json
"$javelin_bin" --project "$test_root/world" conflict resolve "$conflict_id" --use private
"$javelin_bin" --project "$test_root/world" publish conflict --idempotency-key acceptance-conflict-resolved

"$javelin_bin" --project "$test_root/world" refresh local
cat >>"$test_root/world/javelin.toml" <<'RULE'

[[verification.rule]]
name = "guard"
command = ["test", "-f", "pass.txt"]
required = true
timeout_seconds = 10
RULE
printf 'pass\n' >"$test_root/world/pass.txt"
"$javelin_bin" --project "$test_root/world" publish --idempotency-key acceptance-policy
"$javelin_bin" --project "$test_root/world" layer create rejected --from world >/dev/null
rejected_path="$("$javelin_bin" --project "$test_root/world" layer path rejected)"
rm "$rejected_path/pass.txt"
printf 'uses changed contract\n' >"$rejected_path/consumer.ts"
set +e
"$javelin_bin" --project "$test_root/world" publish rejected --idempotency-key acceptance-rejected
verification_exit=$?
set -e
test "$verification_exit" -eq 5
printf 'pass\n' >"$rejected_path/pass.txt"
"$javelin_bin" --project "$test_root/world" publish rejected --idempotency-key acceptance-repaired

"$javelin_bin" --project "$test_root/world" layer create feature --from world >/dev/null
"$javelin_bin" --project "$test_root/world" layer create child-api --from layer:feature --target layer:feature >/dev/null
"$javelin_bin" --project "$test_root/world" layer create child-test --from layer:feature --target layer:feature >/dev/null
child_api_path="$("$javelin_bin" --project "$test_root/world" layer path child-api)"
child_test_path="$("$javelin_bin" --project "$test_root/world" layer path child-test)"
printf 'export const api = 2;\n' >"$child_api_path/api.ts"
printf 'export const tested = true;\n' >"$child_test_path/test-support.ts"
"$javelin_bin" --project "$test_root/world" publish child-api --idempotency-key acceptance-child-api
"$javelin_bin" --project "$test_root/world" publish child-test --idempotency-key acceptance-child-test
"$javelin_bin" --project "$test_root/world" publish feature --idempotency-key acceptance-feature

"$javelin_bin" --project "$test_root/world" layer create bad --from world >/dev/null
bad_path="$("$javelin_bin" --project "$test_root/world" layer path bad)"
session="$("$javelin_bin" --project "$test_root/world" provenance begin --layer bad --actor fake-agent --kind agent)"
"$javelin_bin" --project "$test_root/world" provenance event --session "$session" --event-type prompt --payload '{"summary":"discarded experiment"}'
printf 'discard me\n' >"$bad_path/bad.txt"
"$javelin_bin" --project "$bad_path" hook operation-end --session "$session"
"$javelin_bin" --project "$test_root/world" provenance end "$session"
"$javelin_bin" --project "$test_root/world" discard bad
"$javelin_bin" --project "$test_root/world" discarded list --json
"$javelin_bin" --project "$test_root/world" discarded recover bad
test -f "$bad_path/bad.txt"
"$javelin_bin" --project "$test_root/world" layer create throwaway --from world >/dev/null
"$javelin_bin" --project "$test_root/world" discard throwaway
"$javelin_bin" --project "$test_root/world" discarded purge throwaway

feature_path="$("$javelin_bin" --project "$test_root/world" layer path feature)"
printf 'damaged view\n' >"$feature_path/api.ts"
"$javelin_bin" --project "$test_root/world" repair --view feature
test "$(cat "$feature_path/api.ts")" = 'export const api = 2;'

current_before_restore="$("$javelin_bin" --project "$test_root/world" world current --json)"
restore_version="$("$javelin_bin" --project "$test_root/world" world history | tail -n 2 | head -n 1 | cut -f1)"
test -n "$restore_version"
"$javelin_bin" --project "$test_root/world" world restore "$restore_version"
"$javelin_bin" --project "$test_root/world" world current --json
"$javelin_bin" --project "$test_root/world" world history --json
"$javelin_bin" --project "$test_root/world" history --path shared.txt --json
"$javelin_bin" --project "$test_root/world" explain shared.txt --json
"$javelin_bin" --project "$test_root/world" provenance show "$session" --json
"$javelin_bin" --project "$test_root/world" events --since 0 --jsonl
"$javelin_bin" --project "$test_root/world" fsck

test ! -s "$test_root/git.log"
printf 'ACCEPTANCE_RESULT conflict_exit=%s verification_exit=%s git_calls=0 before_restore=%s\n' \
  "$conflict_exit" "$verification_exit" "$current_before_restore"

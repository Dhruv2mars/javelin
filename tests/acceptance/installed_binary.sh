#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

cargo install --quiet --root "$test_root/prefix" --path "$project_root"
javelin_bin="$test_root/prefix/bin/javelin"

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
"$javelin_bin" --project "$test_root/world" layer create alpha
alpha_path="$("$javelin_bin" --project "$test_root/world" layer path alpha)"
printf 'alpha\n' >"$alpha_path/alpha.txt"
"$javelin_bin" --project "$alpha_path" checkpoint --reason acceptance
"$javelin_bin" --project "$test_root/world" publish alpha --idempotency-key acceptance-alpha
"$javelin_bin" --project "$test_root/world" history --json
"$javelin_bin" --project "$test_root/world" world restore v1
"$javelin_bin" --project "$test_root/world" fsck

test ! -s "$test_root/git.log"


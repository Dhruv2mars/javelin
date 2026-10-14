#!/usr/bin/env bash
# Reverts one guard at a time in src/lib.rs and requires the test suite to fail.
# Restores the original source on exit. Run from crates/javelin-collaboration.
set -euo pipefail

src=src/lib.rs
backup=$(mktemp)
cp "$src" "$backup"
trap 'rc=$?; cp "$backup" "$src"; rm -f "$backup"; exit $rc' EXIT

# A mutation counts only if it compiles and then fails at least one test.
if ! cargo test --locked --quiet --no-fail-fast >/dev/null 2>&1; then
  echo "baseline: tests fail before mutation"
  exit 1
fi

survived=0
mutate() {
  local name=$1 expr=$2 out failed
  cp "$backup" "$src"
  perl -0pi -e "$expr" "$src"
  if cmp -s "$backup" "$src"; then
    echo "$name: NOT APPLIED"
    survived=1
  elif ! out=$(cargo test --locked --no-run 2>&1); then
    echo "$name: DOES NOT COMPILE"
    echo "$out" | grep -E '^error' | head -5
    survived=1
  elif out=$(cargo test --locked --no-fail-fast 2>&1); then
    echo "$name: SURVIVED"
    survived=1
  else
    failed=$({ echo "$out" | grep -E '^test .* FAILED$' || true; } | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | paste -sd, -)
    if [ -z "$failed" ]; then
      echo "$name: FAILED WITHOUT A NAMED TEST"
      survived=1
    else
      echo "$name: caught by $failed"
    fi
  fi
}

mutate takeover 's/if self\.author\.as_ref\(\) != Some\(actor\) \{\n\s*return Err\(Refusal::NotAuthor\);\n\s*\}//'
mutate self_approval 's/if self\.author\.as_ref\(\) == Some\(actor\) \{\n\s*return Err\(Refusal::SelfApproval\);\n\s*\}//'
mutate stale_role_cache 's/&& permissions\.has\(p, Role::Review\)//'
mutate replay_ignores_input 's/if prior == command \{/if true {/'
mutate epoch_regression 's/if permissions\.epoch < self\.epoch \{/if false {/'
mutate carry_approvals 's/approvals: Vec::new\(\),\n        \}\);/approvals: self.revisions.last().map(|r| r.approvals.clone()).unwrap_or_default(),\n        });/'
mutate approve_value_only 's/if \*index != current\.index \{\n\s*return Err\(Refusal::StaleRevision \{\n\s*current: current\.index,\n\s*\}\);\n\s*\}\n\s*if current\.revision != \*revision/if current.revision != *revision/'
mutate revise_no_expected 's/if \*expected != current\.index \{/if false {/'
mutate publish_role 's/permissions\.has\(publisher, Role::Maintain\)/permissions.has(publisher, Role::Read)/'
mutate exact_mismatch 's/if current\.revision != \*exact \{/if false {/'
mutate public_reviewer 's/Visibility::Public => granted\.or\(Some\(Role::Read\)\)/Visibility::Public => granted.or(Some(Role::Review))/'
mutate repo_mismatch 's/if permissions\.repository != self\.repository \{/if false {/g'
mutate skip_permission_validation 's/self\.check_permissions\(permissions\)\?;\n        let actor/let actor/'
mutate equality_ignores_base 's/#\[derive\(Clone, Copy, Debug, Eq, PartialEq\)\]\npub struct RevisionRef/#[derive(Clone, Copy, Debug, Eq)]\npub struct RevisionRef/; s/(impl RevisionRef \{)/impl PartialEq for RevisionRef {\n    fn eq(&self, o: &Self) -> bool {\n        self.manifest == o.manifest && self.context == o.context\n    }\n}\n\n$1/'
mutate equality_ignores_context 's/#\[derive\(Clone, Copy, Debug, Eq, PartialEq\)\]\npub struct RevisionRef/#[derive(Clone, Copy, Debug, Eq)]\npub struct RevisionRef/; s/(impl RevisionRef \{)/impl PartialEq for RevisionRef {\n    fn eq(&self, o: &Self) -> bool {\n        self.manifest == o.manifest && self.base == o.base\n    }\n}\n\n$1/'
mutate unchanged_revision_allowed 's/if current\.revision == \*revision \{/if false {/'
mutate publish_state_before_access 's/    let current = review\.current_open\(\)\?;\n    if current\.revision != \*exact \{\n        return Err\(Refusal::RevisionMismatch\);\n    \}\n//; s/(    review\.check_permissions\(permissions\)\?;\n)/$1    let current = review.current_open()?;\n    if current.revision != *exact {\n        return Err(Refusal::RevisionMismatch);\n    }\n/'
mutate replay_before_repository 's/        \/\/ Replays still require permissions for this repository\.\n        if permissions\.repository != self\.repository \{\n            return Err\(Refusal::RepositoryMismatch\);\n        \}\n//'

exit $survived

#!/usr/bin/env bash
# Gate before you push or update a pull request. Runs every test and the lint and doc checks,
# and mutates only the lines that differ from BASE. Time limit: 90 seconds, plus 2.5 seconds
# per mutant on those lines, at most 300 seconds.
# Usage: scripts/pre-pr-test.sh [BASE]   BASE defaults to origin/main.
# DRY_RUN=1 prints the plan without running it.

# shellcheck source=scripts/common.sh
source "$(dirname "$0")/common.sh"

if [[ -z ${DROID_USAGE_GATE_SNAPSHOT:-} && -z ${DRY_RUN:-} ]]; then
    snapshot_gate "$0" "$@"
    exit $?
fi

base=${1:-origin/main}
if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
    echo "pre-pr-test: unknown base $base (run git fetch first)" >&2
    exit 2
fi

# Enforce the time limit by running this same script under timeout. The limit grows with the mutant count.
if [[ -z ${PRE_PR_TEST_TIMED:-} ]]; then
    count=$(mutant_count "$base") || exit 2
    limit=$(time_limit "$count")
    echo "pre-pr-test: $count mutants on the changed lines, time limit ${limit}s"
    PRE_PR_TEST_TIMED=1 timeout --kill-after=10 "$limit" "$0" "$@"
    code=$?
    if ((code == 124 || code == 137)); then
        echo "pre-pr-test: stopped at the ${limit}-second limit before it finished; the gate did not pass" >&2
    fi
    exit "$code"
fi

changed=$(git diff --name-only "$base")

run_step "clean tree" check_clean_tree
run_step "shellcheck" shellcheck scripts/*.sh
run_step "gate self-test" scripts/selftest-gates.sh
run_step "fmt" cargo fmt --check
run_step "clippy" cargo clippy --all-targets --all-features -- -D warnings
run_step "tests" test_env cargo test -q
run_step "test count" check_test_count
run_step "docs" check_docs
if grep -qxE 'Cargo\.toml|Cargo\.lock|deny\.toml' <<<"$changed"; then
    run_step "cargo deny" cargo deny check
fi
if grep -qxE 'src/(prices|multipliers)\.rs' <<<"$changed"; then
    run_step "price keys in newest snapshot" python3 scripts/check-price-keys.py
fi
if grep -qE '^src/.*\.rs$' <<<"$changed"; then
    run_step "mutants on changed lines" changed_lines_mutants "$base"
fi
finish

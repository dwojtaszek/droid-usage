#!/usr/bin/env bash
# Full regression: every check in the Testing section of AGENTS.md, with no time limit and no step skipped.
# Every step runs even after a failure, then the script lists what failed.
# Usage: scripts/full-test.sh   DRY_RUN=1 prints the plan without running it.

# shellcheck source=scripts/common.sh
source "$(dirname "$0")/common.sh"

if [[ -z ${DROID_USAGE_GATE_SNAPSHOT:-} && -z ${DRY_RUN:-} ]]; then
    snapshot_gate "$0" "$@"
    exit $?
fi

miri() {
    MIRIFLAGS=-Zmiri-disable-isolation PROPTEST_CASES=8 \
        test_env cargo +nightly miri test --lib -- --skip opencode --skip agy --skip app:: --skip download::
}

fuzz_target() {
    local target=$1
    mkdir -p "fuzz/corpus/$target" &&
        cargo +nightly fuzz run "$target" "fuzz/corpus/$target" "fuzz/seeds/$target" \
            -- -max_total_time=60 "-dict=fuzz/dictionaries/$target.dict"
}

echo "full-test: HEAD $(git rev-parse --short HEAD), $(git status --porcelain | wc -l) changed paths in the tree"

run_step "shellcheck" shellcheck scripts/*.sh
run_step "gate self-test" scripts/selftest-gates.sh
run_step "fmt" cargo fmt --check
run_step "clippy" cargo clippy --all-targets --all-features -- -D warnings
run_step "tests" test_env cargo test -q
run_step "test count" check_test_count
run_step "docs" check_docs
run_step "cargo deny" cargo deny check
run_step "price keys in newest snapshot" python3 scripts/check-price-keys.py
run_step "miri" miri
run_step "mutants on the whole crate" run_mutants --timeout 180
run_step "fuzz multipliers_page" fuzz_target multipliers_page
run_step "fuzz prices_table" fuzz_target prices_table
run_step "line coverage floor 92%" test_env cargo llvm-cov --summary-only --fail-under-lines 92
finish

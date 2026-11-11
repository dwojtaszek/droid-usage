#!/usr/bin/env bash
# Fast loop while coding. Runs fmt, clippy, and only the tests that cover the files changed against BASE,
# then checks the test-count floor.
# Usage: scripts/dev-test.sh [BASE]   BASE defaults to HEAD, so uncommitted work is what runs.
# DRY_RUN=1 prints the plan without running it.

# shellcheck source=scripts/common.sh
source "$(dirname "$0")/common.sh"

base=${1:-HEAD}
if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
    echo "dev-test: unknown base $base" >&2
    exit 2
fi

changed=$({ git diff --name-only "$base"; git ls-files --others --exclude-standard; } | sort -u)
if [[ -z $changed ]]; then
    echo "dev-test: no changes against $base"
    exit 0
fi
echo "changed against $base:"
while IFS= read -r file; do
    echo "  $file"
done <<<"$changed"

# Map each changed file to the tests that cover it. A file not listed here runs every test.
run_all=0
lib_filters=()
integration_tests=()
while IFS= read -r file; do
    case $file in
        src/main.rs) integration_tests+=(binary) ;;
        src/lib.rs | src/testing.rs | src/*/*) run_all=1 ;;
        src/*.rs) lib_filters+=("$(basename "$file" .rs)::") ;;
        tests/golden/*) integration_tests+=(golden) ;;
        tests/common/*) integration_tests+=(binary golden readers) ;;
        tests/*.rs) integration_tests+=("$(basename "$file" .rs)") ;;
        *.md | docs/* | evals/* | fuzz/* | scripts/* | .factory/* | .github/* | LICENSE | .gitignore) ;;
        *) run_all=1 ;;
    esac
done <<<"$changed"

run_step "shellcheck" shellcheck scripts/*.sh
run_step "gate self-test" env DROID_USAGE_GATE_SNAPSHOT=1 scripts/selftest-gates.sh

rust_changes=$((run_all + ${#lib_filters[@]} + ${#integration_tests[@]}))
if ((rust_changes == 0)); then
    echo "dev-test: no test covers these files"
    finish
    exit 0
fi

run_step "fmt" cargo fmt --check
run_step "clippy" cargo clippy --all-targets --all-features -- -D warnings
if ((run_all)); then
    run_step "tests (all)" test_env cargo test
else
    if ((${#lib_filters[@]})); then
        run_step "unit tests for ${lib_filters[*]}" test_env cargo test --lib -- "${lib_filters[@]}"
    fi
    if ((${#integration_tests[@]})); then
        test_args=()
        while IFS= read -r target; do
            test_args+=(--test "$target")
        done < <(printf '%s\n' "${integration_tests[@]}" | sort -u)
        run_step "integration tests" test_env cargo test "${test_args[@]}"
    fi
fi
run_step "test count" check_test_count
finish

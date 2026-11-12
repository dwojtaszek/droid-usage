#!/usr/bin/env bash
# Shared setup and helpers for scripts/*.sh. Sourced by those scripts, not run on its own.

set -uo pipefail

# Hooks can export repository-local overrides; each script must use its own checkout.
while IFS= read -r gate_git_variable; do
    unset "$gate_git_variable"
done < <(git rev-parse --local-env-vars)

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2
export PATH="$HOME/.cargo/bin:$PATH"

# Lower bound for the number of tests. Raise it when tests are added. Lowering it needs the user's approval.
MIN_TEST_COUNT=183

failed_steps=()

# Keep test output independent of the caller's terminal preferences. Binary tests set their own NO_COLOR.
test_env() (
    unset NO_COLOR
    export TERM=xterm-256color COLORTERM=
    "$@"
)

# Long gates test a committed snapshot so editing the caller's tree cannot change their inputs.
snapshot_gate() (
    check_clean_tree || exit 1
    local gate=$1
    shift
    local revision root scratch gate_status=0
    revision=$(git rev-parse HEAD) || exit 2
    root=$PWD
    scratch=$(mktemp -d) || exit 2

    trap '
        gate_status=$?
        cd "$root" || exit 2
        if ((gate_status != 0)) && [[ -f $scratch/checkout/.git ]]; then
            echo "gate failed: snapshot and diagnostics retained at $scratch/checkout" >&2
            exit "$gate_status"
        fi
        if [[ -f $scratch/checkout/.git ]]; then
            git worktree remove --force "$scratch/checkout" || gate_status=1
        fi
        rm -rf "$scratch"
        exit "$gate_status"
    ' EXIT
    git worktree add --quiet --detach "$scratch/checkout" "$revision" || exit 2
    echo "gate snapshot: $revision"
    DROID_USAGE_GATE_SNAPSHOT=1 CARGO_TARGET_DIR="$scratch/checkout/target" \
        "$scratch/checkout/scripts/$(basename "$gate")" "$@"
)

# run_step NAME COMMAND [ARGS...]: runs one check, records a failure, and keeps going.
# With DRY_RUN=1 it prints the command instead of running it.
run_step() {
    local name=$1
    shift
    echo
    echo "==> $name"
    if [[ -n ${DRY_RUN:-} ]]; then
        echo "    would run: $*"
        return 0
    fi
    local started=$SECONDS
    if "$@"; then
        echo "<== $name: ok ($((SECONDS - started))s)"
    else
        echo "<== $name: FAILED ($((SECONDS - started))s)"
        failed_steps+=("$name")
    fi
}

# finish: prints the verdict and exits 1 when any step failed.
finish() {
    echo
    if ((${#failed_steps[@]})); then
        local IFS=', '
        echo "FAILED: ${failed_steps[*]}"
        exit 1
    fi
    echo "PASSED: every step ran and passed"
}

check_clean_tree() {
    local changes
    changes=$(git status --porcelain)
    if [[ -n $changes ]]; then
        echo "$changes"
        echo "The tree has changes. Commit or stash them: this gate tests what you push."
        return 1
    fi
}

check_docs() {
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
}

check_test_count() {
    local count
    count=$(cargo test -q -- --list 2>/dev/null | grep -c ': test$')
    echo "tests listed: $count (floor: $MIN_TEST_COUNT)"
    ((count >= MIN_TEST_COUNT))
}

# check_missed_mutants: fails on a timed-out mutant, or on a missed mutant that is not a known equivalent.
check_missed_mutants() {
    local status=0 line known
    known=$(grep -v '^#' scripts/known-equivalent-mutants.txt)
    if [[ -s mutants.out/timeout.txt ]]; then
        echo "timed out:"
        cat mutants.out/timeout.txt
        status=1
    fi
    if [[ -s mutants.out/missed.txt ]]; then
        while IFS= read -r line; do
            if grep -Fxq -- "$line" <<<"$known"; then
                echo "known equivalent: $line"
            else
                echo "MISSED: $line"
                status=1
            fi
        done <mutants.out/missed.txt
    fi
    return $status
}

# check_mutants_judged: fails when mutants were found but all were unviable, so the tests judged none of them.
check_mutants_judged() {
    local judged unviable
    judged=$(cat mutants.out/caught.txt mutants.out/missed.txt mutants.out/timeout.txt 2>/dev/null | wc -l)
    unviable=$(cat mutants.out/unviable.txt 2>/dev/null | wc -l)
    if ((judged == 0 && unviable > 0)); then
        echo "no mutant was tested: $unviable found on these lines, and none of them compiles."
        echo "The tests judged none of this code. See mutants.out/unviable.txt and mutants.out/log/."
        return 1
    fi
}

# run_mutants ARGS...: runs cargo-mutants, then judges its misses against the known-equivalent list.
run_mutants() {
    test_env cargo mutants "$@"
    local code=$?
    local status=0
    case $code in
        0) ;;
        2 | 3) check_missed_mutants || status=1 ;; # 2: some mutants were missed, 3: some timed out
        *)
            echo "cargo-mutants stopped with status $code"
            return 1
            ;;
    esac
    check_mutants_judged || status=1
    return $status
}

# mutant_count BASE: prints how many mutants cargo-mutants generates on the lines that differ from BASE.
mutant_count() {
    local base=$1 diff listed status
    diff=$(mktemp) || return 1
    git diff -U0 "$base" -- 'src/*.rs' >"$diff"
    if [[ ! -s $diff ]]; then
        rm -f "$diff"
        echo 0
        return 0
    fi
    listed=$(cargo mutants --list --in-diff "$diff" 2>/dev/null)
    status=$?
    rm -f "$diff"
    if ((status != 0)); then
        echo "could not list the mutants on the changed lines (cargo mutants --list failed)" >&2
        return 1
    fi
    grep -c . <<<"$listed" || true
}

# time_limit COUNT: prints the pre-PR time limit in seconds: 90, plus 2.5 per mutant, at most 300.
time_limit() {
    local limit=$((90 + $1 * 5 / 2))
    ((limit > 300)) && limit=300
    echo "$limit"
}

# changed_lines_mutants BASE: mutation-tests only the lines that differ from BASE.
changed_lines_mutants() {
    local base=$1 diff status
    diff=$(mktemp) || return 1
    git diff -U0 "$base" -- 'src/*.rs' >"$diff"
    run_mutants --in-diff "$diff" --timeout 60
    status=$?
    rm -f "$diff"
    return $status
}

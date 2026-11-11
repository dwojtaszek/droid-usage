#!/usr/bin/env bash
# Checks the gate logic in common.sh against known inputs. It runs in a scratch copy with a stub cargo,
# so the real mutants.out and the real known-equivalent list are never touched.
# Usage: scripts/selftest-gates.sh

set -uo pipefail

# Probe gates create their own snapshots even when this self-test runs inside a long gate.
unset DROID_USAGE_GATE_SNAPSHOT

scratch=$(mktemp -d) || exit 2
trap 'cd / && rm -rf "$scratch"' EXIT
mkdir -p "$scratch/scripts"
cp "$(dirname "$0")/common.sh" "$scratch/scripts/"
cp "$(dirname "$0")/pre-pr-test.sh" "$scratch/scripts/"
cd "$scratch" || exit 2

# Sourcing common.sh cds to the root of the copy, so mutants.out and the known list are scratch files.
# shellcheck source=/dev/null
source scripts/common.sh
printf '%s\n' "src/agy.rs:79:42: replace | with ^ in model_from_db" >scripts/known-equivalent-mutants.txt

failures=()

# check NAME WANT GOT: records a failure when GOT is not WANT.
check() {
    if [[ $2 == "$3" ]]; then
        echo "ok   $1"
    else
        echo "FAIL $1: want $2, got $3"
        failures+=("$1")
    fi
}

# Stands in for cargo. Each case writes the verdict files that cargo-mutants would write, then returns its exit status.
cargo() {
    mkdir -p mutants.out
    : >mutants.out/caught.txt
    : >mutants.out/missed.txt
    : >mutants.out/timeout.txt
    : >mutants.out/unviable.txt
    case $STUB in
        all_unviable)
            echo "src/x.rs:1:1: r" >mutants.out/unviable.txt
            return 0
            ;;
        caught_and_unviable)
            echo "src/x.rs:1:1: r" >mutants.out/caught.txt
            echo "src/y.rs:1:1: r" >mutants.out/unviable.txt
            return 0
            ;;
        nothing_found) return 0 ;;
        known_missed)
            echo "src/agy.rs:79:42: replace | with ^ in model_from_db" >mutants.out/missed.txt
            return 2
            ;;
        unknown_missed)
            echo "src/x.rs:1:1: r" >mutants.out/missed.txt
            return 2
            ;;
        timed_out)
            echo "src/x.rs:1:1: r" >mutants.out/timeout.txt
            return 3
            ;;
        stopped) return 4 ;;
    esac
}

# expect_verdict CASE WANT: runs run_mutants as cargo-mutants reports CASE, then checks its exit status.
expect_verdict() {
    STUB=$1 run_mutants --timeout 60 >/dev/null 2>&1
    check "mutant verdict: $1" "$2" "$?"
}

expect_verdict all_unviable 1
expect_verdict caught_and_unviable 0
expect_verdict nothing_found 0
expect_verdict known_missed 0
expect_verdict unknown_missed 1
expect_verdict timed_out 1
expect_verdict stopped 1

check "time limit, no mutants" 90 "$(time_limit 0)"
check "time limit, 32 mutants" 170 "$(time_limit 32)"
check "time limit, capped" 300 "$(time_limit 100)"

# Color settings belong to the test child, not the calling shell.
(
    export NO_COLOR=1
    test_env printenv NO_COLOR >/dev/null
    [[ $? == 1 && $(test_env printenv TERM) == xterm-256color && $NO_COLOR == 1 ]]
) >/dev/null 2>&1
check "test environment with NO_COLOR set" 0 "$?"
(
    unset NO_COLOR
    test_env printenv NO_COLOR >/dev/null
    [[ $? == 1 && $(test_env printenv TERM) == xterm-256color ]]
) >/dev/null 2>&1
check "test environment with NO_COLOR unset" 0 "$?"

# A tiny committed gate lets us test isolation and cleanup without running Cargo.
cat >scripts/snapshot-probe.sh <<'PROBE'
#!/usr/bin/env bash
# shellcheck source=scripts/common.sh
source "$(dirname "$0")/common.sh"
if [[ -z ${DROID_USAGE_GATE_SNAPSHOT:-} ]]; then
    snapshot_gate "$0" "$@"
    exit $?
fi
[[ $PWD != "$SOURCE_TREE" && $(cat payload) == committed ]] || exit 9
[[ $CARGO_TARGET_DIR == "$PWD/target" ]] || exit 9
printf edited >"$SOURCE_TREE/payload"
[[ $(cat payload) == committed ]] || exit 9
[[ ${1:-} == argument ]] || exit 9
mkdir -p mutants.out/log fuzz/artifacts/probe
printf diagnostics >mutants.out/log/probe.log
printf '\000\377' >fuzz/artifacts/probe/crash
exit "${PROBE_EXIT:-0}"
PROBE
chmod +x scripts/snapshot-probe.sh
printf committed >payload
printf '/mutants.out\n/target\n/foreign\n/fake-home\n/pre-pr.log\n' >.gitignore
printf '#!/usr/bin/env bash\nexit 0\n' >scripts/selftest-gates.sh
chmod +x scripts/selftest-gates.sh
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
git init --quiet .
git add scripts payload .gitignore
git -c user.name=gate-test -c user.email=gate-test@example.invalid commit --quiet -m fixture

SOURCE_TREE=$PWD scripts/snapshot-probe.sh argument >/dev/null 2>&1
check "snapshot stays committed while caller edits" 0 "$?"
check "caller edits remain intact" edited "$(cat payload)"
check "successful snapshot is removed" 1 "$(git worktree list --porcelain | grep -c '^worktree ')"

SOURCE_TREE=$PWD scripts/snapshot-probe.sh argument >/dev/null 2>&1
check "dirty tree is rejected" 1 "$?"
check "dirty rejection preserves caller edits" edited "$(cat payload)"

printf committed >payload
PROBE_EXIT=7 SOURCE_TREE=$PWD scripts/snapshot-probe.sh argument >/dev/null 2>&1
check "snapshot preserves gate exit status" 7 "$?"
check "failed snapshot is retained" 2 "$(git worktree list --porcelain | grep -c '^worktree ')"
retained=$(git worktree list --porcelain | sed -n 's/^worktree //p' | tail -1)
check "mutation diagnostics survive failure" diagnostics "$(cat "$retained/mutants.out/log/probe.log")"
cmp "$retained/fuzz/artifacts/probe/crash" <(printf '\000\377') >/dev/null 2>&1
check "fuzz crash bytes survive failure" 0 "$?"
git worktree remove --force "$retained"
rmdir "$(dirname "$retained")"

# Git overrides must not redirect either the snapshot or scratch-fixture operations.
printf committed >payload
git init --quiet foreign
printf original >foreign/sentinel
git -C foreign add sentinel
git -C foreign -c user.name=gate-test -c user.email=gate-test@example.invalid commit --quiet -m sentinel
foreign_head=$(git -C foreign rev-parse HEAD)
printf staged >foreign/sentinel
git -C foreign add sentinel
GIT_DIR=$PWD/foreign/.git GIT_WORK_TREE=$PWD/foreign GIT_INDEX_FILE=$PWD/foreign/.git/index \
    SOURCE_TREE=$PWD scripts/snapshot-probe.sh argument >/dev/null 2>&1
check "inherited Git overrides cannot escape snapshot" 0 "$?"
check "foreign repository commits remain intact" "$foreign_head" "$(git -C foreign rev-parse HEAD)"
check "foreign staged data remains intact" staged "$(git -C foreign show :sentinel)"

# Exercise the real pre-PR dispatch with a caller-relative upstream and a stub toolchain.
printf committed >payload
git branch baseline HEAD
git branch --set-upstream-to=baseline >/dev/null
mkdir -p fake-home/.cargo/bin
cat >fake-home/.cargo/bin/cargo <<'CARGO'
#!/usr/bin/env bash
if [[ $* == *--list* ]]; then
    for ((i=0; i<GATE_TEST_COUNT; i++)); do
        echo "fixture_$i: test"
    done
fi
exit 0
CARGO
chmod +x fake-home/.cargo/bin/cargo
HOME=$PWD/fake-home GATE_TEST_COUNT=$MIN_TEST_COUNT scripts/pre-pr-test.sh '@{upstream}' >pre-pr.log 2>&1
check "caller-relative base survives snapshot and timeout" 0 "$?"
check "pre-PR snapshot is removed after success" 1 "$(git worktree list --porcelain | grep -c '^worktree ')"

echo
if ((${#failures[@]})); then
    echo "FAILED: ${failures[*]}"
    exit 1
fi
echo "PASSED: every gate check matched"

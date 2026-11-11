# Testing

## Check schedule

Use the repository scripts from the root; their implementations define the exact checks.

| When | Command | Purpose |
| --- | --- | --- |
| After each edit and before handback | `scripts/dev-test.sh [base]` | Fast checks for the working changes; defaults to `HEAD`. |
| During implementation, before handback | Focused checks below | Exercise the changed behavior at the relevant layers. |
| Before push or creating/updating a PR | `scripts/pre-pr-test.sh [base]` | Check a committed revision, including mutations on changed lines; defaults to `origin/main`. |
| Before merging to main, or when explicitly requested | `scripts/full-test.sh` | Full regression: all layers, both fuzz targets, whole-crate mutations, and the coverage floor. |

The full gate is not required for every implementation handback. It remains mandatory before merging. In the 2026-11-11 session, full Miri took 640 seconds and whole-crate mutations took 767 seconds. Focused Codex Miri took 11 seconds. These are observations, not runtime budgets. The pre-PR script computes its own time limit; a timeout is a failed gate, not permission to omit checks. Reduce the change or run the full gate.

The two long gates reject a dirty caller tree, then create a temporary detached worktree at `HEAD`, print its full commit SHA, and run there. Later edits in the caller's checkout cannot enter that run. Each snapshot has its own build directory: sharing Cargo output between checkouts can reuse binaries containing a stale `CARGO_MANIFEST_DIR`. Successful snapshots are removed automatically. Failed snapshots are retained at the printed path so `mutants.out` diagnostics and binary fuzz crash inputs remain recoverable. Keep the command's log outside the worktree too. After preserving needed reproducers, remove a failed worktree with `git worktree remove --force <printed-path>` and remove its empty parent directory. If unrelated changes are pending, use a separate clean worktree containing the intended commits; preserve those pending changes.

Scripts clear Git's repository-local environment overrides so a hook's `GIT_DIR`, `GIT_WORK_TREE`, or alternate index cannot redirect operations outside the intended checkout. The pre-PR base is resolved to a commit in the caller's worktree before snapshot dispatch and timeout re-entry; upstream and reflog-relative expressions retain their meaning.

Snapshot isolation pins tracked source. Toolchains, dependencies, and the host's saved price snapshot remain external inputs. A green gate applies to the printed revision; rerun affected checks after changes.

## Test environment

The scripts use `test_env` from `scripts/common.sh` to remove inherited `NO_COLOR` and set a known terminal for test subprocesses, including mutation tests, Miri, and coverage. This does not change the calling shell or production CLI behavior. Binary tests explicitly set `NO_COLOR` where plain output is part of their contract.

For a focused Cargo check, use the same helper in a subshell. For example:

```bash
(source scripts/common.sh; MIRIFLAGS=-Zmiri-disable-isolation PROPTEST_CASES=8 test_env cargo +nightly miri test --lib -- codex::)
```

Run gate regressions with `scripts/selftest-gates.sh`. They test mutant verdicts, time limits, color isolation with `NO_COLOR` set and unset, snapshot isolation, successful cleanup, retained failure artifacts, exit status, inherited Git overrides, and caller-relative bases. The development and long gates run these regressions too. Run the development gate from both parent environments when changing test-environment handling:

```bash
NO_COLOR=1 scripts/dev-test.sh
env -u NO_COLOR scripts/dev-test.sh
```

## Focused checks by change

- **Library logic (`src/*.rs` except `main.rs`):** add or update a unit test in the same module. Start a bug fix with a regression test that fails before the fix.
- **Parsers (`droid`, `opencode`, `agy`, `codex`, `prices`, `multipliers`):** keep or add a proptest that arbitrary input never panics. For downloaded-text parsers, also fuzz the changed target for 60 seconds:

  ```bash
  target=multipliers_page # or prices_table
  mkdir -p "fuzz/corpus/$target"
  cargo +nightly fuzz run "$target" "fuzz/corpus/$target" "fuzz/seeds/$target" -- -max_total_time=60 "-dict=fuzz/dictionaries/$target.dict"
  ```

  A crash is a bug. Add a failing regression, fix it, and keep the crash input in `fuzz/seeds/<target>/`.
- **Report layout:** extend the report tests for alignment, widths, and stripes, as well as text.
- **Intentional report-output change:** inspect the golden diff. Get user approval before changing `tests/golden/report.txt` or `warnings.txt`, then regenerate using the command in `tests/golden/anonymize.py`.
- **Price or multiplier data:** verify each changed key exists in the newest snapshot under `~/.local/share/droid-usage/prices/` using `scripts/check-price-keys.py`.
- **Pure logic:** run focused Miri with the changed module filter, `MIRIFLAGS=-Zmiri-disable-isolation`, and `PROPTEST_CASES=8`. Isolation must be disabled for file-based tests. SQLite (`opencode`, `agy`, `app`) and sockets (`download`) are excluded from full Miri. Done means no undefined behavior is reported.
- **Changed module logic:** run mutations once for the touched modules, for example `(source scripts/common.sh; run_mutants --file src/codex.rs --timeout 180)`. The configured lint cap lets warning-only mutants build. Do not add `--jobs 4`; it was slower than the default. Every survivor needs a test or an explanation of equivalence in the handback. The authoritative equivalent list is `scripts/known-equivalent-mutants.txt`; add to it only after naming the mutant and explaining why it is equivalent to the user.
- **New Rust logic:** run coverage with `(source scripts/common.sh; test_env cargo llvm-cov --summary-only --fail-under-lines 92)`. Coverage identifies untested lines; mutation results judge whether assertions detect faults.
- **Major visual CLI change:** use `.factory/skills/record-demo/SKILL.md`, inspect preview frames, and include the regenerated `docs/demo.gif` with the change.
- **Dependency change:** run `cargo deny check` for licenses and advisories.
- **Gate scripts:** extend the assert-based shell self-test for changed behavior. Run ShellCheck and the self-test through the development gate. No Rust Miri, fuzz, or mutation run is needed for shell-only changes.

## Fixture and assertion rules

Use inline strings, scratch directories, or anonymized golden fixtures in `tests/golden/home`. Tests never read the real `HOME`, `~/.factory`, or the internet. The loopback server in `src/download.rs` is the exception: it binds only `127.0.0.1` and serves the test's own requests.

Assert the value that matters, not only success. Fix the code under test when a test fails. Never add `#[ignore]` or weaken an assertion to get green; report the failure instead. Changing or removing an existing test requires the user's approval, including golden expectations. Adding tests does not. Record the Rust test count before editing; the final count must be equal or higher. Raise `MIN_TEST_COUNT` in `scripts/common.sh` when adding Rust tests; lowering it requires approval.

Static checks, unit and integration tests, proptests, layout assertions, data checks, Miri, fuzzing, mutation testing, coverage, and dependency audit each catch different faults. A green gate means the checks ran and found nothing; it does not prove correctness. Put an escaped bug's regression at the lowest layer that reproduces it and consider which other layers shared that blind spot. Use both hand-written fixtures and generated inputs.

## Known gaps

- `main.rs` reads the environment and clock. Put new behavior in `app.rs` or a library module. `app::run` accepts a fake download; `tests/binary.rs` exercises the entry point offline.
- Live network sources are not tested. `download` has loopback tests for statuses, truncated bodies, refused connections, and timeouts; `refresh` is tested separately.
- Golden fixtures preserve real log shapes from one anonymized machine, not every possible source format.
- Fuzz targets cover only downloaded `multipliers` and `prices` text. Local readers, including Codex, have proptests.
- No automatic test CI or Git hooks are configured. The scripts and independent review must be invoked by the developer or agent.

Say when a change lands outside these checks' reach; do not claim coverage the checks do not provide.

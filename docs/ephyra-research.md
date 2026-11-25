# Ephyra (`datadog-labs/ephyra`) Research for `droid-usage`

Research compiled on 2026-11-25 against primary sources in [`datadog-labs/ephyra`](https://github.com/datadog-labs/ephyra) (`main` at commit `3983b2e339ac5f6b843bef62c7fa6a88eba75370`, `pushed_at: 2026-11-19T01:06:53Z`), `crates.io` metadata (`2026-11-25`), Datadog's first-party engineering post (`2026-11-24`), and [`arXiv:2611.23282`](https://arxiv.org/abs/2611.23282), evaluated against `droid-usage`'s codebase and test gates.

---

## Recommendation (TL;DR)

**Use Ephyra outside the root `Cargo.toml`—either in a standalone harness crate (mirroring `fuzz/`) or in an isolated worktree during agentic bug-hunting campaigns—and distill confirmed findings back into `droid-usage` as plain deterministic tests, `proptest` strategies, or `fuzz/seeds/`.**

Do **not** add `ephyra` directly to the root `Cargo.toml` `[dev-dependencies]` right now:

1. **Dependency policy conflict (`deny.toml`)**: `droid-usage` enforces `multiple-versions = "deny"` in [`deny.toml`](../deny.toml). Its `Cargo.lock` resolves `proptest 1.12.0` against `rand 0.10.3` / `rand_core 0.10.1`, whereas `ephyra 0.0.1` depends on `rand_core = "0.9"` and `rand_chacha = "0.9"` (plus `object 0.40`, `miniz_oxide 0.9`, and `rustc-demangle 0.1` under `coverage`).
2. **Gate and lint friction**: Root [`Cargo.toml`](../Cargo.toml) denies `warnings`, `missing_docs`, `unused_results`, `as_conversions`, `string_slice`, `cognitive_complexity`, and similar Clippy restriction lints across `--all-targets`; [`scripts/full-test.sh`](../scripts/full-test.sh) runs Miri on `--lib` with `PROPTEST_CASES=8`; and [`scripts/pre-pr-test.sh`](../scripts/pre-pr-test.sh) / [`scripts/full-test.sh`](../scripts/full-test.sh) treat any `cargo-mutants` timeout (`60s` / `180s`) as a failed gate ([`scripts/common.sh`](../scripts/common.sh)). Default `10_000`-case `#[ephyra::test]` runs inside `src/` or `tests/` would conflict with Miri, `cargo-mutants` budgets, and clean-tree snapshot checks unless explicitly gated.
3. **High upside when isolated**: Many of `droid-usage`'s existing `proptest` suites feed unstructured `text in ".*"` into structured Markdown/JSON/JSONL parsers (`multipliers`, `prices`, `codex`, `pi`, `droid`). Ephyra's inductive generator-evolution loop, in-process LLVM coverage tracking, and `ephyra check` shrinker/mutator verifier fit those structured parser and state-machine surfaces well without disturbing existing CI gates.

---

## 1. What `datadog-labs/ephyra` Is

Ephyra is an MIT-licensed, experimental Rust property-based testing (PBT) library and CLI from Datadog Systems Research, described as *"A property-based testing library for the LLM era"* ([`README.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/README.md), [`LICENSE`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/LICENSE)).

### Workspace & Crates.io Status

- **Release state**: Version `0.0.1` across all four workspace crates (git tag `v0.0.1` at `3a50d838248f7ae858e0a4c1127b3ea4f2b938b`; 0 GitHub Releases). All four crates were published to `crates.io` on `2026-10-29`.
- **MSRV & edition**: `edition = "2024"`, `rust-version = "1.88"`, `rust-toolchain.toml` pins `channel = "stable"` ([`Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/Cargo.toml), [`rust-toolchain.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/rust-toolchain.toml)).
- **Workspace members** ([`Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/Cargo.toml)):
  - `ephyra` ([`crates/ephyra/Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/Cargo.toml)): Core library. Direct dependencies: `rand_core = "0.9"`, `rand_chacha = "0.9"`, plus optional `ephyra-macros = "=0.0.1"`, `serde`, `serde_json`, `rustc-demangle = "0.1"`, `miniz_oxide = "0.9"`, `object = "0.40"`. Features: `default = ["json", "derive"]`, `json`, `derive`, `coverage`, `coverage-force`. Uses `#![deny(unsafe_code)]` with one `#[allow(unsafe_code)]` block inside `#[cfg(ephyra_instrumented)]` for LLVM profile runtime FFI ([`lib.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/lib.rs#L165), [`counters.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/coverage/counters.rs#L413-L548)).
  - `ephyra-macros` ([`crates/ephyra-macros/Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-macros/Cargo.toml)): Proc macros (`#[derive(Arbitrary)]`, `#[ephyra::test]`, `#[ephyra::generator]`, `#[ephyra::shrinker]`) using `proc-macro2`, `quote`, `syn` (`3.0`, `full`, `visit`, `visit-mut`). No `unsafe`.
  - `ephyra-cli` ([`crates/ephyra-cli/Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-cli/Cargo.toml)): `ephyra` binary (`test`, `check`, `guide`, `visualize`) using `ephyra = "=0.0.1"`, `syn`, `quote`, `proc-macro2`, `serde`, `serde_json`. No HTTP client dependency (`visualize` uses `std::net::TcpListener`).
  - `ephyra-datadog` ([`crates/ephyra-datadog/Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-datadog/Cargo.toml)): Optional `Sink` for DogStatsD metrics and `ephyra-datadog` dashboard CLI (`cli` feature pulls `ureq = "3"`).
- **Non-workspace directories**: `examples/locate-demo`, `examples/synthesis`, `ci/package-smoke`, and `case-studies/` (29 subdirectories including `sqlite`, `postgresql`, `duckdb`, `datafusion`, `gix`, `brush`, `coreutils`, `cedar`, `litellm`, `lean4`, `ocaml`, `ada`).

### Core Architecture & API

1. **Explicit generators, shrinkers, and mutators on the target type** ([`crates/ephyra/README.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/README.md#L244-L274), [`gens/mod.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/gens/mod.rs#L1659-L1857)):
   - `Source` wraps `ChaCha8Rng`, `size: usize` (`floor(log2(iteration + 1)).min(max_size)`), `filter_max_attempts` (default `100`), and discard state ([`source.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/source.rs)). `Source` draws are strictly uniform; edge-case bias is explicit (`gens::edges`, `any::<T>()`).
   - `Gen` defines `generate(&self, g: &mut Source)`, `shrink(&self, value, visitor) -> ControlFlow<()>`, `shrink_candidates(&self, value) -> Vec<Value>`, and `mutate(&self, value, g: &mut Source)` (defaulting to fresh `generate(g)`). `Trace` adds `steps` and `resume` for prefix-rewinding causality searches.
   - `GenExt` adds `.map`, `.iso` (bijection transporting shrink/mutate), `.and_then`, `.dependent`, `.filter(label, pred)`, `.with_shrink`, `.with_mutate`, `.collect`, `.boxed`. Note `.map` and `.and_then` drop shrinkers/mutators unless `.iso` or `.with_shrink`/`.with_mutate` are supplied.
   - `#[ephyra::shrinker]` compiles a restricted `fn(&T) -> Vec<T>` DSL into lazy `ControlFlow<()>` visitors.
2. **Two-phase runner & programmable loops** ([`runner/mod.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/runner/mod.rs), [`loops.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/loops.rs)):
   - Default `check` flow: `Run::new` (JSON config via `EPHYRA_CONFIG`, `EPHYRA_*` env overrides, coverage lease) -> `remembered()` (replays `EPHYRA_REPLAY` and `.ephyra/regressions/<name>.jsonl`) -> `random_search()` (`10_000` cases by default) -> `targeted_search()` (`256` cases over a `pool_size = 8` seed pool when `target(score)` or coverage novelty is active) -> `finish()` (re-checks for flakiness, shrinks with `Debug`-length monotonicity up to `10_000` steps / `60s`, runs SBFL fault localization, saves regressions, checks health metrics, emits optional JSON / OpenPBTStats JSONL / HTML grid / `.profraw`).
   - `ephyra::loops` exposes `fuzz`, `target`, `perf_fuzz`, `localize`, and `causality` alongside `SeedPool` variants (`Fifo`, `Filo`, `Heap`, `Singleton`, `DoubleQueue`).
   - `Config::failures(n)` (`EPHYRA_FAILURES=n`) + `ephyra::signature(...)` deduplicate failures to surface multiple distinct bugs per run.
3. **In-process LLVM coverage & SBFL without `llvm-cov`/`llvm-profdata`** ([`build.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/build.rs), [`counters.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/coverage/counters.rs), [`mapping.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/coverage/mapping.rs), [`locate.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/coverage/locate.rs)):
   - Activated when `CARGO_ENCODED_RUSTFLAGS` contains `-C instrument-coverage` (or `coverage-force` feature).
   - Reads `__llvm_profile_{begin,end}_{counters,data,names}` in-process and parses covmap/covfun sections from `current_exe()`. Supports `__llvm_profile_data` layouts `(64, 48)` (LLVM 20–22) and `(72, 56)` (LLVM 23 / rustc 1.99+) and covmap/covfun versions 4–7.
   - Stable Rust instruments region/arm counters; nightly adds optional branch coverage (`-Z coverage-options=branch`).
   - Computes per-input AFL-bucketed coverage deltas across selected functions (`EPHYRA_COVERAGE_FILTER`) and runs spectrum-based fault localization (`ochiai`, `tarantula`, `dstar`, `jaccard`, `op2`) using `locate.neighbourhood` (default `64`) mutations around the shrunk input.
4. **CLI & Agent Workflow** ([`STARTUP.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/STARTUP.md), [`crates/ephyra-cli/src/main.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-cli/src/main.rs)):
   - Instead of embedding an LLM client in the crate, Ephyra provides agent documentation (`README.md`, `STARTUP.md`, `API.md`, `ephyra guide`), `ephyra check` (verifies `#[ephyra::generator]` / `#[ephyra::test]` support, shrinker non-emptiness/termination/acyclicity, and mutator validity across 10,000 draws; exits `2` when predicates lack generators), `ephyra test` (wraps `cargo test` with coverage instrumentation and `RUST_TEST_THREADS=1`), and `ephyra visualize` (local web UI on `127.0.0.1:7878`).
   - `STARTUP.md` instructs coding agents to: (1) list 3–5 doc/spec-grounded properties, (2) build valid-by-construction generators + explicit shrinkers, (3) verify them with `ephyra check`, (4) inspect coverage gaps and health warnings (`discard_ratio`, `coverage_plateau`, `low_distinct_inputs`), (5) evolve generators inductively, and (6) capture failing inputs into standalone minimal tests with no Ephyra dependencies and triage verdicts (`Real bug`, `Test bug`, `Intended`, `Environment`).

### Network & Telemetry Audit

Verified across all source files under `crates/` in `datadog-labs/ephyra`:

- **`ephyra` and `ephyra-macros`**: **100% offline**—no HTTP clients, sockets, or telemetry. Disk I/O is limited to `.ephyra/regressions/`, `.ephyra/targets.jsonl`, and configured output artifacts (`EPHYRA_REPORT_JSON`, `EPHYRA_OBSERVATIONS`, `EPHYRA_COVERAGE_HTML`, `EPHYRA_PROFRAW_DIR`).
- **`ephyra-cli`**: **100% offline / loopback-only**. `ephyra visualize` binds `127.0.0.1:7878` via `std::net::TcpListener` ([`serve.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-cli/src/serve.rs#L365)) and serves self-contained HTML ([`serve.html`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-cli/src/serve.html), [`grid.html`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/coverage/grid.html)) with no external CDN or remote URLs.
- **`ephyra-datadog`**: Separate opt-in crate not linked by `ephyra` or `ephyra-cli`; only emits DogStatsD UDP or Datadog HTTPS calls when explicitly installed/invoked.

### Stated Limitations & External Paper/Blog Caveats

- **Experimental status**: `README.md`, `crates/ephyra/README.md`, and `lib.rs` warn the API is unfrozen (`0.0.1`), and the Datadog blog post (`2026-11-24`) states *"Ephyra is experimental and is not a supported Datadog product offering."*
- **No per-test timeout or process isolation**: Infinite loops, stack overflows, `abort`s, and OOMs kill the test process without shrinking or reporting ([`API.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/API.md#L630-L638)).
- **Coverage & mutation constraints**: Per-input counter attribution requires `RUST_TEST_THREADS=1`; an exported `RUSTFLAGS` env var silently overrides `.cargo/config.toml` `[build] rustflags`; mutator failures (`mutate`) lack a `ReplayPoint` and require `EPHYRA_SEED` rather than `EPHYRA_REPLAY`; `ephyra check` validates 10,000 samples (not formal soundness/completeness, [`examples/synthesis/README.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/examples/synthesis/README.md#L33)); and `index.rs` scans ASTs with `syn` without `cfg` evaluation or macro expansion.
- **Methodology caveats from Datadog's primary write-ups** ([Datadog blog, `2026-11-24`](https://www.datadoghq.com/blog/ai/agentic-property-based-testing-with-ephyra/), [`arXiv:2611.23282`](https://arxiv.org/abs/2611.23282)): Reported bug counts (535 bugs across 26 systems in the blog; 349 bugs in the Oct 2026 paper snapshot) were produced with coding agents using classical PBT loops and required human validation/triage before filing; the paper explicitly notes *reward hacking* of generator diversity metrics as an open problem.

---

## 2. Fit Assessment Against `droid-usage`

### High-Value Targets in `droid-usage`

`droid-usage` already achieves strong baseline rigor: `#![forbid(unsafe_code)]`, `proptest = "1.12.0"`, two `cargo-fuzz` targets ([`fuzz/fuzz_targets/`](../fuzz/fuzz_targets/)), `cargo-mutants`, Miri, and a `>= 92%` `cargo-llvm-cov` floor ([`docs/testing.md`](testing.md)). However, inspecting the unit `proptest!` blocks reveals a systematic blind spot: **several parsers only fuzz unstructured `text in ".*"` strings**, which overwhelmingly fail on the first syntax check and rarely exercise deeper state transitions or cross-field invariants:

| Module | Current `proptest` / fuzz coverage | High-leverage Ephyra targets (structured generators + invariants) |
| --- | --- | --- |
| [`src/multipliers.rs`](../src/multipliers.rs) | `parse_page` on `".*"`; single-row regex `rate`; `Snapshot::from_json` round trip (`L760-L801`); `fuzz/fuzz_targets/multipliers_page.rs` | Valid-by-construction Markdown tables with multi-row duplicate display names (`ids_by_name`), `<sup>†</sup>` footnotes, `Promotional pricing.` date/rate cross-checks (`L126-L138`), and `MultiplierBook::multiplier_at` boundary histories across snapshots. |
| [`src/prices.rs`](../src/prices.rs) | `parse_table` on `".*"`; `cost_usd` additivity (`L813-L831`); `fuzz/fuzz_targets/prices_table.rs` | Structured LiteLLM JSON objects (`"mode": "chat" \| "responses"`, mixed `embedding`/missing fields, negative/NaN/Inf rejection, `cache_*` fallback-to-input), `PriceBook::rate_at` bare-name collision (`BTreeMap` lexicographic first `provider/<name>` vs nested `a/b/name`), and multi-snapshot `snapshot_in_effect` lookups. |
| [`src/codex.rs`](../src/codex.rs) | `parse_transcript` on `".*"` (`L301-L306`) | Multi-line JSONL streams interleaving `session_meta`, `turn_context`, `event_msg` (`token_count`), `token_usage_record`, malformed/UTF-8 lines, cumulative replacement vs `saturating_sub(cache_read).saturating_sub(cache_write)`, and latest-model updates (`L44-L96`). |
| [`src/pi.rs`](../src/pi.rs) | `parse_transcript` on `".*"` (`L275-L280`) | Multi-line JSONL streams covering `session`, `model_change`, `message` (`assistant` / `toolResult`), `usage`, `compaction`, `branch_summary`, positive-only `cost.total` filtering (`0.0`/`-1.0` ignored), and `u64` saturation (`L45-L102`). |
| [`src/droid.rs`](../src/droid.rs) | `parse_settings` & `parse_header` on `".*"` (`L216-L222`) | Paired `.settings.json` + `.jsonl` headers (`inclusiveTokenUsage` ignored, early break on first `message` timestamp, first-seen `session_start` `cwd`, skipped malformed lines). |
| [`src/pricing.rs`](../src/pricing.rs), [`src/summary.rs`](../src/summary.rs), [`src/report.rs`](../src/report.rs) | `summary::weekly` conservation/Monday checks (`L321-L399`), `report::shorten` width/identity (`L746-L759`) | End-to-end `price_sessions` -> `weekly` / `group_by` / `ranked` -> `write_report` properties: `-fast`/`-ultrafast` suffix stripping (`list_key`), Droid first-party `exact_rate_at` vs fallback `rate_at`, `unpriced` vs `priced_tokens` accounting, `TOP_PROJECTS` tie-breaking, and 100-column table width invariants under CJK/ZWJ/control-character strings. |
| [`src/files.rs`](../src/files.rs), [`src/snapshots.rs`](../src/snapshots.rs), [`src/refresh.rs`](../src/refresh.rs) | Unit tests on `ScratchDir` | Sequence-based filesystem/state-machine tests (symlink loops, `depth`, atomic `.tmp` cleanup, partial-refresh rollback in `refresh::apply` when `multipliers::save` fails). |

### Friction & Constraints Specific to `droid-usage`

| Constraint in `droid-usage` | Evidence in Repo | Impact with Ephyra |
| --- | --- | --- |
| **Strict `deny.toml` bans** | [`deny.toml`](../deny.toml) sets `[bans] multiple-versions = "deny"` (skipping only `syn@2`/`3` and `windows-sys@0.52`/`0.61`) and `unknown-git = "deny"`. [`Cargo.lock`](../Cargo.lock) pins `proptest 1.12.0` -> `rand 0.10.3` / `rand_core 0.10.1`. | Adding `ephyra = "0.0.1"` (`rand_core = "0.9"`, `rand_chacha = "0.9"`, plus `coverage` deps) directly to root `Cargo.toml` introduces duplicate `rand_core` (`0.9` vs `0.10`) and would fail `cargo deny check` unless `deny.toml` adds exemptions or Ephyra stays in a separate crate. |
| **Crate-level lints on all targets** | [`Cargo.toml`](../Cargo.toml) denies `warnings`, `unsafe_code = "forbid"`, `missing_docs`, `unused_results`, `as_conversions`, `string_slice`, `cognitive_complexity`, etc.; [`clippy.toml`](../clippy.toml) bans `HashMap`/`HashSet`/`chrono::Local`. [`scripts/dev-test.sh`](../scripts/dev-test.sh) runs `cargo clippy --locked --all-targets --all-features -- -D warnings`. | Proc-macro code expanded by `ephyra-macros` inside `droid-usage` test targets is subject to root lints and `disallowed-types`. Isolating Ephyra in its own crate (like `fuzz/`) avoids relaxing root lints. |
| **Miri & `cargo-mutants` gates** | [`scripts/full-test.sh`](../scripts/full-test.sh) runs `MIRIFLAGS=-Zmiri-disable-isolation PROPTEST_CASES=8 cargo +nightly miri test --lib ...` and `run_mutants --timeout 180`; [`scripts/pre-pr-test.sh`](../scripts/pre-pr-test.sh) uses `--timeout 60` (max `300s`); [`scripts/common.sh`](../scripts/common.sh) fails the gate on any timeout. | `ephyra` runs `10_000` cases by default (ignoring `PROPTEST_CASES`) and registers a global panic hook + thread-local context. Putting un-gated `#[ephyra::test]`s in `src/` or `tests/` risks Miri slowdowns/incompatibilities and mutant timeouts. |
| **Clean-tree snapshot gates & CI** | `pre-pr-test.sh` and `full-test.sh` enforce `check_clean_tree`. [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs `cargo test --locked -q` without `ephyra-cli`. | Unignored `.ephyra/regressions/`, `*.profraw`, or `target/cov` artifacts inside the repo will fail `check_clean_tree`. |
| **Toolchain & LLVM compatibility** | Local host runs `rustc 1.100.0` (`LLVM version: 23.1.5`) + `nightly`; `AGENTS.md` forbids network/real-`HOME` access in tests ([`docs/testing.md`](testing.md)). | Exceeds Ephyra's Rust `1.88` MSRV and matches its documented LLVM 23 `(72, 56)` counter layout (not compiled/tested locally yet). Ephyra's core/CLI is 100% offline; `ephyra-datadog` must stay unused. |

---

## 3. Recommended Integration Options

### Option A (Recommended): Standalone Harness Crate (`ephyra-tests/`) + Upstream Distillation

Mirror the repository's existing [`fuzz/`](../fuzz/) pattern:

1. Create an excluded standalone Cargo package (e.g., `ephyra-tests/Cargo.toml`, `publish = false`, `droid-usage = { path = ".." }`, `ephyra = { version = "0.0.1", features = ["coverage"] }`, own `Cargo.lock` and `.gitignore` for `.ephyra/`, `*.profraw`, `target/`).
2. Run Ephyra campaigns manually or via an agent in `ephyra-tests/`:
   - `EPHYRA_COVERAGE_FILTER=droid_usage RUST_TEST_THREADS=1 ephyra check --dir ephyra-tests`
   - `EPHYRA_COVERAGE_FILTER=droid_usage RUST_TEST_THREADS=1 ephyra test --dir ephyra-tests`
   - `EPHYRA_COVERAGE_FILTER=droid_usage ephyra visualize --dir ephyra-tests`
3. Whenever Ephyra uncovers a bug or coverage blind spot, distill the minimized reproducer into a **zero-Ephyra** regression test inside `droid-usage` (`src/*.rs` / `tests/*.rs` / `fuzz/seeds/`) per [`docs/testing.md`](testing.md) and raise `MIN_TEST_COUNT` in [`scripts/common.sh`](../scripts/common.sh).
4. [`scripts/dev-test.sh`](../scripts/dev-test.sh) lists `ephyra-tests/*` beside `fuzz/*` on line 39 so harness edits stay fast and isolated from root `cargo test`; run `cargo fmt --manifest-path ephyra-tests/Cargo.toml --check` and `cargo clippy --manifest-path ephyra-tests/Cargo.toml --locked --all-targets -- -D warnings` directly when editing `ephyra-tests/`. Note also that `ephyra` `0.0.1` uses an unfrozen API and `ephyra-cli` is a local developer prerequisite.

### Option B: Ephemeral Agentic Campaigns (No Committed Ephyra Dependency)

Follow `ephyra`'s [`STARTUP.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/STARTUP.md) inside a temporary `git worktree` (adding `ephyra` only there, running `ephyra check` / `ephyra test`), then cherry-pick **only** the resulting pure-Rust bug fixes, unit tests, `proptest` generators, and `fuzz/seeds/` back to `main`.

- **Pros**: Zero new `Cargo.lock`, zero `deny.toml` / `dev-test.sh` impact, zero dependence on an experimental `0.0.1` crate in the repo.
- **Cons**: Handwritten Ephyra generators are thrown away unless manually rewritten as `proptest` strategies.

### Option C (Not Recommended Now): Root `Cargo.toml` `[dev-dependencies]`

Adding `ephyra` directly to root `Cargo.toml` requires: (1) adding `deny.toml` `multiple-versions` skips (`rand_core@0.9`, `rand_chacha@0.9`, and any `coverage` dependencies), (2) auditing `ephyra-macros` expansions against root restriction lints, (3) wrapping Ephyra tests or lowering case counts under `cfg(miri)` and `cargo-mutants`, and (4) gitignoring `.ephyra/` and `*.profraw`. Given `droid-usage` already runs `proptest`, `cargo-fuzz`, `miri`, `cargo-mutants`, and `cargo-llvm-cov`, that maintenance burden outweighs the benefit.

---

## 4. Verification Scope & Gaps

- **Observed directly in this repository**: `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `deny.toml`, `.github/workflows/ci.yml`, `docs/testing.md`, `scripts/*.sh`, `fuzz/*`, `src/*.rs`, `tests/*.rs`, and `rustc -Vv` (`rustc 1.100.0`, `LLVM version: 23.1.5`).
- **Observed from primary external sources**: `datadog-labs/ephyra` repository files (`3983b2e339ac5f6b843bef62c7fa6a88eba75370`), `crates.io` metadata (`2026-11-25`), Datadog blog (`2026-11-24`), and `arXiv:2611.23282`.
- **Verified live integration (`ephyra-tests/`)**: Installed `ephyra-cli v0.0.1` via `cargo install --locked ephyra-cli`, scaffolded the standalone consumer crate [`ephyra-tests/`](../ephyra-tests/Cargo.toml) (`Option A`), and verified on `rustc 1.100.0` (`LLVM version: 23.1.5`):
  - `cargo clippy --manifest-path ephyra-tests/Cargo.toml --locked --all-targets -- -D warnings` passed under strict `[lints]` (`missing_docs`, `unsafe_code = "forbid"`, `clippy::pedantic`, `as_conversions`).
  - `ephyra check` passed all 11 custom generators/shrinkers (`10,000` draws each).
  - `ephyra test` passed all 13 properties (`130,000` total property executions) with in-process `droid_usage` coverage filtering (`EPHYRA_COVERAGE_FILTER=droid_usage`, `RUST_TEST_THREADS=1`), finding zero bugs in `droid-usage` across the tested parsers, pricing, summary, CLI, and report invariants.
- **Remaining gaps**: `app::run`, `files` directory walking, `refresh::apply`, and `snapshots` filesystem persistence do not yet have Ephyra properties in `ephyra-tests/`; `scripts/pre-pr-test.sh` and `scripts/full-test.sh` were not run (tree has uncommitted new files).

---

## Primary Sources

1. [`datadog-labs/ephyra` (`main` @ `3983b2e339ac5f6b843bef62c7fa6a88eba75370`)](https://github.com/datadog-labs/ephyra/tree/3983b2e339ac5f6b843bef62c7fa6a88eba75370):
   - [`README.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/README.md), [`STARTUP.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/STARTUP.md), [`API.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/API.md), [`LICENSE`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/LICENSE)
   - [`Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/Cargo.toml), [`rust-toolchain.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/rust-toolchain.toml), [`.github/workflows/ci.yml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/.github/workflows/ci.yml)
   - [`crates/ephyra/Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/Cargo.toml), [`crates/ephyra/README.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/README.md), [`crates/ephyra/build.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/build.rs), [`crates/ephyra/src/lib.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/lib.rs)
   - [`crates/ephyra/src/source.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/source.rs), [`crates/ephyra/src/gens/mod.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/gens/mod.rs), [`crates/ephyra/src/runner/mod.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/runner/mod.rs), [`crates/ephyra/src/loops.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/loops.rs), [`crates/ephyra/src/coverage/counters.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/coverage/counters.rs), [`crates/ephyra/src/coverage/mapping.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/coverage/mapping.rs), [`crates/ephyra/src/coverage/locate.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra/src/coverage/locate.rs)
   - [`crates/ephyra-macros/Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-macros/Cargo.toml), [`crates/ephyra-cli/Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-cli/Cargo.toml), [`crates/ephyra-cli/GUIDE.md`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-cli/GUIDE.md), [`crates/ephyra-cli/src/serve.rs`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-cli/src/serve.rs), [`crates/ephyra-datadog/Cargo.toml`](https://github.com/datadog-labs/ephyra/blob/3983b2e339ac5f6b843bef62c7fa6a88eba75370/crates/ephyra-datadog/Cargo.toml)
2. First-party Datadog engineering post: Alp Keles, [*Ephyra: A new agentic property-based testing library to find the bugs you missed*](https://www.datadoghq.com/blog/ai/agentic-property-based-testing-with-ephyra/), Datadog (`2026-11-24`).
3. Research paper: Alperen Keles, [*Ephyra: A Property-Based Testing Framework for the LLM Era*](https://arxiv.org/abs/2611.23282), `arXiv:2611.23282` (`2026-10-23`, to appear in FPBT at POPL '27).

# Project agent rules

Read `/home/dom/.agents/SHARED-AGENT-RULES.md` before coding.

## Scope and Rust

Read [.factory/skills/rust-best-practices/SKILL.md](.factory/skills/rust-best-practices/SKILL.md) before writing, reviewing, or refactoring Rust. This is an application: use concrete types and existing patterns. Library-only builders, mocks, sealed traits, and documentation rules apply only to actual library APIs when requested. Add newtypes only for a real value-mixup risk. Comment why. Keep refactors inside the requested modules and smells; preserve trust-boundary validation and data-loss prevention.

## Development and review

- Before choosing checks, read [docs/testing.md](docs/testing.md). Run `scripts/dev-test.sh` after each edit. Before handback, pass it and the focused checks required by the change.
- Before pushing or creating/updating a PR, run `scripts/pre-pr-test.sh` on committed changes. Before merging to main, run `scripts/full-test.sh`. Both long gates require a clean tree and run in isolated commit snapshots.
- Before handing back code or script changes, and before pushing, delegate an independent diff review using [CODING_STANDARDS.md](CODING_STANDARDS.md). For Rust changes, the reviewer must also read [.factory/skills/rust-review/SKILL.md](.factory/skills/rust-review/SKILL.md). Resolve findings or report remaining limits. If independent review is unavailable, disclose that gap.
- Existing tests, golden expectations, and the test-count floor may be changed or removed only with the user's approval. New tests may be added; raise the floor when adding Rust tests.

## Tool output

Search before reading; request relevant file ranges with bounded output. Discover only the selected tool's schema and return only the response fields needed for the task. Save full check logs outside the repository; show exit status and a short summary or tail. Expand the relevant failure when diagnosis needs it.

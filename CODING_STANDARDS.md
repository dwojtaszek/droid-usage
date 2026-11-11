# Diff review

An independent reviewer reads this file and the diff before handback and before push. Review the changed behavior and connected callers, not unrelated code. For Rust, also read `.factory/skills/rust-review/SKILL.md` and apply the application scope in `AGENTS.md`.

The scripts own mechanical checks. Reuse their recorded results when they cover the final diff; rerun affected checks after edits. The reviewer owns these judgement calls:

- Does the change fully meet the request across callers, configuration, fixtures, and documentation without adding unrequested flexibility?
- Does it fit the surrounding application code and reuse existing patterns? Avoid library machinery unless a real caller needs it.
- Does it preserve trust-boundary validation, error handling, existing user data, and unrelated working changes?
- Do tests assert the behavior that could fail, including relevant boundary and error cases? Would a plausible incorrect implementation still pass?
- Are approximations and known limits accurate and visible where users make decisions? In usage readers, consider repeated events, cache accounting, model switches, and missing metadata.
- Do verification results cover the final source revision? Are skipped checks, surviving mutants, and untested paths disclosed?

Report actionable findings by severity with file and line, consequence, and the smallest fix. A review passes when no unresolved correctness or requested-scope issue remains. Explain accepted limitations and disclose when an independent review was unavailable.

# Sotto task board

Task files are the unit of assignment. One agent per task file. The planner/reviewer
(not the implementer) creates and closes tasks.

## File format

Every task is `Tnnn-slug.md` with:

- `# Title`
- `**Status:**` — one of `todo` | `blocked` | `in-progress` | `in-review` |
  `changes-requested` | `done`
- `## Plan` — the ordered steps the implementer follows

Plus the supporting sections: `Depends on`, `Owns` (file paths this task may write),
`Contract` (what other tasks rely on), `Acceptance`, `Out of scope`.

## The parallelism rule

**An agent may only create or modify files under its `Owns` list.**

This is what makes concurrent work safe. Consequences:

- The root `Cargo.toml` is owned by T001 only. T001 pre-registers *every* workspace
  member up front, including crates that are still empty stubs, so no later task ever
  needs to edit it.
- Each crate declares its own dependencies in its own `crates/<name>/Cargo.toml`.
  Do **not** add to root `[workspace.dependencies]` — that file is frozen after T001.
  Duplicate version drift between crates is the planner's problem to reconcile at
  review time, not a reason to touch a shared file.
- Cross-crate types live in `crates/core` and are frozen by T001. If a task needs a
  type change in `core`, it stops and reports back rather than editing `core`.
  The planner amends T001's contract and notifies affected tasks.

## House lint idiom — read before writing a test

The workspace lint table is **deliberately strict**. Do not weaken it, and do not add
blanket `#[allow]` — `clippy::allow_attributes` is denied, so suppressions must be
`#[expect(..., reason = "...")]`, which self-reports when it goes stale.

Two consequences that will otherwise cost every implementer the same hour:

**There is no warn tier.** `[workspace.lints.rust] warnings = { level = "deny" }` denies
the whole warnings group, so clippy entries written as `"warn"` are emitted as errors —
including `cast_possible_truncation`, `missing_assert_message`, `panic` and
`expect_used`. Treat the entire table as deny. Casts on the audio path (`f32`↔`i16`,
sample counts to `u32`) need explicit handling, not a silent `as`.

**`unwrap()`, `expect()` and `panic!()` are denied in tests too.** `AGENTS.md` permits
them in tests; this workspace does not. The house idiom, verified clean under
`cargo clippy --workspace --all-targets --all-features -- -D warnings`:

```rust
#[test]
fn parses_the_thing() -> Result<(), Box<dyn std::error::Error>> {
    let parsed: u8 = "7".parse()?;
    assert_eq!(parsed, 7, "parse should round-trip");   // message is mandatory
    Ok(())
}
```

`assert!`/`assert_eq!` are fine and do not trip `clippy::panic` — but every assertion
needs a message. Fallible setup uses `?`, not `unwrap()`.

Integration tests under `tests/` additionally trip `tests_outside_test_module`, since
the lint does not special-case integration targets. One inner attribute at the top of
each file, once:

```rust
#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]
```

## Waves

| Wave | Tasks | Can run concurrently |
|---|---|---|
| 0 | T001 | no — blocks everything |
| 1 | T002–T010 | yes — all nine, fully disjoint |
| 2 | T011–T013 | yes, after their listed deps land |

Wave 1 contains the two Phase 0 de-risk spikes (T003, T004). They gate architecture
decisions but do **not** block the other wave-1 crates, which are pure-Rust and
platform-independent. Start the spikes first anyway — they have the longest tail.

## Reporting back

On completion the implementer updates `**Status:** in-review` in its own task file
(that file is always implicitly owned by its assignee) and appends a `## Notes`
section: what was built, what deviated from the plan, what the next task needs to know.
Deviations from `AGENTS.md` decisions need an ADR in `/docs/adr/` — see T001 for the
template.

# Plan: Strengthen Unit Testing Standards

## Context

CUPID cleanup revealed test debt — multi-assert tests, unused rstest dependency, no proptest. The existing `references/testing.md` covers tools but lacks enforceable rules. Goal: codify strict testing discipline so rust-architect can flag violations and code-minion follows TDD.

## Approach

Update the **existing** `.claude/skills/rust-architect/references/testing.md` rather than creating a separate skill. This is where rust-architect already looks for testing guidance (line 23 of SKILL.md). Also add `proptest` as a workspace dependency.

## Changes

### 1. Add `proptest` workspace dependency

**File:** `Cargo.toml` (workspace root)

Add under `[workspace.dependencies]`:
```toml
proptest = "1"
```

No crate needs it in `[dev-dependencies]` yet — that happens when tests are written.

### 2. Rewrite `.claude/skills/rust-architect/references/testing.md`

Keep existing good content (rstest, fixtures, builders, test doubles, TDD) but add these new sections at the top, before everything else:

#### New section: "Unit Test Laws" (top of file, after Core Philosophy)

Five non-negotiable rules:

1. **One reason to fail** — each `#[test]` has exactly one `assert!`/`assert_eq!`/`prop_assert!`. Multiple inputs → `rstest #[case]`. Multiple behaviors → multiple tests.

2. **Never trust a test you haven't seen fail** — TDD red-green-refactor. Write the failing test first. `todo!()` counts as seeing it fail.

3. **Simplest code that could possibly work** — if you need complexity, prove it with a failing test first.

4. **No filesystem in unit tests** — no `std::fs`, `File::open`, `tempfile`, `TempDir`. Use `&[u8]`, `Cursor`, or `Path::new("fake.ext")` for extension checks (no I/O).

5. **Unit tests must be fast** — thousands per second. No I/O, no network, no `thread::sleep`.

#### New section: "Serialization Testing"

- Serialize and deserialize are **separate concerns** — never test as round-trip in example-based tests
- A round-trip test hides symmetric bugs (serialize and deserialize both wrong in the same way)
- Use `proptest` for round-trip **invariants** — that's the one place both appear together, because the property under test IS the invariant

Bad:
```rust
#[test]
fn bpm_serialization() {
    let bpm = Bpm::from_f32(125.45).unwrap();
    let json = serde_json::to_string(&bpm).unwrap();
    assert_eq!(json, "12545");
    let back: Bpm = serde_json::from_str(&json).unwrap();
    assert_eq!(back, bpm);  // hides symmetric bugs
}
```

Good:
```rust
#[test]
fn bpm_serializes_as_hundredths() {
    assert_eq!(serde_json::to_string(&Bpm::from_f32(125.45).unwrap()).unwrap(), "12545");
}

#[test]
fn bpm_deserializes_from_hundredths() {
    assert_eq!(serde_json::from_str::<Bpm>("12545").unwrap().as_f32(), 125.45);
}

proptest! {
    #[test]
    fn bpm_roundtrip(hundredths in 2000u32..=99999u32) {
        let bpm = Bpm::try_from(hundredths).unwrap();
        let json = serde_json::to_string(&bpm).unwrap();
        let back: Bpm = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(bpm, back);
    }
}
```

#### New section: "Review Checklist" (at the bottom)

Actionable checklist for rust-architect during reviews:

- [ ] Each `#[test]` has exactly one assert
- [ ] No multi-assert tests that should be `rstest #[case]`
- [ ] No loops containing assertions — use `#[case]` instead
- [ ] Serialization and deserialization tested separately
- [ ] Round-trip invariants use `proptest`, not example-based tests
- [ ] No filesystem I/O in unit tests
- [ ] No `thread::sleep` or real network calls
- [ ] Test names describe the single behavior being verified
- [ ] New code follows TDD (test written before implementation)

#### Update existing "proptest" section

Change from "Future Exploration / Curious about" to active/mandated for round-trip and invariant testing.

#### Update "Best Practices Summary"

Add the five laws and serialization rule.

### 3. Update `rust-architect/SKILL.md` review section

**File:** `.claude/skills/rust-architect/SKILL.md`

In the "Code Review" section under "For Code Reviews", add a step:
```
5. Check tests against Unit Test Laws (see references/testing.md)
```

## Files to modify

| File | Change |
|------|--------|
| `Cargo.toml` | Add `proptest = "1"` to workspace deps |
| `.claude/skills/rust-architect/references/testing.md` | Add Unit Test Laws, Serialization Testing, Review Checklist; update proptest status |
| `.claude/skills/rust-architect/SKILL.md` | Add test review step |

## Verification

```bash
cargo build  # proptest dep resolves
cargo test   # nothing breaks (no tests use proptest yet)
grep -c "proptest" Cargo.toml  # confirms dependency added
```

Review the updated testing.md to ensure it reads well and the checklist is complete.

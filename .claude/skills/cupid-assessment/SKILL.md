---
name: cupid-assessment
description: Assess code against Dan North's CUPID properties for joyful coding. Use when evaluating code quality, reviewing modules, or deciding where to invest refactoring effort.
---

# CUPID Assessment

Assess code against Dan North's [CUPID properties](https://dannorth.net/blog/cupid-for-joyful-coding/) — five qualities that make code joyful to work with.

CUPID properties are a **direction of travel**, not binary pass/fail. Code is always closer to or further from the ideal.

## The Five Properties

### C — Composable: Plays well with others
- **Small surface area**: Narrow, opinionated APIs (less to learn, less to fail)
- **Intention-revealing**: Easy to discover and assess whether it solves your need
- **Minimal dependencies**: Fewer version conflicts and external constraints

### U — Unix Philosophy: Does one thing well
- **Single purpose** (outside-in): What does this component *do*? Not how is it organized internally
- **Simple, consistent model**: Elegant design that composes naturally with other components
- **Pipeline-friendly**: Outputs become inputs for other components

### P — Predictable: Does what you expect
- **Behaves as expected**: Intended behavior obvious from structure and naming
- **Deterministic**: Same inputs → same results; clear boundaries (memory, time, deps)
- **Observable**: Internal state inferable from outputs (instrumentation, telemetry, logging)
- Three dimensions: *robustness* (breadth), *reliability* (consistency), *resilience* (graceful degradation)

### I — Idiomatic: Feels natural
- **Language idioms**: Follows Rust conventions — ownership, iterators, `Result`/`Option` chains, trait-based design, `clippy` clean
- **Local idioms**: MDMA conventions — type-safe primitives, Action trait pattern, workspace structure (bases/components), conventional commits
- Goes "with the grain" of the language and project

### D — Domain-based: In language and structure
- **Domain language**: Code vocabulary matches the problem space (`Bpm`, `Deck`, `Volume`, `PitchClass` — not generic types)
- **Domain structure**: Directory layout mirrors the problem domain, not framework templates
- **Domain boundaries**: Module boundaries align with domain boundaries

## How to Run an Assessment

### Scope
Assess one component or module at a time. Don't assess the whole workspace at once.

### Rating Scale
For each property, rate the code on a scale:

| Rating | Meaning |
|--------|---------|
| ◉◉◉ | Exemplary — a reference for this property |
| ◉◉○ | Good — clearly moving in the right direction |
| ◉○○ | Fair — some evidence but room to improve |
| ○○○ | Weak — not exhibiting this property |

### Output Format

```
## CUPID Assessment: <component>

| Property | Rating | Summary |
|----------|--------|---------|
| Composable | ◉◉○ | ... |
| Unix philosophy | ◉◉◉ | ... |
| Predictable | ◉○○ | ... |
| Idiomatic | ◉◉○ | ... |
| Domain-based | ◉◉◉ | ... |

### Observations
- **Strengths**: ...
- **Opportunities**: specific, actionable improvements ranked by impact

### Recommendation
One sentence: where to invest next for the biggest joy-of-coding improvement.
```

### Assessment Checklist

For each property, ask:

1. **Composable**: Can I use this component without understanding its internals? Is the API narrow? Could I swap it out?
2. **Unix philosophy**: Can I describe what this does in one sentence without "and"? Does it do that one thing thoroughly?
3. **Predictable**: Can I confidently predict the outcome of calling this? Are edge cases handled or documented? Can I observe what it's doing?
4. **Idiomatic**: Would an experienced Rust developer find this familiar? Does it follow MDMA project conventions?
5. **Domain-based**: Does the vocabulary match the music/DJ/audio domain? Does the structure reflect domain boundaries?

## Mutual Reinforcement

The properties reinforce each other. Improving one typically helps others:
- Domain-based naming makes code more **predictable** (intent is clearer)
- Unix philosophy (single purpose) makes code more **composable**
- Idiomatic code is more **predictable** to experienced developers
- Composable code with small surface area is easier to keep **domain-based**

## When to Use

- Code reviews (dispatch rust-architect with CUPID lens)
- Before refactoring — identify which property to improve for highest impact
- After implementing a new component — sanity check
- Periodic codebase health checks

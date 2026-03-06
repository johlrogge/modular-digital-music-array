# Testing in Rust

## Core Philosophy

**Test behaviors, not conformance.** The type system eliminates illegal states and guarantees correctness. Tests verify that correct behaviors emerge from correct states.

## Unit Test Laws

Five non-negotiable rules:

1. **One reason to fail** — each `#[test]` has exactly one `assert!`/`assert_eq!`/`prop_assert!`. Multiple inputs → `rstest #[case]`. Multiple behaviors → multiple tests.

2. **Never trust a test you haven't seen fail** — TDD red-green-refactor. Write the failing test first. `todo!()` counts as seeing it fail.

3. **Simplest code that could possibly work** — if you need complexity, prove it with a failing test first.

4. **No filesystem in unit tests** — no `std::fs`, `File::open`, `tempfile`, `TempDir`. Use `&[u8]`, `Cursor`, or `Path::new("fake.ext")` for extension checks (no I/O).

5. **Unit tests must be fast** — thousands per second. No I/O, no network, no `thread::sleep`.

## Test Grouping

Two tools for grouping tests:

- **rstest `#[case]`** — when multiple tests share the same assertion pattern with different inputs. This is the primary tool for parameterization.
- **submodules** — when breaking up a test that covers multiple *distinct behaviors* of the same subject that cannot be parameterized. Group related single-assert tests under a descriptive submodule.

Submodules are valid for *topic* grouping (different tests, same subject area). rstest replaces *scenario* modules (same test, different inputs) — not topic grouping.

Example of submodule grouping for a state machine:

```rust
#[cfg(test)]
mod tests {
    mod play_queue {
        #[test]
        fn from_idle_state_becomes_playing() { /* ... */ }

        #[test]
        fn from_idle_emits_load_and_play() { /* ... */ }

        #[test]
        fn from_playing_stops_current_then_loads_new() { /* ... */ }
    }

    mod skip {
        #[test]
        fn with_next_track_advances_queue() { /* ... */ }

        #[test]
        fn on_empty_queue_transitions_to_idle() { /* ... */ }
    }
}
```

## Test Naming

### Rule 1: DRY — don't repeat context

- No `test_` prefix on functions inside `mod tests`
- No repeating the submodule name in the function name
- No repeating the type name in associated test methods
- Rename `foo_tests` submodules to just `foo`

```rust
// Bad
mod tests { fn test_playlist_create() { ... } }
mod volume { fn volume_converts_to_linear() { ... } }

// Good
mod tests { fn playlist_create() { ... } }
mod volume { fn converts_to_linear() { ... } }
```

### Rule 2: Name describes the outcome/behavior being verified

The test name states *what should happen*, not *what action is performed*. The submodule provides the action context; the function name states the expected outcome.

```rust
mod tests {
    mod add_track {
        #[test]
        fn does_not_allow_adding_the_same_track_twice() {
            add_track("track1").expect("first add should succeed");
            assert_eq!(add_track("track1"), Err(DuplicateTrack));
        }
    }
}
```

## Assertion Rules

### Use `pretty_assertions`

Add `use pretty_assertions::assert_eq;` in test modules. It produces structured diffs on failure instead of raw Debug output, making it far easier to spot what differs.

### Avoid bare `assert!()`

Prefer `assert_eq!` with an explicit expected value so failures show what was actually received.

### Test collection equality, not length

```rust
// Bad
assert!(my_vec.is_empty());
// Good
assert_eq!(my_vec, vec![]);

// Bad
assert_eq!(results.len(), 2);
// Good
assert_eq!(results, vec![expected_a, expected_b]);
```

### Multiple asserts decision tree

When you see a test with multiple asserts, apply this decision tree:

1. **Function called multiple times with different inputs, output asserted each time** → convert to `rstest #[case]`. Each input/output pair is a separate case.

2. **Function called once, multiple asserts on the output:**
   - **All fields asserted** → replace with a single `assert_eq!` comparing the whole value against an expected value (use a builder if the struct is large)
   - **Only some fields asserted** → keep as selective field assertions, but verify they check the *relevant* fields. This is the one exception to "one assert per test."

```rust
// Pattern 1: multiple inputs → rstest (NOT multiple asserts in one test)
#[rstest]
#[case("track.flac", true)]
#[case("track.mp3", true)]
#[case("cover.jpg", false)]
fn audio_extension_recognition(#[case] filename: &str, #[case] expected: bool) {
    assert_eq!(is_audio_file(Path::new(filename)), expected);
}

// Pattern 2a: all fields → single assert_eq!
#[test]
fn returns_correct_config() {
    assert_eq!(build_config(), Config { host: "localhost", port: 8080 });
}

// Pattern 2b: selective fields on one value — acceptable exception
#[test]
fn returns_the_matching_track() {
    let track = library.search("Init").first();
    assert_eq!(track.artist, "Carbon Based Lifeforms");
    assert_eq!(track.title, "Init");
    // duration, bpm, key — don't care, don't assert
}
```

If you find yourself writing eight field assertions selectively, ask: does this struct need to be this large? Needing many selective field assertions is a design smell.

## Serialization Testing

- Serialize and deserialize are **separate concerns** — never test as round-trip in example-based tests
- A round-trip test hides symmetric bugs (serialize and deserialize both wrong in the same way)
- Use `proptest` for round-trip **invariants** — that's the one place both appear together

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

## Testing Tools

### rstest - Parametric Testing

Primary testing tool for parameterized tests and fixtures.

```rust
use rstest::*;

#[rstest]
#[case(1, 2, 3)]
#[case(5, 5, 10)]
#[case(0, 100, 100)]
fn addition(#[case] a: i32, #[case] b: i32, #[case] expected: i32) {
    assert_eq!(a + b, expected);
}
```

**Replaces scenario modules**: Instead of creating submodules for different scenarios of the *same test with different inputs*, use rstest with multiple cases. Submodules remain valid for topic grouping of *different tests on the same subject*.

#### Before (scenario submodule — replaced by rstest):
```rust
#[cfg(test)]
mod tests {
    mod when_user_exists {
        #[test]
        fn returns_user() { /* ... */ }
    }

    mod when_user_missing {
        #[test]
        fn returns_none() { /* ... */ }
    }
}
```

#### After (rstest pattern):
```rust
#[cfg(test)]
mod tests {
    use rstest::*;

    #[rstest]
    #[case::user_exists(UserId(1), Some(user))]
    #[case::user_missing(UserId(999), None)]
    fn get_user(#[case] id: UserId, #[case] expected: Option<User>) {
        let result = get_user(id);
        assert_eq!(result, expected);
    }
}
```

#### Fixtures with rstest
```rust
#[fixture]
fn sample_user() -> User {
    User {
        id: UserId(1),
        email: "test@example.com".into(),
    }
}

#[rstest]
fn user_has_expected_id(sample_user: User) {
    assert_eq!(sample_user.id, UserId(1));
}
```

### Property-Based Testing with proptest

**Mandated** for round-trip invariants and property verification across the input space. Use `proptest` whenever you need to assert that a property holds for all valid inputs — not just the examples you thought of.

Use proptest for:
- Round-trip invariants (serialize → deserialize → same value)
- Algebraic properties (commutativity, associativity, idempotency)
- Finding edge cases automatically
- Verifying correctness across the full valid input domain

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn reversing_twice_is_identity(s: String) {
        let reversed_twice = s.chars().rev().collect::<String>()
            .chars().rev().collect::<String>();
        prop_assert_eq!(s, reversed_twice);
    }
}
```

**Rule**: Any type that implements `Serialize + Deserialize` must have a proptest round-trip test. Example-based tests cover the specific known values; proptest covers the invariant.

### Benchmarking with Criterion

**When to benchmark:**
- Testing algorithm performance
- Evaluating caching strategies
- Comparing implementation approaches
- Optimizing hot paths

**Reminder**: Consider benchmarking for algorithms and caching!

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn fibonacci_benchmark(c: &mut Criterion) {
    c.bench_function("fib 20", |b| {
        b.iter(|| fibonacci(black_box(20)))
    });
}

criterion_group!(benches, fibonacci_benchmark);
criterion_main!(benches);
```

### Async Testing with tokio-test

```rust
use tokio::test;

#[tokio::test]
async fn async_operation_succeeds() {
    let result = fetch_data().await;
    assert!(result.is_ok());
}
```

## Test Organization

### Module-Level Testing

Tests live in a `tests` module within the same file:

```rust
// src/user.rs
pub struct User {
    id: UserId,
    email: String,
}

impl User {
    pub fn new(id: UserId, email: String) -> Self {
        Self { id, email }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::*;

    #[rstest]
    #[case(UserId(1), "test@example.com")]
    fn user_creation(#[case] id: UserId, #[case] email: &str) {
        let user = User::new(id, email.into());
        assert_eq!(user.id, id);
        assert_eq!(user.email, email);
    }
}
```

**Access to private items**: Tests can access private module items because they're in the same module.

```rust
fn internal_helper(x: i32) -> i32 {
    x * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_helper_doubles_input() {
        assert_eq!(internal_helper(5), 10);
    }
}
```

### Exposing Internals for Testing (Use Sparingly)

**Considered a code smell** but sometimes necessary:

```rust
// Only expose in test builds
#[cfg(test)]
pub fn internal_function() -> i32 {
    42
}

#[cfg(not(test))]
fn internal_function() -> i32 {
    42
}
```

**Prefer**: Keeping tests in the same module to avoid needing `#[cfg(test)]` exposure.

## Builder Pattern for Test Fixtures (Object Mother)

Builders serve double duty: they set up test state AND construct expected values for `assert_eq!`. When you need to express "I only care about these fields," builders let you declare what matters without spelling out every irrelevant default.

```rust
assert_eq!(
    library.get_track(hash),
    TrackBuilder::new().artist("Carbon Based Lifeforms").title("Init").build()
);
```

Without builders, `assert_eq!` on structs forces you to spell out every field, burying the test's intent. With builders, the test declares what matters.

### Basic Test Builder

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct UserBuilder {
        id: UserId,
        email: String,
        verified: bool,
        created_at: DateTime<Utc>,
    }

    impl UserBuilder {
        fn new() -> Self {
            Self {
                id: UserId(1),
                email: "test@example.com".into(),
                verified: true,
                created_at: Utc::now(),
            }
        }

        fn with_id(mut self, id: UserId) -> Self {
            self.id = id;
            self
        }

        fn with_email(mut self, email: impl Into<String>) -> Self {
            self.email = email.into();
            self
        }

        fn unverified(mut self) -> Self {
            self.verified = false;
            self
        }

        fn build(self) -> User {
            User {
                id: self.id,
                email: self.email,
                verified: self.verified,
                created_at: self.created_at,
            }
        }
    }

    #[rstest]
    #[case::verified_user(UserBuilder::new().build())]
    #[case::unverified_user(UserBuilder::new().unverified().build())]
    fn user_scenarios(#[case] user: User) {
        // Test with different user configurations
    }

    #[test]
    fn specific_email_is_stored() {
        let user = UserBuilder::new()
            .with_email("specific@example.com")
            .build();

        assert_eq!(user.email, "specific@example.com");
    }
}
```

### Composable Closure-Based Builder

For test fixtures that combine multiple nested objects, the closure pattern composes cleanly and reads like a declarative description of the test state:

```rust
struct TestBuilder<S>(S);

impl TestBuilder<TestLibrary> {
    fn build(setup: impl FnOnce(Self) -> Self) -> TestLibrary {
        let TestBuilder(library) = setup(Self(TestLibrary::default()));
        library
    }

    fn track(self, tb: impl FnOnce(TestBuilder<&mut Track>) -> TestBuilder<&mut Track>) -> Self {
        let mut library = self.0;
        let track = library.add_default_track();
        tb(TestBuilder(track));
        Self(library)
    }
}

impl TestBuilder<&mut Track> {
    fn artist(self, name: &str) -> Self { self.0.artist = name.into(); self }
    fn title(self, name: &str) -> Self { self.0.title = name.into(); self }
    fn bpm(self, bpm: f32) -> Self { self.0.bpm = Some(Bpm::from_f32(bpm).unwrap()); self }
}

// Usage in tests:
let library = TestBuilder::build(|lib| lib
    .track(|t| t.artist("Carbon Based Lifeforms").title("Init").bpm(128.0))
    .track(|t| t.artist("Sunju Hargun").title("Silverhaze"))
);
```

### Multiple Builders for Different Scenarios

```rust
#[cfg(test)]
mod tests {
    fn user() -> UserBuilder {
        UserBuilder::new()
    }

    fn admin_user() -> UserBuilder {
        UserBuilder::new()
            .with_role(Role::Admin)
            .with_permissions(all_permissions())
    }

    fn suspended_user() -> UserBuilder {
        UserBuilder::new()
            .suspended()
            .with_suspension_reason("Terms violation")
    }

    #[test]
    fn admins_can_delete_users() {
        let admin = admin_user().build();
        let target = user().build();

        assert!(admin.can_delete(&target));
    }

    #[test]
    fn suspended_users_cannot_login() {
        let user = suspended_user().build();

        assert!(user.login().is_err());
    }
}
```

### Builder + Typestate for Complex Setup

When test setup has required steps, combine builder with typestate:

```rust
#[cfg(test)]
mod tests {
    struct NoDatabase;
    struct WithDatabase;

    struct TestEnvironmentBuilder<D> {
        database: D,
        port: u16,
        log_level: LogLevel,
    }

    impl TestEnvironmentBuilder<NoDatabase> {
        fn new() -> Self {
            Self {
                database: NoDatabase,
                port: 0,  // Random port
                log_level: LogLevel::Error,
            }
        }

        fn with_database(self, db: TestDb) -> TestEnvironmentBuilder<WithDatabase> {
            TestEnvironmentBuilder {
                database: WithDatabase(db),
                port: self.port,
                log_level: self.log_level,
            }
        }
    }

    impl<D> TestEnvironmentBuilder<D> {
        fn with_port(mut self, port: u16) -> Self {
            self.port = port;
            self
        }

        fn with_log_level(mut self, level: LogLevel) -> Self {
            self.log_level = level;
            self
        }
    }

    // Can only build with database set
    impl TestEnvironmentBuilder<WithDatabase> {
        fn build(self) -> TestEnvironment {
            TestEnvironment {
                database: self.database.0,
                port: self.port,
                log_level: self.log_level,
            }
        }
    }

    #[test]
    fn environment_starts_with_correct_port() {
        let env = TestEnvironmentBuilder::new()
            .with_database(TestDb::in_memory())
            .with_port(8080)
            .build();

        assert_eq!(env.port, 8080);
    }
}
```

### Builders with rstest Fixtures

Combine builders with rstest fixtures for reusable test data:

```rust
#[cfg(test)]
mod tests {
    use rstest::*;

    #[fixture]
    fn test_user() -> User {
        UserBuilder::new().build()
    }

    #[fixture]
    fn admin_user() -> User {
        UserBuilder::new()
            .with_role(Role::Admin)
            .build()
    }

    #[fixture]
    fn test_db() -> TestDatabase {
        DatabaseBuilder::new()
            .in_memory()
            .with_schema()
            .build()
    }

    #[rstest]
    fn saved_user_is_retrievable_by_email(test_db: TestDatabase) {
        let user = UserBuilder::new()
            .with_email("new@example.com")
            .build();

        test_db.save_user(&user).unwrap();

        let retrieved = test_db.get_user(user.id).unwrap();
        assert_eq!(retrieved.email, "new@example.com");
    }

    #[rstest]
    fn admins_have_all_permissions(admin_user: User) {
        assert!(admin_user.has_permission(Permission::DeleteUser));
        assert!(admin_user.has_permission(Permission::ManageRoles));
    }
}
```

### Benefits

1. **Sensible defaults** - Most tests use standard configuration
2. **Easy customization** - Override only what matters for specific test
3. **Readable tests** - Intent is clear from builder chain
4. **DRY** - Reuse builders across tests
5. **Type safety** - When combined with typestate, ensures valid setup
6. **Expressive assertions** - Build expected values with the same builders used for setup

## Test Doubles: Terminology and Usage

### Precise Terminology (Freeman & Pryce)

**Mock** (NOT used in Rust):
- A test double that verifies its interactions
- Used in interaction-based testing
- Common in OO languages
- **Rarely appropriate in Rust**

**Stub**:
- Minimal test double for compilation
- Provides no-op or default implementations
- Just enough to not crash

```rust
struct StubDatabase;

impl Database for StubDatabase {
    fn get_user(&self, _id: UserId) -> Option<User> {
        None  // Minimal implementation
    }

    fn save_user(&self, _user: User) -> Result<(), Error> {
        Ok(())  // No-op
    }
}
```

**Simulator**:
- Simulates the real system
- Removes external dependencies (disk, network)
- Contains realistic logic

```rust
struct SimulatedDatabase {
    users: HashMap<UserId, User>,
}

impl Database for SimulatedDatabase {
    fn get_user(&self, id: UserId) -> Option<User> {
        self.users.get(&id).cloned()
    }

    fn save_user(&mut self, user: User) -> Result<(), Error> {
        self.users.insert(user.id, user);
        Ok(())
    }
}

impl SimulatedDatabase {
    fn new() -> Self {
        Self {
            users: HashMap::new(),
        }
    }

    fn with_users(users: Vec<User>) -> Self {
        Self {
            users: users.into_iter()
                .map(|u| (u.id, u))
                .collect(),
        }
    }
}
```

### Creating Test Doubles Manually

Prefer manual test doubles over mocking frameworks:

```rust
trait PaymentProcessor {
    fn charge(&self, amount: Money) -> Result<Transaction, PaymentError>;
}

// Production implementation
struct StripeProcessor {
    api_key: String,
}

// Test simulator
struct TestPaymentProcessor {
    should_fail: bool,
    transactions: RefCell<Vec<Transaction>>,
}

impl TestPaymentProcessor {
    fn new() -> Self {
        Self {
            should_fail: false,
            transactions: RefCell::new(vec![]),
        }
    }

    fn failing() -> Self {
        Self {
            should_fail: true,
            transactions: RefCell::new(vec![]),
        }
    }

    fn transactions(&self) -> Vec<Transaction> {
        self.transactions.borrow().clone()
    }
}

impl PaymentProcessor for TestPaymentProcessor {
    fn charge(&self, amount: Money) -> Result<Transaction, PaymentError> {
        if self.should_fail {
            return Err(PaymentError::ProcessingFailed);
        }

        let tx = Transaction {
            id: TransactionId::new(),
            amount,
            status: TransactionStatus::Success,
        };

        self.transactions.borrow_mut().push(tx.clone());
        Ok(tx)
    }
}
```

## Test-Driven Development (TDD) Approach

### Write Tests First

Use `todo!()` liberally during development:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rstest::*;

    #[rstest]
    #[case(vec![1, 2, 3], 6)]
    #[case(vec![10, 20], 30)]
    fn sum_of_inputs(#[case] input: Vec<i32>, #[case] expected: i32) {
        assert_eq!(sum(&input), expected);
    }
}

pub fn sum(numbers: &[i32]) -> i32 {
    todo!("implement sum")
}
```

### Debug-Only todo!() Macro (Planned)

Create alternative to `todo!()` that only compiles in debug mode:

```rust
#[macro_export]
macro_rules! dev_todo {
    () => {
        #[cfg(debug_assertions)]
        { todo!() }

        #[cfg(not(debug_assertions))]
        compile_error!("dev_todo!() must be implemented before release build")
    };
    ($msg:expr) => {
        #[cfg(debug_assertions)]
        { todo!($msg) }

        #[cfg(not(debug_assertions))]
        compile_error!(concat!("dev_todo!() must be implemented: ", $msg))
    };
}
```

**Benefit**: Catches incomplete implementations at release build time.

## Testing Philosophy

### What to Test

**Test behaviors at module boundaries:**
```rust
// Good: Test public API behavior
#[test]
fn user_service_creates_user() {
    let service = UserService::new(test_db());
    let result = service.create_user("test@example.com");
    assert!(result.is_ok());
}
```

**Don't test implementation details:**
```rust
// Bad: Testing internal state representation
#[test]
fn internal_cache_uses_hashmap() {
    let service = UserService::new(test_db());
    // Asserting on internal HashMap structure
}
```

### Types Eliminate Tests

When types guarantee correctness, tests become unnecessary:

```rust
// No need to test "can't have negative count"
struct Count(NonZeroUsize);

// No need to test "can't have unverified email without email"
enum EmailState {
    None,
    Unverified(String),
    Verified(String),
}
```

**Guideline**: If the type system prevents it, don't test it.

### Testing vs Type Guarantees

Balance type-level guarantees with test coverage:

- **Type system**: Prevents invalid states
- **Tests**: Verify correct behavior given valid states

```rust
struct Age(u8);  // 0-255 range

impl Age {
    pub fn new(years: u8) -> Result<Self, AgeError> {
        if years > 150 {
            Err(AgeError::Unrealistic)
        } else {
            Ok(Age(years))
        }
    }
}

#[cfg(test)]
mod tests {
    #[rstest]
    #[case(0, true)]
    #[case(50, true)]
    #[case(150, true)]
    #[case(151, false)]
    fn age_validation(#[case] years: u8, #[case] valid: bool) {
        assert_eq!(Age::new(years).is_ok(), valid);
    }
}
```

## Doctests

**Limited use**: Primarily for examples in documentation.

```rust
/// Calculates the sum of two numbers.
///
/// # Examples
///
/// ```
/// use mylib::add;
/// assert_eq!(add(2, 3), 5);
/// ```
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
```

**Challenge**: Hard to maintain as code evolves.

**Guideline**: Use for simple examples, not comprehensive testing.

## Integration Tests

Place in `tests/` directory for testing public API:

```
my-crate/
├── src/
│   ├── lib.rs
│   └── user.rs
└── tests/
    ├── user_integration.rs
    └── auth_integration.rs
```

```rust
// tests/user_integration.rs
use my_crate::UserService;

#[test]
fn end_to_end_user_workflow() {
    let service = UserService::new();
    let user = service.create_user("test@example.com").unwrap();
    let fetched = service.get_user(user.id).unwrap();
    assert_eq!(user.email, fetched.email);
}
```

## Best Practices Summary

1. **Use rstest** for parameterized tests and fixtures
2. **Test behaviors**, not implementation details
3. **Let types prevent invalid states**, don't test what types guarantee
4. **Write tests first**, use `todo!()` liberally
5. **Manual test doubles** with precise terminology (stub vs simulator)
6. **Test at module level** to access private items
7. **Consider benchmarks** for algorithms and caching strategies
8. **Doctests for examples only**, not comprehensive coverage
9. **Avoid `#[cfg(test)]` exposure** when possible (code smell)
10. **Use proptest** for round-trip invariants and property verification — it is mandated for any type implementing `Serialize + Deserialize`
11. **Use `pretty_assertions`** in test modules for clear diffs on failure
12. **Use builders** to construct both test fixtures and expected values

## Review Checklist

- [ ] Each `#[test]` has exactly one assert
- [ ] No multi-assert tests where function is called multiple times — use `rstest #[case]`
- [ ] No loops containing assertions — use `#[case]` instead
- [ ] Multi-assert on single return value: whole-value `assert_eq!` or justified selective fields
- [ ] Serialization and deserialization tested separately
- [ ] Round-trip invariants use `proptest`, not example-based tests
- [ ] No filesystem I/O in unit tests
- [ ] No `thread::sleep` or real network calls
- [ ] Test names describe the single behavior being verified
- [ ] New code follows TDD (test written before implementation)
- [ ] No `test_` prefix on functions inside `mod tests`
- [ ] Function names do not repeat their enclosing module or type name
- [ ] Test names describe the outcome/behavior, not the action
- [ ] Related tests grouped in submodules when not parameterizable by rstest
- [ ] Uses `pretty_assertions::assert_eq!` in test modules
- [ ] No bare `assert!()` when `assert_eq!` with expected value is possible
- [ ] Collection assertions compare full contents, not `.len()` or `.is_empty()`

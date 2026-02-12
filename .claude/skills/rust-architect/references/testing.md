# Testing in Rust

## Core Philosophy

**Test behaviors, not conformance.** The type system eliminates illegal states and guarantees correctness. Tests verify that correct behaviors emerge from correct states.

## Testing Tools

### rstest - Parametric Testing

Primary testing tool for parameterized tests and fixtures.

```rust
use rstest::*;

#[rstest]
#[case(1, 2, 3)]
#[case(5, 5, 10)]
#[case(0, 100, 100)]
fn test_addition(#[case] a: i32, #[case] b: i32, #[case] expected: i32) {
    assert_eq!(a + b, expected);
}
```

**Replaces scenario modules**: Instead of creating submodules for different scenarios, use rstest with multiple cases.

#### Before (submodule pattern):
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
    fn test_get_user(#[case] id: UserId, #[case] expected: Option<User>) {
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
fn test_user_operations(sample_user: User) {
    assert_eq!(sample_user.id, UserId(1));
}
```

### Property-Based Testing (Future Exploration)

Consider **proptest** or **quickcheck** for:
- Testing invariants across input space
- Finding edge cases automatically
- Verifying algebraic properties

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_reversing_twice_is_identity(s: String) {
        let reversed_twice = s.chars().rev().collect::<String>()
            .chars().rev().collect::<String>();
        prop_assert_eq!(s, reversed_twice);
    }
}
```

**Status**: Curious about, not yet in regular use.

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
async fn test_async_operation() {
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
    use rstest::*;
    
    #[rstest]
    #[case(UserId(1), "test@example.com")]
    fn test_user_creation(#[case] id: UserId, #[case] email: &str) {
        let user = User::new(id, email.into());
        assert_eq!(user.id, id);
        assert_eq!(user.email, email);
    }
}
```

**Access to private items**: Tests can access private module items because they're in the same module.

```rust
// Private function in module
fn internal_helper(x: i32) -> i32 {
    x * 2
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_internal_helper() {
        // Can test private functions
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

Use the builder pattern to create test fixtures with sensible defaults and easy customization.

### Basic Test Builder

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    // Builder with sensible test defaults
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
    fn test_user_scenarios(#[case] user: User) {
        // Test with different user configurations
    }
    
    #[test]
    fn test_specific_email() {
        let user = UserBuilder::new()
            .with_email("specific@example.com")
            .build();
        
        assert_eq!(user.email, "specific@example.com");
    }
}
```

### Multiple Builders for Different Scenarios

```rust
#[cfg(test)]
mod tests {
    // Default happy-path user
    fn user() -> UserBuilder {
        UserBuilder::new()
    }
    
    // Admin user preset
    fn admin_user() -> UserBuilder {
        UserBuilder::new()
            .with_role(Role::Admin)
            .with_permissions(all_permissions())
    }
    
    // Suspended user preset
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
    fn test_with_environment() {
        let env = TestEnvironmentBuilder::new()
            .with_database(TestDb::in_memory())
            .with_port(8080)
            .build();
        
        // Test with fully configured environment
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
    fn test_user_creation(test_db: TestDatabase) {
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
    fn test_sum(#[case] input: Vec<i32>, #[case] expected: i32) {
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
// Type prevents invalid age
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
    fn test_age_validation(#[case] years: u8, #[case] valid: bool) {
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
10. **Explore property-based testing** for invariants and edge cases

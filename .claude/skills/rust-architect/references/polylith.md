# Polylith Architecture in Rust

## What is Polylith?

Polylith is a software architecture that aims for:
- **Maximum code reuse** across projects
- **Independent development** of components
- **Incremental testing** - test only what changed
- **Simplified CI/CD** - build only affected artifacts

Originally from Clojure, adapting to Rust requires thinking through unique tradeoffs.

## Core Concepts

### Workspace Structure
```
polylith-rust/
├── components/          # Reusable logic
│   ├── user/
│   │   ├── interface.rs    # Public API
│   │   └── core.rs         # Implementation
│   ├── auth/
│   └── payment/
├── bases/              # Entry points (main, handlers)
│   ├── web-api/
│   └── cli/
├── projects/           # Deployable artifacts
│   ├── backend/
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   └── admin-cli/
│       ├── Cargo.toml
│       └── src/main.rs
└── development/        # For REPL/testing
    ├── Cargo.toml
    └── src/lib.rs
```

### Component Structure
```
components/user/
├── interface.rs       # Public trait/types
└── core.rs           # Implementation

// interface.rs
pub trait UserService {
    fn create_user(&self, email: &str) -> Result<User>;
    fn get_user(&self, id: UserId) -> Result<User>;
}

pub struct User {
    pub id: UserId,
    pub email: String,
}

// core.rs
pub struct UserServiceImpl {
    db: Box<dyn Database>,
}

impl UserService for UserServiceImpl {
    fn create_user(&self, email: &str) -> Result<User> {
        // Implementation
    }
}
```

## Rust-Specific Considerations

### Challenge 1: Cargo Workspaces vs Polylith

**Polylith ideal**: One namespace, all components available
**Cargo reality**: Explicit dependencies in Cargo.toml

**Solution**: Workspace with path dependencies
```toml
# projects/backend/Cargo.toml
[dependencies]
user-interface = { path = "../../components/user", package = "user-interface" }
user-core = { path = "../../components/user", package = "user-core" }
auth-interface = { path = "../../components/auth", package = "auth-interface" }
```

**Philosophical tradeoff:**
- ✅ Explicit dependencies (Rust philosophy)
- ❌ More ceremony than Clojure
- ✅ Compiler catches missing deps
- ❌ Can't easily "try" components without declaring

### Challenge 2: Interface/Implementation Split

**Option A: Traits** (Most Rusty)
```rust
// components/user/interface.rs
pub trait UserService {
    fn create_user(&self, email: &str) -> Result<User>;
}

// components/user/core.rs
pub struct UserServiceImpl { /* ... */ }

impl user_interface::UserService for UserServiceImpl {
    fn create_user(&self, email: &str) -> Result<User> {
        // Implementation
    }
}
```

**Pros:**
- Testable (mock implementations)
- Multiple implementations possible
- True abstraction

**Cons:**
- Dynamic dispatch overhead (can use generics)
- More ceremony
- Trait objects can complicate lifetimes

**Option B: Modules** (Simpler)
```rust
// components/user/mod.rs
pub mod interface {
    pub struct User { /* ... */ }
    pub fn create_user(email: &str) -> Result<User> {
        crate::user::core::create_user_impl(email)
    }
}

mod core {
    pub(crate) fn create_user_impl(email: &str) -> Result<User> {
        // Implementation
    }
}
```

**Pros:**
- Simpler
- No trait overhead
- Direct compilation

**Cons:**
- Harder to mock
- Less flexible
- Tighter coupling

**Recommendation**: Use traits for true boundaries, modules for simple cases

### Challenge 3: Incremental Compilation

**Polylith promise**: Only rebuild what changed

**Rust reality**: Cargo already does incremental compilation

**Question**: Does Polylith add value here?

**Answer**: Yes, but differently:
- Polylith: Deploy only changed projects
- Cargo: Rebuild only changed crates
- Value: Deployment optimization, not build optimization

### Challenge 4: Feature Flags vs Components

**Rust way**: Feature flags
```toml
[features]
default = ["user", "auth"]
user = []
auth = []
admin = []
```

**Polylith way**: Include/exclude components per project

**Tradeoff discussion:**
- Features: Compile-time selection, binary size optimization
- Components: Clearer boundaries, easier to understand dependencies
- **Hybrid**: Use components, expose as features
  ```toml
  [features]
  default = []
  full = ["user-component", "auth-component"]
  
  [dependencies]
  user-component = { path = "../components/user", optional = true }
  ```

### Challenge 5: Testing

**Polylith approach**: Test components in isolation + integration

```rust
// components/user/tests/integration_test.rs
#[test]
fn test_user_creation() {
    let service = UserServiceImpl::new(MockDb::new());
    let user = service.create_user("test@example.com").unwrap();
    assert_eq!(user.email, "test@example.com");
}
```

**Rust enhancement**: Use workspace-level tests
```toml
# Workspace Cargo.toml
[workspace]
members = [
    "components/*",
    "bases/*",
    "projects/*",
]

# Run all component tests
# cargo test -p user-core -p auth-core
```

## Practical Polylith in Rust

### Component Template
```rust
// components/payment/interface.rs
pub trait PaymentProcessor {
    async fn charge(&self, amount: Money) -> Result<Transaction>;
}

pub struct Money {
    pub amount: i64,
    pub currency: Currency,
}

pub struct Transaction {
    pub id: TransactionId,
    pub status: TransactionStatus,
}

// components/payment/core.rs
use interface::*;

pub struct StripeProcessor {
    client: StripeClient,
}

impl PaymentProcessor for StripeProcessor {
    async fn charge(&self, amount: Money) -> Result<Transaction> {
        // Stripe implementation
    }
}

pub struct MockProcessor;

impl PaymentProcessor for MockProcessor {
    async fn charge(&self, amount: Money) -> Result<Transaction> {
        // Mock for testing
    }
}
```

### Base Template
```rust
// bases/web-api/src/main.rs
use axum::{Router, routing::post};
use user_interface::UserService;
use auth_interface::AuthService;

struct AppState {
    user_service: Box<dyn UserService>,
    auth_service: Box<dyn AuthService>,
}

#[tokio::main]
async fn main() {
    let state = AppState {
        user_service: Box::new(user_core::UserServiceImpl::new()),
        auth_service: Box::new(auth_core::AuthServiceImpl::new()),
    };
    
    let app = Router::new()
        .route("/users", post(create_user))
        .with_state(state);
    
    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
```

### Project Composition
```toml
# projects/backend/Cargo.toml
[package]
name = "backend"
version = "0.1.0"

[dependencies]
# Include only what this project needs
user-interface = { path = "../../components/user/interface" }
user-core = { path = "../../components/user/core" }
auth-interface = { path = "../../components/auth/interface" }
auth-core = { path = "../../components/auth/core" }
payment-interface = { path = "../../components/payment/interface" }
payment-core = { path = "../../components/payment/core" }

web-api-base = { path = "../../bases/web-api" }
```

## Tooling for Rust Polylith

### Potential Cargo Extensions

**cargo-poly** (hypothetical tool):
```bash
# Create new component
cargo poly component create user

# Show component dependencies
cargo poly deps user

# Build affected projects
cargo poly build --since main

# Test changed components
cargo poly test --changed
```

**Implementation ideas**:
```rust
// Use cargo metadata to analyze workspace
let metadata = MetadataCommand::new()
    .exec()
    .unwrap();

// Find components that changed
let changed_components = git_diff()
    .iter()
    .filter_map(|file| component_from_path(file))
    .collect();

// Find projects that depend on changed components
let affected_projects = metadata.workspace_members
    .iter()
    .filter(|pkg| depends_on_any(pkg, &changed_components))
    .collect();
```

## Philosophical Considerations

### Where Polylith Fits in Rust

**Strengths in Rust:**
1. **Monorepo benefits** - Atomic refactoring across components
2. **Clear boundaries** - Interfaces force good design
3. **Reuse** - Share components across projects
4. **Testing** - Component isolation helps testing

**Tensions with Rust:**
1. **Explicitness** - Cargo wants explicit deps, Polylith wants implicit availability
2. **Compile times** - More crates = longer builds (though incremental helps)
3. **Trait objects** - Performance overhead for maximum flexibility
4. **Async** - Trait async can be tricky

### When to Use Polylith in Rust

**Good fit:**
- Multiple related applications (web, CLI, mobile backend)
- Large team needing clear boundaries
- High component reuse across projects
- Microservices that share logic

**Poor fit:**
- Single application
- Small team (<5 people)
- Greenfield with uncertain boundaries
- Performance-critical single deployment

### Alternative: Cargo Workspaces

Standard workspace is often sufficient:
```
my-app/
├── Cargo.toml (workspace)
├── core/          # Business logic
├── web/           # Web server
├── cli/           # CLI tool
└── shared/        # Common utilities
```

**Use Polylith when:**
- Workspace isn't providing enough structure
- Need stronger component boundaries
- Want automated affected-project detection
- Multiple teams working on overlapping concerns

## Migration Strategy

### Step 1: Start with Workspace
```toml
[workspace]
members = ["app", "cli", "core"]
```

### Step 2: Extract Components
```
core/ → components/user/
        components/auth/
```

### Step 3: Add Interface Layers
```rust
// components/user/interface.rs
pub trait UserService { /* ... */ }

// components/user/core.rs
impl UserService for UserServiceImpl { /* ... */ }
```

### Step 4: Create Projects
```
app/ → projects/web-api/
cli/ → projects/admin-cli/
```

### Step 5: Add Tooling
- Scripts for affected projects
- CI/CD optimizations
- Development environment

## Conclusion

Polylith in Rust requires balancing:
- **Polylith philosophy**: Maximum reuse and flexibility
- **Rust philosophy**: Explicit dependencies and zero-cost abstractions

**Key insight**: Use Polylith for **architecture**, not fight against Cargo. Let Cargo handle building, use Polylith for organizing.

**Success pattern**:
1. Clear component boundaries (interface crates)
2. Cargo workspaces for dependency management
3. Custom tooling for deployment optimization
4. Traits for true flexibility, modules for simplicity

The goal isn't pure Polylith—it's **better Rust codebases inspired by Polylith principles**.

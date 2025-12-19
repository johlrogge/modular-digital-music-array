# Testing Guide

## Quick Test Commands

```bash
# Compile check
cargo check

# Run all tests
cargo test

# Run specific test module
cargo test actions
cargo test provisioning
cargo test hardware

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_action_planning
```

## Test Structure

```
tests/
├── Unit tests (in each file)
│   └── #[cfg(test)] mod tests { ... }
├── Integration tests
│   └── tests/ directory
└── Documentation tests
    └── Examples in /// comments
```

## Running the Beacon

### Dry-Run Mode (Safe)

```bash
# Default mode - no changes made
cargo run

# Or explicitly
cargo run -- --check

# With debug logging
RUST_LOG=debug cargo run -- --check
```

### Apply Mode (DANGEROUS!)

```bash
# Actually makes system changes
cargo run -- --apply

# With verbose logging
RUST_LOG=trace cargo run -- --apply
```

## Test on Development Machine

The beacon works in dry-run mode even without Raspberry Pi:

```bash
# Uses mock hardware data
RUST_LOG=info cargo run -- --check
```

## Test on Raspberry Pi

### Setup

```bash
# On your Pi
git clone https://github.com/your/mdma.git
cd mdma/bases/beacon
cargo build
```

### Dry-Run First!

```bash
# ALWAYS test dry-run first
sudo cargo run -- --check
```

### Then Apply (if ready)

```bash
# BE CAREFUL - this formats drives!
sudo cargo run -- --apply
```

## Continuous Testing

```bash
# Watch mode (requires cargo-watch)
cargo install cargo-watch
cargo watch -x test
```

## Coverage

```bash
# Install tarpaulin
cargo install cargo-tarpaulin

# Run coverage
cargo tarpaulin --out Html
```

## Benchmarking

```bash
# Install criterion
# Add to Cargo.toml: [dev-dependencies] criterion = "0.5"

# Run benchmarks
cargo bench
```

## Common Test Scenarios

### 1. Test Action Planning

```rust
#[tokio::test]
async fn test_planning_detects_state() {
    let input = mock_input();
    let action = MyAction::new();
    
    let planned = action.plan(&input).await.unwrap();
    
    // Verify it detected current state correctly
    assert!(planned.details().contains("expected_state"));
}
```

### 2. Test Execution Validation

```rust
#[tokio::test]
async fn test_execution_validates_output() {
    let input = mock_input();
    let action = MyAction::new();
    let planned = action.plan(&input).await.unwrap();
    
    let (tx, _rx) = mpsc::channel(10);
    let result = planned.execute(&tx).await;
    
    assert!(result.is_ok());
}
```

### 3. Test Progress Events

```rust
#[tokio::test]
async fn test_progress_events() {
    let (tx, mut rx) = mpsc::channel(10);
    let planned = action.plan(&input).await.unwrap();
    
    tokio::spawn(async move {
        planned.execute(&tx).await
    });
    
    // Check we get the right events
    let started = rx.recv().await.unwrap();
    assert!(matches!(started, ExecutionProgress::StageStarted { .. }));
    
    let complete = rx.recv().await.unwrap();
    assert!(matches!(complete, ExecutionProgress::StageComplete { .. }));
}
```

### 4. Test Full Pipeline

```rust
#[tokio::test]
async fn test_complete_pipeline() {
    let config = mock_config();
    let hw = mock_hardware();
    
    // Build complete plan
    let plan = build_provisioning_plan(config, hw).await.unwrap();
    
    assert_eq!(plan.len(), 7);  // All 7 stages
    
    // Execute
    let (tx, _rx) = mpsc::channel(100);
    let result = plan.execute(tx).await;
    
    assert!(result.is_ok());
}
```

## Debugging Tests

### Use --nocapture

```bash
cargo test test_name -- --nocapture
```

### Add Debug Prints

```rust
#[tokio::test]
async fn test_something() {
    println!("Starting test...");
    
    let result = do_something().await;
    println!("Result: {:?}", result);
    
    assert!(result.is_ok());
}
```

### Use RUST_LOG

```bash
RUST_LOG=debug cargo test test_name -- --nocapture
```

## Testing Checklist

Before committing code:

- [ ] `cargo check` passes
- [ ] `cargo test` passes
- [ ] `cargo clippy` has no warnings
- [ ] `cargo fmt` has been run
- [ ] Dry-run mode works: `cargo run -- --check`
- [ ] Documentation updated
- [ ] Tests added for new features

## CI/CD

Create `.github/workflows/test.yml`:

```yaml
name: Test

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - run: cargo test --all-features
```

## Performance Testing

### Measure Stage Times

```rust
use std::time::Instant;

let start = Instant::now();
let result = stage.apply(input).await?;
let duration = start.elapsed();
println!("Stage took: {:?}", duration);
```

### Profile with perf

```bash
cargo build --release
perf record --call-graph=dwarf ./target/release/beacon --check
perf report
```

## Safety Testing

### Test Error Conditions

```rust
#[tokio::test]
async fn test_fails_on_invalid_input() {
    let invalid_input = create_invalid();
    let result = action.plan(&invalid_input).await;
    
    assert!(result.is_err());
}
```

### Test Dry-Run Safety

```rust
#[tokio::test]
async fn test_dry_run_makes_no_changes() {
    let before = read_system_state();
    
    execute_in_dry_run_mode().await;
    
    let after = read_system_state();
    assert_eq!(before, after);
}
```

## Test Data

Create test fixtures:

```rust
fn mock_hardware() -> HardwareInfo {
    HardwareInfo {
        model: "Raspberry Pi 5 Model B".to_string(),
        nvme_drives: vec![
            mock_nvme_drive("/dev/nvme0n1", 512_000_000_000),
        ],
        memory_mb: Some(8192),
        serial: Some("test123".to_string()),
    }
}
```

## Troubleshooting

### Tests Hang

```bash
# Run with timeout
cargo test -- --test-threads=1 --timeout=30
```

### Tests Fail on CI

```bash
# Check platform differences
cargo test --target x86_64-unknown-linux-gnu
```

### Flaky Tests

```bash
# Run multiple times
for i in {1..10}; do cargo test || break; done
```

## Best Practices

1. **One assert per test** (when possible)
2. **Descriptive test names**: `test_planning_detects_existing_partitions`
3. **Use fixtures** for common test data
4. **Mock external dependencies**
5. **Test error paths** not just happy paths
6. **Keep tests fast** - mock slow operations
7. **Test in isolation** - don't depend on order

## Resources

- [Rust Book - Testing](https://doc.rust-lang.org/book/ch11-00-testing.html)
- [tokio Testing](https://tokio.rs/tokio/topics/testing)
- [cargo test docs](https://doc.rust-lang.org/cargo/commands/cargo-test.html)

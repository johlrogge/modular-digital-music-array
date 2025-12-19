# Migration Guide: ActionLegacy → Action (Plan-Then-Execute)

## Overview

This guide shows how to migrate a stage from the old `ActionLegacy` pattern to the new `Action` plan-then-execute pattern.

## Why Migrate?

The new pattern provides:
- **Type-safe plan construction** before execution
- **Self-describing plans** via Display traits
- **Strict output validation** (actual must match planned)
- **Real-time progress feedback** for UI
- **Better testability** (plan and execute separately)

## Migration Checklist

### Step 1: Add Clone Derive

```rust
// OLD
pub struct MyAction {
    config: MyConfig,
}

// NEW
#[derive(Clone)]  // Add this!
pub struct MyAction {
    config: MyConfig,
}
```

### Step 2: Change Trait Import

```rust
// OLD
use crate::actions::ActionLegacy;

// NEW
use crate::actions::{Action, ActionId, PlannedAction};
```

### Step 3: Implement New Trait

```rust
// OLD
impl ActionLegacy<Input, Output> for MyAction {
    fn description(&self) -> String {
        "My action description".to_string()
    }
    
    async fn check(&self, input: &Input) -> Result<bool> {
        // Check if action is needed
        Ok(true)  // or false if already done
    }
    
    async fn apply(&self, input: Input) -> Result<o> {
        // Do the actual work
        Ok(output)
    }
    
    async fn preview(&self, input: Input) -> Result<o> {
        // Show what would happen
        Ok(mock_output)
    }
}

// NEW
impl Action<Input, Output> for MyAction {
    fn id(&self) -> ActionId {
        ActionId::new("my-action")  // Unique ID
    }
    
    fn description(&self) -> String {
        "My action description".to_string()
    }
    
    async fn plan(&self, input: &Input) 
        -> Result<PlannedAction<Input, Output, Self>> 
    {
        // 1. Check current state (like old check())
        let is_needed = check_if_needed(input).await?;
        
        // 2. Build assumed output based on current state
        let assumed_output = if is_needed {
            build_output_from_state(input).await?
        } else {
            build_mock_output(input)
        };
        
        // 3. Return planned action with captured input
        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }
    
    async fn apply(&self, input: Input) -> Result<o> {
        // Same as old apply() - do the actual work
        // Output MUST match what plan() returned in assumed_output!
        Ok(output)
    }
}
```

### Step 4: Add PartialEq to Output Types

```rust
// OLD
#[derive(Debug, Clone)]
pub struct MyOutput {
    field: String,
}

// NEW
#[derive(Debug, Clone, PartialEq)]  // Add PartialEq!
pub struct MyOutput {
    field: String,
}
```

### Step 5: Add Display to Output Types

```rust
impl std::fmt::Display for MyOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MyOutput: {}", self.field)
        // This is used for plan details and error messages
    }
}
```

### Step 6: Update Tests

```rust
// OLD
#[tokio::test]
async fn test_action() {
    let action = MyAction::new();
    let input = mock_input();
    
    let result = action.apply(input).await;
    assert!(result.is_ok());
}

// NEW
#[tokio::test]
async fn test_action_planning() {
    let action = MyAction::new();
    let input = mock_input();
    
    // Test planning
    let planned = action.plan(&input).await.unwrap();
    assert_eq!(planned.id(), ActionId::new("my-action"));
    
    // Verify assumed output looks correct
    assert_eq!(planned.assumed_output.field, "expected_value");
}

#[tokio::test]
async fn test_action_execution() {
    let action = MyAction::new();
    let input = mock_input();
    
    let planned = action.plan(&input).await.unwrap();
    
    // Test execution with progress channel
    let (tx, _rx) = tokio::sync::mpsc::channel(10);
    let result = planned.execute(&tx).await;
    
    assert!(result.is_ok());
}
```

## Complete Example: stage0_safety.rs

### Before (ActionLegacy)

```rust
use crate::actions::ActionLegacy;
use crate::error::{BeaconError, Result};
use crate::hardware::HardwareInfo;
use crate::provisioning::types::SafeHardware;

pub struct CheckRaspberryPiAction;

impl ActionLegacy<HardwareInfo, SafeHardware> for CheckRaspberryPiAction {
    fn description(&self) -> String {
        "Verify running on Raspberry Pi".to_string()
    }

    async fn check(&self, _input: &HardwareInfo) -> Result<bool> {
        Ok(true)  // Always need to verify
    }

    async fn apply(&self, input: HardwareInfo) -> Result<SafeHardware> {
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
        if !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety("Not running on Pi".into()));
        }
        Ok(SafeHardware { info: input })
    }

    async fn preview(&self, input: HardwareInfo) -> Result<SafeHardware> {
        Ok(SafeHardware { info: input })
    }
}
```

### After (Action)

```rust
use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::hardware::HardwareInfo;
use crate::provisioning::types::SafeHardware;

#[derive(Clone)]  // Added!
pub struct CheckRaspberryPiAction;

impl Action<HardwareInfo, SafeHardware> for CheckRaspberryPiAction {
    fn id(&self) -> ActionId {  // NEW!
        ActionId::new("check-raspberry-pi")
    }
    
    fn description(&self) -> String {
        "Verify running on Raspberry Pi".to_string()
    }

    async fn plan(&self, input: &HardwareInfo)  // NEW!
        -> Result<PlannedAction<HardwareInfo, SafeHardware, Self>> 
    {
        // Check DURING PLANNING
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
        if !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety("Not running on Pi".into()));
        }
        
        let assumed_output = SafeHardware { info: input.clone() };
        
        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }
    
    async fn apply(&self, input: HardwareInfo) -> Result<SafeHardware> {
        // Re-verify during execution (paranoid + idempotent)
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
        if !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety("Not running on Pi".into()));
        }
        Ok(SafeHardware { info: input })
    }
}
```

### Updated Types (SafeHardware)

```rust
#[derive(Debug, Clone, PartialEq)]  // Added PartialEq!
pub struct SafeHardware {
    pub info: HardwareInfo,
}

impl std::fmt::Display for SafeHardware {  // NEW!
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ Raspberry Pi verified: {}", self.info.model)
    }
}
```

## Common Patterns

### Pattern 1: Already Done Check

```rust
async fn plan(&self, input: &Input) -> Result<PlannedAction<...>> {
    // Check if already done
    if already_done(input).await? {
        // Build output from current state
        let assumed_output = read_current_state(input).await?;
    } else {
        // Build output from what we'll create
        let assumed_output = calculate_what_to_create(input);
    }
    
    Ok(PlannedAction { ... })
}
```

### Pattern 2: State-Based Planning

```rust
async fn plan(&self, input: &Input) -> Result<PlannedAction<...>> {
    // Read current system state
    let current_partitions = read_partitions(&input.drive).await?;
    
    if current_partitions.matches_requirements() {
        // Use existing
        assumed_output = current_partitions;
    } else {
        // Calculate new layout
        assumed_output = calculate_new_layout(input);
    }
    
    Ok(PlannedAction { ... })
}
```

### Pattern 3: Validation During Planning

```rust
async fn plan(&self, input: &Input) -> Result<PlannedAction<...>> {
    // Validate requirements DURING PLANNING
    if !meets_requirements(input) {
        return Err(BeaconError::Hardware("Requirements not met".into()));
    }
    
    let assumed_output = build_output(input)?;
    Ok(PlannedAction { ... })
}
```

## Testing Migration

1. **Plan test** - Verify plan() builds correct assumed_output
2. **Execute test** - Verify execute() matches planned output
3. **Integration test** - Verify stage chains correctly

## Gotchas

### 1. PartialEq on Complex Types

If your output type contains complex fields:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct MyOutput {
    simple_field: String,
    #[allow(clippy::derive_partial_eq_without_eq)]
    complex_field: Vec<ComplexType>,  // Might not have Eq
}
```

### 2. Display for Complex Outputs

Keep Display implementations concise:

```rust
impl std::fmt::Display for ComplexOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Don't dump everything!
        write!(f, "Summary: {} items processed", self.items.len())
    }
}
```

### 3. Cloning Actions

Make sure all fields in your action are Clone:

```rust
#[derive(Clone)]
pub struct MyAction {
    config: MyConfig,  // MyConfig must be Clone!
}
```

## Benefits After Migration

1. **Show plan before executing**:
```rust
let plan = build_complete_plan().await?;
for summary in plan.summary() {
    println!("{}: {}", summary.id, summary.description);
    println!("  {}", summary.details);  // Uses Display!
}
```

2. **Real-time progress**:
```rust
let (tx, mut rx) = mpsc::channel(100);
tokio::spawn(async move { plan.execute(tx).await });

while let Some(progress) = rx.recv().await {
    // Send to UI via Server-Sent Events!
}
```

3. **Strict validation**:
```rust
// Execution FAILS if output doesn't match plan
// Prevents silent corruption!
```

## Questions?

See the migrated `stage0_safety.rs` and `stage1_validate.rs` for complete working examples!

// bases/beacon/src/actions.rs
//! Action framework for idempotent, mode-aware operations
//!
//! This module provides the core Action trait that enables:
//! - Type-safe chaining via Input/Output generics
//! - Idempotency through check() method
//! - Mode-aware execution (DRY RUN vs APPLY)
//! - Automatic handling of preview vs actual execution

use crate::error::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Execution mode for actions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    /// Preview what would happen without making changes
    DryRun,
    /// Actually execute the action
    Apply,
}

/// Send a log message to both tracing and the broadcast channel
macro_rules! send_log {
    ($tx:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        tracing::info!("{}", msg);
        let _ = $tx.send(msg);
    }};
}

/// An action that transforms Input into Output
///
/// The type system enforces correct chaining - an Action<A, B> can only
/// follow an Action<_, A>. This gives us compile-time guarantees about
/// the pipeline ordering.
///
/// # Type Parameters
///
/// - `Input`: The type this action consumes
/// - `Output`: The type this action produces
///
/// # Example
///
/// ```ignore
/// struct PartitionDrivesAction;
///
/// impl Action<ValidatedHardware, PartitionedDrives> for PartitionDrivesAction {
///     fn description(&self) -> String {
///         "Partition NVMe drives".to_string()
///     }
///     
///     async fn check(&self, input: &ValidatedHardware) -> Result<bool> {
///         // Check if already partitioned
///         Ok(true)
///     }
///     
///     async fn apply(&self, input: ValidatedHardware) -> Result<PartitionedDrives> {
///         // Actually partition
///     }
///     
///     async fn preview(&self, input: ValidatedHardware) -> Result<PartitionedDrives> {
///         // Build mock output for DRY RUN
///     }
/// }
/// ```
pub trait Action<Input, Output> {
    /// Human-readable description of what this action does
    fn description(&self) -> String;

    /// Check if this action needs to be applied
    ///
    /// Given the input, determine if the transformation is needed.
    /// Returns true if apply() should be called.
    ///
    /// This enables idempotency - if the system is already in the desired
    /// state, we can skip the action.
    async fn check(&self, input: &Input) -> Result<bool>;

    /// Apply the transformation (APPLY mode)
    ///
    /// Consumes the input and produces the output.
    /// Should execute actual commands and make real changes.
    async fn apply(&self, input: Input) -> Result<Output>;

    /// Preview the transformation (DRY RUN mode)
    ///
    /// Builds a mock output that allows the rest of the pipeline
    /// to continue in preview mode. Should NOT execute any commands.
    ///
    /// The mock output should be realistic enough that subsequent
    /// actions can preview their behavior correctly.
    async fn preview(&self, input: Input) -> Result<Output>;
}

/// Execute an action in the given mode
///
/// This is the central execution function that handles mode checking.
/// All actions should be executed through this function.
///
/// # Behavior
///
/// - **DRY RUN**: Calls preview() and logs what would happen
/// - **APPLY**: Calls check(), then either apply() if needed or preview() if already done
///
/// # Type Parameters
///
/// - `I`: Input type
/// - `O`: Output type  
/// - `A`: Action type implementing Action<I, O>
///
/// # Example
///
/// ```ignore
/// let partitioned = execute_action(
///     &PartitionDrivesAction,
///     validated,
///     mode,
///     &log_tx
/// ).await?;
/// ```
pub async fn execute_action<I, O, A>(
    action: &A,
    input: I,
    mode: ExecutionMode,
    log_tx: &broadcast::Sender<String>,
) -> Result<O>
where
    A: Action<I, O>,
    I: Clone,  // Need to clone for check in APPLY mode
{
    send_log!(log_tx, "🔍 {}", action.description());

    match mode {
        ExecutionMode::DryRun => {
            send_log!(log_tx, "   [DRY RUN] Previewing...");
            let output = action.preview(input).await?;
            send_log!(log_tx, "   ✅ Preview complete");
            Ok(output)
        }
        ExecutionMode::Apply => {
            // Check if action is needed
            let needed = action.check(&input).await?;

            if needed {
                send_log!(log_tx, "   ⚙️  Executing...");
                let output = action.apply(input).await?;
                send_log!(log_tx, "   ✅ Complete");
                Ok(output)
            } else {
                send_log!(log_tx, "   ⏭️  Already done, skipping");
                // Use preview to construct output from existing state
                let output = action.preview(input).await?;
                Ok(output)
            }
        }
    }
}

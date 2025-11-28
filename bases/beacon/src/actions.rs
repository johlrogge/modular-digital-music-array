// bases/beacon/src/actions.rs
use crate::error::Result;
use std::fmt;
use tracing::info;

/// Execution mode for actions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Dry run - only describe what would happen
    DryRun,
    /// Actually execute the action
    Apply,
}

/// An action that can be described or applied to the system
pub trait Action: fmt::Debug {
    /// Describe what this action would do
    fn describe(&self) -> String;
    
    /// Actually execute the action
    async fn apply(&self) -> Result<()>;
}

/// Execute an action based on the execution mode
pub async fn execute_action<A: Action>(action: &A, mode: ExecutionMode) -> Result<()> {
    let description = action.describe();
    
    match mode {
        ExecutionMode::DryRun => {
            info!("[DRY RUN] {}", description);
            Ok(())
        }
        ExecutionMode::Apply => {
            info!("[EXECUTING] {}", description);
            action.apply().await
        }
    }
}

/// Execute multiple actions in sequence
pub async fn execute_actions<A: Action>(
    actions: &[A],
    mode: ExecutionMode,
) -> Result<()> {
    for action in actions {
        execute_action(action, mode).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::BeaconError;

    #[derive(Debug)]
    struct TestAction {
        name: String,
        should_fail: bool,
    }

    impl Action for TestAction {
        fn describe(&self) -> String {
            format!("Test action: {}", self.name)
        }

        async fn apply(&self) -> Result<()> {
            if self.should_fail {
                Err(BeaconError::Installation("test failure".to_string()))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn test_dry_run_never_fails() {
        let action = TestAction {
            name: "failing action".to_string(),
            should_fail: true,
        };

        // Dry run should succeed even though apply would fail
        let result = execute_action(&action, ExecutionMode::DryRun).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_apply_executes_action() {
        let action = TestAction {
            name: "success action".to_string(),
            should_fail: false,
        };

        let result = execute_action(&action, ExecutionMode::Apply).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_apply_propagates_failure() {
        let action = TestAction {
            name: "failing action".to_string(),
            should_fail: true,
        };

        let result = execute_action(&action, ExecutionMode::Apply).await;
        assert!(result.is_err());
    }
}

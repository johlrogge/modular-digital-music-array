// bases/beacon/src/actions.rs
//! Action framework for plan-then-execute provisioning
//!
//! This module provides both:
//! - NEW: Plan-then-execute architecture with type-safe chaining
//! - LEGACY: Old check/apply/preview pattern (for backward compatibility)

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::{broadcast, mpsc};
// ============================================================================
// Core Types (NEW Architecture)
// ============================================================================

/// Unique identifier for an action stage
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActionId(String);

impl ActionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ActionId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for ActionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Execution mode for actions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    /// Preview what would happen without making changes
    DryRun,
    /// Actually execute the action
    Apply,
}

/// Progress feedback during plan execution
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum ExecutionProgress {
    Started { id: ActionId, description: String },
    Progress { id: ActionId, message: String },
    Complete { id: ActionId },
    Failed { id: ActionId, error: String },
}

/// Errors during plan execution
#[derive(Debug, thiserror::Error)]
pub enum PlanExecutionError {
    #[error("Stage '{stage_id}' failed to execute: {error}")]
    ExecutionFailed { stage_id: ActionId, error: String },

    #[error("Stage '{stage_id}' produced unexpected output.\nExpected:\n{expected}\n\nActual:\n{actual}")]
    UnexpectedOutput {
        stage_id: ActionId,
        expected: String,
        actual: String,
    },

    #[error("Failed to send progress update: {0}")]
    FeedbackChannelClosed(String),
}

// ============================================================================
// PlannedAction (NEW)
// ============================================================================

/// A planned action ready for execution
#[derive(Debug)]
pub struct PlannedAction<Input: Debug, Output: Debug, A: Debug>
where
    A: Action<Input, Output>,
{
    /// Human-readable description
    pub description: String,

    /// The action that will execute this
    pub(crate) action: A,

    /// Input captured during planning
    pub(crate) input: Input,

    /// What we expect to produce
    pub assumed_output: Output,
}

impl<Input, Output, A> PlannedAction<Input, Output, A>
where
    A: Action<Input, Output> + Debug,
    Input: Clone + Send + Sync + Debug + 'static,
    Output: PartialEq + std::fmt::Display + Send + Sync + Debug + 'static,
{
    /// Get the action's ID
    pub fn id(&self) -> ActionId {
        self.action.id()
    }

    /// Get details about what this action will do
    pub fn details(&self) -> String {
        self.assumed_output.to_string()
    }

    /// Execute this planned action
    pub async fn execute(
        &self,
        feedback: &mpsc::Sender<ExecutionProgress>,
    ) -> std::result::Result<(), PlanExecutionError> {
        feedback
            .send(ExecutionProgress::Started {
                id: self.id(),
                description: self.description.clone(),
            })
            .await
            .map_err(|e| PlanExecutionError::FeedbackChannelClosed(e.to_string()))?;

        // Execute the PLAN, not recalculate from input!
        let actual_output = self.action.apply(&self.assumed_output).await.map_err(|e| {
            PlanExecutionError::ExecutionFailed {
                stage_id: self.id(),
                error: e.to_string(),
            }
        })?;

        if actual_output != self.assumed_output {
            return Err(PlanExecutionError::UnexpectedOutput {
                stage_id: self.id(),
                expected: self.assumed_output.to_string(),
                actual: actual_output.to_string(),
            });
        }

        feedback
            .send(ExecutionProgress::Complete { id: self.id() })
            .await
            .map_err(|e| PlanExecutionError::FeedbackChannelClosed(e.to_string()))?;

        Ok(())
    }
}

// ============================================================================
// Action Trait (NEW)
// ============================================================================

/// An action that can be planned and executed
pub trait Action<Input: Debug, Output: Debug>: Clone + Send + Sync + Debug + 'static {
    fn id(&self) -> ActionId;
    fn description(&self) -> String;

    fn plan(
        &self,
        input: &Input,
    ) -> impl std::future::Future<Output = Result<PlannedAction<Input, Output, Self>>> + Send;

    /// Execute the planned output (not recalculate from input!)
    fn apply(
        &self,
        planned_output: &Output,
    ) -> impl std::future::Future<Output = Result<Output>> + Send;
}

// ============================================================================
// Type-Erased Execution (NEW)
// ============================================================================

pub trait ExecutableStage: Send + Sync + std::fmt::Debug {
    fn id(&self) -> ActionId;
    fn description(&self) -> String;
    fn details(&self) -> String;
    fn execute<'a>(
        &'a self,
        feedback: &'a mpsc::Sender<ExecutionProgress>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), PlanExecutionError>> + Send + 'a>>;
}

impl<I, O, A> ExecutableStage for PlannedAction<I, O, A>
where
    A: Action<I, O> + Debug,
    I: Clone + Send + Sync + Debug + 'static,
    O: PartialEq + std::fmt::Display + Send + Sync + Debug + 'static,
{
    fn id(&self) -> ActionId {
        PlannedAction::id(self)
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn details(&self) -> String {
        PlannedAction::details(self)
    }

    fn execute<'a>(
        &'a self,
        feedback: &'a mpsc::Sender<ExecutionProgress>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), PlanExecutionError>> + Send + 'a>>
    {
        Box::pin(PlannedAction::execute(self, feedback))
    }
}

// ============================================================================
// ProvisioningPlan (NEW)
// ============================================================================
#[derive(Debug)]
pub struct ProvisioningPlan {
    stages: Vec<Box<dyn ExecutableStage>>,
}

impl ProvisioningPlan {
    pub fn new<I, O, A>(first_stage: PlannedAction<I, O, A>) -> Self
    where
        A: Action<I, O> + Debug,
        I: Clone + Send + Sync + Debug + 'static,
        O: PartialEq + std::fmt::Display + Send + Sync + Debug + 'static,
    {
        Self {
            stages: vec![Box::new(first_stage)],
        }
    }

    pub fn append<I, O, A>(mut self, stage: PlannedAction<I, O, A>) -> Self
    where
        A: Action<I, O> + Debug,
        I: Clone + Send + Sync + Debug + 'static,
        O: PartialEq + std::fmt::Display + Send + Sync + Debug + 'static,
    {
        self.stages.push(Box::new(stage));
        self
    }

    pub fn len(&self) -> usize {
        self.stages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    pub fn summary(&self) -> Vec<StageSummary> {
        self.stages
            .iter()
            .map(|stage| StageSummary {
                id: stage.id(),
                description: stage.description(),
                details: stage.details(),
            })
            .collect()
    }

    pub async fn execute(
        &self,
        feedback: mpsc::Sender<ExecutionProgress>,
    ) -> std::result::Result<(), PlanExecutionError> {
        for stage in &self.stages {
            stage.execute(&feedback).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StageSummary {
    pub id: ActionId,
    pub description: String,
    pub details: String,
}

// ============================================================================
// LEGACY Support (OLD Architecture - for backward compatibility)
// ============================================================================

/// Send a log message to both tracing and broadcast channel
macro_rules! send_log {
    ($tx:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        tracing::info!("{}", msg);
        let _ = $tx.send(msg);
    }};
}

pub(crate) use send_log;

/// Legacy trait for backward compatibility
pub trait ActionLegacy<Input, Output> {
    fn description(&self) -> String;
    async fn check(&self, input: &Input) -> Result<bool>;
    async fn apply(&self, input: Input) -> Result<Output>;
    async fn preview(&self, input: Input) -> Result<Output>;
}

/// Legacy execute function
pub async fn execute_action<I, O, A>(
    action: &A,
    input: I,
    mode: ExecutionMode,
    log_tx: &broadcast::Sender<String>,
) -> Result<O>
where
    A: ActionLegacy<I, O>,
    I: Clone,
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
            let needed = action.check(&input).await?;

            if needed {
                send_log!(log_tx, "   ⚙️  Executing...");
                let output = action.apply(input).await?;
                send_log!(log_tx, "   ✅ Complete");
                Ok(output)
            } else {
                send_log!(log_tx, "   ⏭️  Already done, skipping");
                let output = action.preview(input).await?;
                Ok(output)
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct TestInput(i32);

    impl std::fmt::Display for TestInput {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Input: {}", self.0)
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    struct TestOutput(i32);

    impl std::fmt::Display for TestOutput {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Output: {}", self.0)
        }
    }

    #[derive(Clone, Debug)]
    struct DoubleAction;

    impl Action<TestInput, TestOutput> for DoubleAction {
        fn id(&self) -> ActionId {
            ActionId::new("double")
        }

        fn description(&self) -> String {
            "Double the input".to_string()
        }

        async fn plan(
            &self,
            input: &TestInput,
        ) -> Result<PlannedAction<TestInput, TestOutput, Self>> {
            let assumed_output = TestOutput(input.0 * 2);

            Ok(PlannedAction {
                description: self.description(),
                action: self.clone(),
                input: input.clone(),
                assumed_output,
            })
        }

        async fn apply(&self, output: &TestOutput) -> Result<TestOutput> {
            Ok(output.clone())
        }
    }

    #[tokio::test]
    async fn planned_action_executes_and_validates() {
        let input = TestInput(5);
        let action = DoubleAction;

        let planned = action.plan(&input).await.unwrap();
        assert_eq!(planned.assumed_output.0, 10);

        let (tx, mut rx) = mpsc::channel(10);
        planned.execute(&tx).await.unwrap();

        let started = rx.recv().await.unwrap();
        assert!(matches!(started, ExecutionProgress::Started { .. }));

        let complete = rx.recv().await.unwrap();
        assert!(matches!(complete, ExecutionProgress::Complete { .. }));
    }

    #[tokio::test]
    async fn provisioning_plan_executes_all_stages() {
        let input = TestInput(5);
        let action = DoubleAction;

        let stage1 = action.plan(&input).await.unwrap();
        let stage2 = action
            .plan(&TestInput(stage1.assumed_output.0))
            .await
            .unwrap();

        let plan = ProvisioningPlan::new(stage1).append(stage2);

        assert_eq!(plan.len(), 2);

        let (tx, _rx) = mpsc::channel(100);
        plan.execute(tx).await.unwrap();
    }
}

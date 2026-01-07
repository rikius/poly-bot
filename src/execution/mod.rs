//! Execution module - order state machine, policies, and executor
//!
//! ## Architecture
//!
//! - `state_machine`: Order state transitions and ledger invariants
//! - `policy`: Execution policies (TakerPolicy, MakerPolicy)
//! - `executor`: Converts OrderIntent → orders and submits
//!
//! ## Flow
//!
//! 1. Strategy returns `OrderIntent` (WHAT it wants)
//! 2. `ExecutionPolicy` converts to `OrderParams` (HOW to execute)
//! 3. `OrderExecutor` signs and submits orders
//! 4. Policy handles partial fills per strategy needs

pub mod executor;
pub mod policy;
pub mod state_machine;

// Re-export state machine types
pub use state_machine::{
    can_cancel, check_invariants, requires_ack, validate_transition, Discrepancy,
    InvariantViolation,
};

// Re-export policy types
pub use policy::{
    ExecutionPolicy, IntentRef, MakerPolicy, OrderParams, PartialFillAction, TakerPolicy,
};

// Re-export executor types
pub use executor::{ExecutionResult, ExecutionStatus, OrderExecutor};

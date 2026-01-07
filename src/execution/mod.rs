//! Execution module - order state machine and execution policies
//!
//! Manages order lifecycle and ensures state consistency.

pub mod state_machine;

pub use state_machine::{
    can_cancel, check_invariants, requires_ack, validate_transition, Discrepancy,
    InvariantViolation,
};

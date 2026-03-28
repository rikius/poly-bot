//! HTTP + WebSocket API server for the frontend dashboard
//!
//! Runs alongside the bot as a separate Tokio task.
//! Exposes real-time bot state to the React frontend via WebSocket.

pub mod controls;
pub mod server;
pub mod types;

pub use controls::ControlState;
pub use server::{run_api_server, ApiState};

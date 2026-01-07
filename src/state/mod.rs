//! State management - order books and market registry
//!
//! Maintains real-time order book state with lock-free updates

pub mod order_book;

pub use order_book::{OrderBookState, BookSnapshot};

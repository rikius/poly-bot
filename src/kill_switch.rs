//! Kill switch for emergency bot shutdown
//!
//! The kill switch can be triggered by:
//! 1. Setting environment variable POLYBOT_KILL=1
//! 2. Creating file /tmp/polybot_kill
//! 3. Calling kill() programmatically
//!
//! ## Async Usage
//!
//! Use `wait_for_kill()` in a tokio::select! for zero-latency shutdown:
//! ```ignore
//! tokio::select! {
//!     _ = kill_switch.wait_for_kill() => break,
//!     // ... other branches
//! }
//! ```

use crate::constants::{KILL_SWITCH_ENV_VAR, KILL_SWITCH_FILE};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;
use tracing::{error, info, warn};

/// Kill switch for emergency shutdown
///
/// Provides both synchronous (`is_killed()`) and asynchronous (`wait_for_kill()`)
/// interfaces for checking shutdown status.
pub struct KillSwitch {
    /// Atomic flag indicating if kill switch has been triggered
    killed: AtomicBool,
    /// Async notification for waiters
    notify: Notify,
}

// Manual Debug impl because Notify doesn't implement Debug
impl std::fmt::Debug for KillSwitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KillSwitch")
            .field("killed", &self.killed.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for KillSwitch {
    fn default() -> Self {
        Self::new()
    }
}

impl KillSwitch {
    /// Create a new kill switch
    pub fn new() -> Self {
        Self {
            killed: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    /// Check if the kill switch has been triggered (synchronous)
    ///
    /// This checks (in order):
    /// 1. Internal atomic flag
    /// 2. Environment variable POLYBOT_KILL
    /// 3. File /tmp/polybot_kill
    #[inline]
    pub fn is_killed(&self) -> bool {
        // Fast path: check atomic flag first
        if self.killed.load(Ordering::Relaxed) {
            return true;
        }

        // Check environment variable
        if std::env::var(KILL_SWITCH_ENV_VAR).is_ok() {
            warn!("Kill switch triggered via environment variable");
            self.do_kill();
            return true;
        }

        // Check file existence
        if Path::new(KILL_SWITCH_FILE).exists() {
            warn!("Kill switch triggered via file");
            self.do_kill();
            return true;
        }

        false
    }

    /// Manually trigger the kill switch
    pub fn kill(&self) {
        warn!("Kill switch manually triggered");
        self.do_kill();
    }

    /// Internal kill - sets flag and notifies waiters
    fn do_kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Async wait for kill signal
    ///
    /// Returns immediately if already killed, otherwise waits for kill().
    /// Use this in tokio::select! for zero-latency shutdown handling.
    ///
    /// # Example
    /// ```ignore
    /// tokio::select! {
    ///     _ = kill_switch.wait_for_kill() => {
    ///         println!("Shutting down...");
    ///         break;
    ///     }
    ///     // other branches...
    /// }
    /// ```
    pub async fn wait_for_kill(&self) {
        // Fast path: already killed
        if self.is_killed() {
            return;
        }

        // Wait for notification
        self.notify.notified().await;
    }

    /// Reset the kill switch (for testing)
    #[cfg(test)]
    pub fn reset(&self) {
        self.killed.store(false, Ordering::SeqCst);
    }
}

/// Graceful shutdown procedure
///
/// This should be called when the kill switch is triggered to:
/// 1. Stop placing new orders
/// 2. Cancel all open orders
/// 3. Print final state snapshot
/// 4. Persist state to disk
pub async fn graceful_shutdown(kill_switch: Arc<KillSwitch>) {
    error!("=== GRACEFUL SHUTDOWN INITIATED ===");

    // Mark as killed to prevent any new operations
    kill_switch.kill();

    // TODO: In later phases, this will:
    // - Cancel all open orders via API
    // - Save ledger state to disk
    // - Print final P&L summary

    info!("Shutdown complete - all operations stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    #[test]
    #[serial]
    fn test_kill_switch_default_not_killed() {
        // Clean up any existing triggers BEFORE creating KillSwitch
        std::env::remove_var(KILL_SWITCH_ENV_VAR);
        let _ = fs::remove_file(KILL_SWITCH_FILE);

        let ks = KillSwitch::new();
        assert!(!ks.is_killed());
    }

    #[test]
    #[serial]
    fn test_kill_switch_manual_trigger() {
        let ks = KillSwitch::new();
        std::env::remove_var(KILL_SWITCH_ENV_VAR);
        let _ = fs::remove_file(KILL_SWITCH_FILE);

        assert!(!ks.is_killed());
        ks.kill();
        assert!(ks.is_killed());
    }

    #[test]
    #[serial]
    fn test_kill_switch_env_var() {
        let ks = KillSwitch::new();
        let _ = fs::remove_file(KILL_SWITCH_FILE);

        std::env::set_var(KILL_SWITCH_ENV_VAR, "1");
        assert!(ks.is_killed());

        // Cleanup
        std::env::remove_var(KILL_SWITCH_ENV_VAR);
    }

    #[test]
    #[serial]
    fn test_kill_switch_file() {
        let ks = KillSwitch::new();
        ks.reset();
        std::env::remove_var(KILL_SWITCH_ENV_VAR);

        // Create kill file
        fs::write(KILL_SWITCH_FILE, "").unwrap();
        assert!(ks.is_killed());

        // Cleanup
        let _ = fs::remove_file(KILL_SWITCH_FILE);
    }
}

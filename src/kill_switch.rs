//! Kill switch for emergency bot shutdown
//!
//! The kill switch can be triggered by:
//! 1. Setting environment variable POLYBOT_KILL=1
//! 2. Creating file /tmp/polybot_kill
//! 3. Calling kill() programmatically

use crate::constants::{KILL_SWITCH_ENV_VAR, KILL_SWITCH_FILE};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info, warn};

/// Kill switch for emergency shutdown
#[derive(Debug)]
pub struct KillSwitch {
    /// Atomic flag indicating if kill switch has been triggered
    killed: AtomicBool,
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
        }
    }

    /// Check if the kill switch has been triggered
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
            self.killed.store(true, Ordering::SeqCst);
            return true;
        }

        // Check file existence
        if Path::new(KILL_SWITCH_FILE).exists() {
            warn!("Kill switch triggered via file");
            self.killed.store(true, Ordering::SeqCst);
            return true;
        }

        false
    }

    /// Manually trigger the kill switch
    pub fn kill(&self) {
        warn!("Kill switch manually triggered");
        self.killed.store(true, Ordering::SeqCst);
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

//! Cash balance tracking
//!
//! Tracks available, reserved, and total USDC balances.
//! Uses atomic operations where possible for thread safety.

use crate::error::{BotError, Result};
use rust_decimal::Decimal;
use std::sync::RwLock;

/// Cash balance with reservation support
///
/// - `available`: Can be used for new orders
/// - `reserved`: Locked in open orders (reserved on ACK, released on cancel/fill)
/// - `total`: available + reserved
#[derive(Debug)]
pub struct CashBalance {
    /// Cash available for new orders
    available: RwLock<Decimal>,
    /// Cash reserved in open orders
    reserved: RwLock<Decimal>,
}

impl Default for CashBalance {
    fn default() -> Self {
        Self::new(Decimal::ZERO)
    }
}

impl CashBalance {
    /// Create a new cash balance with initial amount
    pub fn new(initial: Decimal) -> Self {
        Self {
            available: RwLock::new(initial),
            reserved: RwLock::new(Decimal::ZERO),
        }
    }

    /// Get available cash
    pub fn available(&self) -> Decimal {
        *self.available.read().unwrap()
    }

    /// Get reserved cash
    pub fn reserved(&self) -> Decimal {
        *self.reserved.read().unwrap()
    }

    /// Get total cash (available + reserved)
    pub fn total(&self) -> Decimal {
        self.available() + self.reserved()
    }

    /// Reserve cash for an order (called when order is ACKED)
    ///
    /// Returns error if insufficient funds
    pub fn reserve(&self, amount: Decimal) -> Result<()> {
        if amount <= Decimal::ZERO {
            return Err(BotError::Order("Cannot reserve zero or negative amount".into()));
        }

        let mut available = self.available.write().unwrap();
        let mut reserved = self.reserved.write().unwrap();

        if *available < amount {
            return Err(BotError::Order(format!(
                "Insufficient funds: need {}, have {}",
                amount, *available
            )));
        }

        *available -= amount;
        *reserved += amount;

        Ok(())
    }

    /// Release reserved cash (called on cancel/reject/expire)
    ///
    /// Returns the actual amount released (in case reserved was less)
    pub fn release(&self, amount: Decimal) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let mut available = self.available.write().unwrap();
        let mut reserved = self.reserved.write().unwrap();

        // Release up to reserved amount
        let actual_release = amount.min(*reserved);
        *reserved -= actual_release;
        *available += actual_release;

        actual_release
    }

    /// Transfer from reserved to realized (called on fill)
    ///
    /// This removes the cash from reserved but doesn't add it back to available
    /// (because it was spent on the fill)
    ///
    /// For a BUY fill: the reserved amount is spent
    /// For a SELL fill: we receive USDC, so available increases
    pub fn settle_buy(&self, amount: Decimal) -> Result<Decimal> {
        if amount <= Decimal::ZERO {
            return Err(BotError::Order("Cannot settle zero or negative amount".into()));
        }

        let mut reserved = self.reserved.write().unwrap();

        if *reserved < amount {
            // Partial reservation - shouldn't happen but handle gracefully
            let actual = *reserved;
            *reserved = Decimal::ZERO;
            return Ok(actual);
        }

        *reserved -= amount;
        Ok(amount)
    }

    /// Add proceeds from a sell fill
    pub fn settle_sell(&self, proceeds: Decimal) {
        if proceeds <= Decimal::ZERO {
            return;
        }

        let mut available = self.available.write().unwrap();
        *available += proceeds;
    }

    /// Deposit cash (add to available)
    pub fn deposit(&self, amount: Decimal) {
        if amount <= Decimal::ZERO {
            return;
        }

        let mut available = self.available.write().unwrap();
        *available += amount;
    }

    /// Withdraw cash (remove from available)
    pub fn withdraw(&self, amount: Decimal) -> Result<Decimal> {
        if amount <= Decimal::ZERO {
            return Err(BotError::Order("Cannot withdraw zero or negative amount".into()));
        }

        let mut available = self.available.write().unwrap();

        if *available < amount {
            return Err(BotError::Order(format!(
                "Insufficient funds for withdrawal: need {}, have {}",
                amount, *available
            )));
        }

        *available -= amount;
        Ok(amount)
    }

    /// Deduct fees from available
    pub fn deduct_fee(&self, fee: Decimal) {
        if fee <= Decimal::ZERO {
            return;
        }

        let mut available = self.available.write().unwrap();
        *available -= fee;
        // Allow going negative for fees (will show as debt)
    }

    /// Check if we can afford an order
    pub fn can_afford(&self, amount: Decimal) -> bool {
        self.available() >= amount
    }

    /// Get snapshot of all balances
    pub fn snapshot(&self) -> CashSnapshot {
        CashSnapshot {
            available: self.available(),
            reserved: self.reserved(),
            total: self.total(),
        }
    }

    /// Sync available cash against the exchange's reported total balance.
    ///
    /// The exchange returns the full USDC balance it holds for us (available + reserved
    /// from its perspective).  We reconstruct what our `available` should be as:
    ///   `exchange_total - reserved`
    ///
    /// If the drift between the computed available and our current available exceeds
    /// `threshold`, we correct it and return `Some(drift)` (positive = we were
    /// under-counting, negative = over-counting).  Returns `None` when within tolerance.
    pub fn sync_from_exchange(&self, exchange_total: Decimal, threshold: Decimal) -> Option<Decimal> {
        let mut available = self.available.write().unwrap();
        let reserved = self.reserved.read().unwrap();

        let exchange_available = (exchange_total - *reserved).max(Decimal::ZERO);
        let drift = exchange_available - *available;

        if drift.abs() > threshold {
            *available = exchange_available;
            Some(drift)
        } else {
            None
        }
    }

    /// Reset to initial state (for testing)
    pub fn reset(&self, initial: Decimal) {
        let mut available = self.available.write().unwrap();
        let mut reserved = self.reserved.write().unwrap();
        *available = initial;
        *reserved = Decimal::ZERO;
    }
}

/// Snapshot of cash balances
#[derive(Debug, Clone)]
pub struct CashSnapshot {
    pub available: Decimal,
    pub reserved: Decimal,
    pub total: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_initial_balance() {
        let cash = CashBalance::new(dec!(1000));
        assert_eq!(cash.available(), dec!(1000));
        assert_eq!(cash.reserved(), dec!(0));
        assert_eq!(cash.total(), dec!(1000));
    }

    #[test]
    fn test_reserve_and_release() {
        let cash = CashBalance::new(dec!(1000));

        // Reserve 300
        cash.reserve(dec!(300)).unwrap();
        assert_eq!(cash.available(), dec!(700));
        assert_eq!(cash.reserved(), dec!(300));
        assert_eq!(cash.total(), dec!(1000));

        // Release 100
        let released = cash.release(dec!(100));
        assert_eq!(released, dec!(100));
        assert_eq!(cash.available(), dec!(800));
        assert_eq!(cash.reserved(), dec!(200));
    }

    #[test]
    fn test_insufficient_funds() {
        let cash = CashBalance::new(dec!(100));

        let result = cash.reserve(dec!(200));
        assert!(result.is_err());
        assert_eq!(cash.available(), dec!(100)); // unchanged
    }

    #[test]
    fn test_settle_buy() {
        let cash = CashBalance::new(dec!(1000));

        // Reserve for order
        cash.reserve(dec!(500)).unwrap();
        assert_eq!(cash.reserved(), dec!(500));

        // Fill comes in - settle the purchase
        cash.settle_buy(dec!(500)).unwrap();
        assert_eq!(cash.reserved(), dec!(0));
        assert_eq!(cash.available(), dec!(500)); // 1000 - 500 spent
        assert_eq!(cash.total(), dec!(500));
    }

    #[test]
    fn test_settle_sell() {
        let cash = CashBalance::new(dec!(100));

        // Sell fill comes in - receive proceeds
        cash.settle_sell(dec!(250));
        assert_eq!(cash.available(), dec!(350));
        assert_eq!(cash.total(), dec!(350));
    }

    #[test]
    fn test_deposit_withdraw() {
        let cash = CashBalance::new(dec!(500));

        cash.deposit(dec!(200));
        assert_eq!(cash.available(), dec!(700));

        cash.withdraw(dec!(100)).unwrap();
        assert_eq!(cash.available(), dec!(600));

        // Can't withdraw more than available
        let result = cash.withdraw(dec!(1000));
        assert!(result.is_err());
    }

    #[test]
    fn test_fee_deduction() {
        let cash = CashBalance::new(dec!(100));

        cash.deduct_fee(dec!(0.50));
        assert_eq!(cash.available(), dec!(99.50));
    }

    #[test]
    fn test_can_afford() {
        let cash = CashBalance::new(dec!(100));

        assert!(cash.can_afford(dec!(50)));
        assert!(cash.can_afford(dec!(100)));
        assert!(!cash.can_afford(dec!(101)));

        // Reserve some
        cash.reserve(dec!(60)).unwrap();
        assert!(cash.can_afford(dec!(40)));
        assert!(!cash.can_afford(dec!(50)));
    }

    #[test]
    fn test_snapshot() {
        let cash = CashBalance::new(dec!(1000));
        cash.reserve(dec!(300)).unwrap();

        let snap = cash.snapshot();
        assert_eq!(snap.available, dec!(700));
        assert_eq!(snap.reserved, dec!(300));
        assert_eq!(snap.total, dec!(1000));
    }
}

//! Graceful shutdown — unwind positions and clean up tasks.

use super::Bot;
use crate::strategy::StrategyContext;
use tracing::info;

impl Bot {
    /// Graceful shutdown: runs strategy `on_shutdown` callbacks, cancels WS tasks.
    pub(crate) async fn shutdown(&mut self) {
        info!("Bot shutting down...");

        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);
        let shutdown_intents = self.strategy_router.on_shutdown(&ctx);
        if !shutdown_intents.is_empty() {
            info!("Processing {} shutdown intent(s)", shutdown_intents.len());
            self.process_intents(shutdown_intents);
        }

        self.market_ws_task.abort();
        if let Some(ref task) = self.user_ws_task {
            task.abort();
        }

        self.order_tracker.log_status();

        info!(
            "Final stats: {} intents | {} executions | {} fills",
            self.total_intents, self.total_executions, self.total_fills
        );
        info!("Bot shutdown complete");
    }
}

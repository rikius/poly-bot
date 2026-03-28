//! Alerting — fire webhook notifications on critical bot events.
//!
//! ## Supported backends
//!
//! - **Discord**: POSTs `{"content": "…"}` to a Discord webhook URL.
//! - **Telegram**: POSTs to the Telegram Bot API `sendMessage` endpoint.
//!
//! ## Configuration
//!
//! Set `ALERT_BACKEND=discord` or `ALERT_BACKEND=telegram` plus the
//! corresponding env vars:
//!
//! ```text
//! ALERT_BACKEND=discord
//! DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/…
//!
//! ALERT_BACKEND=telegram
//! TELEGRAM_BOT_TOKEN=1234567890:AAF…
//! TELEGRAM_CHAT_ID=-100…
//! ```
//!
//! If `ALERT_BACKEND` is absent or unrecognised, alerts are silently dropped.

use std::sync::Arc;

use reqwest::Client;
use serde_json::json;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// AlertBackend
// ---------------------------------------------------------------------------

/// The destination to which alert messages are dispatched.
///
/// All URL construction is done once at creation time so `send()` never
/// allocates a temporary string on the hot path.
#[derive(Debug, Clone)]
pub enum AlertBackend {
    /// Discord incoming webhook — POST `{"content": msg}`.
    Discord { url: String },
    /// Telegram bot — `endpoint` is the fully-formed `sendMessage` URL
    /// and `chat_id` is stored separately for the JSON body.
    Telegram { endpoint: String, chat_id: String },
}

// ---------------------------------------------------------------------------
// AlertSender
// ---------------------------------------------------------------------------

/// Sends fire-and-forget alert messages to the configured backend.
///
/// Errors are logged at `warn` level but never propagated — alerting failures
/// must not interrupt the trading loop.
pub struct AlertSender {
    client: Client,
    backend: AlertBackend,
}

impl AlertSender {
    /// Construct a new sender wrapping the given backend.
    pub fn new(backend: AlertBackend) -> Arc<Self> {
        Arc::new(Self {
            client: Client::new(),
            backend,
        })
    }

    // -----------------------------------------------------------------------
    // Core send
    // -----------------------------------------------------------------------

    /// Send a free-form alert message.  Errors are swallowed after logging.
    pub async fn send(&self, msg: &str) {
        let result = match &self.backend {
            AlertBackend::Discord { url } => {
                self.client
                    .post(url)
                    .json(&json!({"content": msg}))
                    .send()
                    .await
            }
            AlertBackend::Telegram { endpoint, chat_id } => {
                self.client
                    .post(endpoint)
                    .json(&json!({"chat_id": chat_id, "text": msg}))
                    .send()
                    .await
            }
        };

        match result {
            Ok(resp) if resp.status().is_success() => {
                debug!("Alert delivered");
            }
            Ok(resp) => {
                warn!("Alert delivery failed: HTTP {}", resp.status());
            }
            Err(e) => {
                warn!("Alert send error: {}", e);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Typed helpers
    // -----------------------------------------------------------------------

    /// Alert that the circuit breaker has tripped.
    pub async fn send_circuit_breaker_trip(&self, reason: &str) {
        self.send(&format!(
            "[polybot] \u{1F534} Circuit breaker OPEN: {}",
            reason
        ))
        .await;
    }

    /// Alert that the daily loss limit was exceeded.
    pub async fn send_daily_loss_exceeded(&self, loss_usd: &str, limit_usd: &str) {
        self.send(&format!(
            "[polybot] \u{1F6A8} Daily loss limit hit: ${} >= ${} — trading halted",
            loss_usd, limit_usd
        ))
        .await;
    }

    /// Alert that a WebSocket feed is reconnecting.
    pub async fn send_ws_reconnect(&self) {
        self.send("[polybot] \u{26A0}\u{FE0F} Market WebSocket reconnecting...")
            .await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discord_backend_constructs() {
        let sender = AlertSender::new(AlertBackend::Discord {
            url: "https://discord.com/api/webhooks/test/token".to_string(),
        });
        assert!(matches!(sender.backend, AlertBackend::Discord { .. }));
    }

    #[test]
    fn test_telegram_backend_constructs() {
        let sender = AlertSender::new(AlertBackend::Telegram {
            endpoint: "https://api.telegram.org/bot123:token/sendMessage".to_string(),
            chat_id: "-100123456".to_string(),
        });
        assert!(matches!(sender.backend, AlertBackend::Telegram { .. }));
    }

    #[test]
    fn test_discord_url_stored() {
        let url = "https://discord.com/api/webhooks/999/abc";
        let sender = AlertSender::new(AlertBackend::Discord {
            url: url.to_string(),
        });
        if let AlertBackend::Discord { url: stored } = &sender.backend {
            assert_eq!(stored, url);
        } else {
            panic!("expected Discord backend");
        }
    }

    #[test]
    fn test_telegram_fields_stored() {
        let endpoint = "https://api.telegram.org/bot99:abc/sendMessage";
        let sender = AlertSender::new(AlertBackend::Telegram {
            endpoint: endpoint.to_string(),
            chat_id: "-100777".to_string(),
        });
        if let AlertBackend::Telegram { endpoint: ep, chat_id } = &sender.backend {
            assert_eq!(ep, endpoint);
            assert_eq!(chat_id, "-100777");
        } else {
            panic!("expected Telegram backend");
        }
    }
}

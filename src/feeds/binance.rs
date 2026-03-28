//! Binance aggTrade WebSocket feed
//!
//! Connects to `wss://stream.binance.com/stream?streams=…` and streams
//! aggregated trade prices for BTC, ETH and SOL into the shared
//! [`ExternalPriceStore`].
//!
//! ## Reconnection
//!
//! On any WS error or disconnection the task sleeps 5 s and reconnects.
//! During the reconnect window the price records remain in the store but
//! `is_fresh()` will return `false` after `max_age_secs`.
//!
//! ## Combined stream format
//!
//! Binance combined streams wrap each event:
//! ```json
//! { "stream": "btcusdt@aggTrade",
//!   "data": { "s": "BTCUSDT", "p": "50000.00", … } }
//! ```

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::feeds::{ExternalPriceRecord, ExternalPriceStore};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Which assets to track.  Extend as needed.
pub const TRACKED_SYMBOLS: &[(&str, &str)] = &[
    ("BTCUSDT", "BTC"),
    ("ETHUSDT", "ETH"),
    ("SOLUSDT", "SOL"),
];

/// Rolling window for price change calculation (seconds).
const WINDOW_SECS: u64 = 30;

/// Reconnect delay after a disconnection.
const RECONNECT_DELAY_SECS: u64 = 5;

/// Binance combined stream base URL.
const WS_BASE: &str = "wss://stream.binance.com/stream?streams=";

// ---------------------------------------------------------------------------
// BinanceFeed
// ---------------------------------------------------------------------------

/// Connects to Binance aggTrade WebSocket and updates the shared price store.
pub struct BinanceFeed {
    prices: ExternalPriceStore,
}

impl BinanceFeed {
    /// Create a feed backed by `prices`.
    pub fn new(prices: ExternalPriceStore) -> Arc<Self> {
        Arc::new(Self { prices })
    }

    /// Run the feed forever, reconnecting with exponential backoff on failure.
    ///
    /// Back-off schedule: 5 s, 10 s, 20 s, 40 s, 60 s (cap), 60 s, …
    /// A successful connection (stream stayed up > 0 messages) resets the delay
    /// to the initial value.
    ///
    /// Spawn this with `tokio::spawn`.
    pub async fn run(&self) {
        const MAX_DELAY_SECS: u64 = 60;

        let streams: String = TRACKED_SYMBOLS
            .iter()
            .map(|(ticker, _)| format!("{}@aggTrade", ticker.to_lowercase()))
            .collect::<Vec<_>>()
            .join("/");
        let url = format!("{}{}", WS_BASE, streams);

        info!("Binance feed connecting: {}", url);

        let mut delay_secs = RECONNECT_DELAY_SECS;

        loop {
            match self.connect_and_stream(&url).await {
                Ok(()) => {
                    // Clean disconnect — stream was alive and then closed
                    warn!("Binance WS stream ended cleanly — reconnecting in {}s", delay_secs);
                    // Reset backoff: clean disconnects are transient
                    delay_secs = RECONNECT_DELAY_SECS;
                }
                Err(e) => {
                    warn!("Binance WS error: {} — reconnecting in {}s", e, delay_secs);
                    // Increase delay for next attempt (exponential, capped)
                    delay_secs = (delay_secs * 2).min(MAX_DELAY_SECS);
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
    }

    async fn connect_and_stream(&self, url: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (ws_stream, _response) = connect_async(url).await?;
        let (_, mut read) = ws_stream.split();

        info!("Binance WS connected");

        while let Some(msg) = read.next().await {
            let msg = msg?;
            match msg {
                Message::Text(text) => {
                    self.process_message(&text);
                }
                Message::Ping(_) => {
                    // tungstenite auto-responds to pings
                    debug!("Binance WS: ping received");
                }
                Message::Close(_) => {
                    info!("Binance WS: close frame received");
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn process_message(&self, text: &str) {
        #[derive(Deserialize)]
        struct CombinedMessage {
            data: AggTradeData,
        }

        #[derive(Deserialize)]
        struct AggTradeData {
            #[serde(rename = "s")]
            symbol: String,
            #[serde(rename = "p")]
            price: String,
        }

        let msg: CombinedMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(_) => return,
        };

        let new_price: Decimal = match msg.data.price.parse() {
            Ok(p) => p,
            Err(_) => return,
        };

        let asset = match binance_symbol_to_asset(&msg.data.symbol) {
            Some(a) => a,
            None => return,
        };

        // Update or insert the record
        if let Some(mut record) = self.prices.get_mut(asset) {
            record.update(new_price, WINDOW_SECS);
        } else {
            self.prices.insert(
                asset.to_string(),
                ExternalPriceRecord::new(asset, new_price),
            );
        }

        debug!(
            asset = asset,
            price = %new_price,
            "Binance price update"
        );
    }
}

/// Map Binance ticker symbol to canonical asset key ("BTC", "ETH", "SOL").
fn binance_symbol_to_asset(symbol: &str) -> Option<&'static str> {
    for (ticker, asset) in TRACKED_SYMBOLS {
        if symbol.eq_ignore_ascii_case(ticker) {
            return Some(asset);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Helper exposed for tests / TemporalArbStrategy
// ---------------------------------------------------------------------------

/// Detect the asset symbol ("BTC", "ETH", "SOL") from a market slug or description.
///
/// Checks for case-insensitive substring matches against the tracked symbols.
pub fn detect_asset_from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if lower.contains("btc") || lower.contains("bitcoin") {
        Some("BTC".to_string())
    } else if lower.contains("eth") || lower.contains("ethereum") || lower.contains("ether") {
        Some("ETH".to_string())
    } else if lower.contains("sol") || lower.contains("solana") {
        Some("SOL".to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_asset_btc() {
        assert_eq!(detect_asset_from_text("btc-updown-15m-123"), Some("BTC".to_string()));
        assert_eq!(detect_asset_from_text("BTC Up Down 15m"), Some("BTC".to_string()));
        assert_eq!(detect_asset_from_text("Will Bitcoin be higher?"), Some("BTC".to_string()));
    }

    #[test]
    fn test_detect_asset_eth() {
        assert_eq!(detect_asset_from_text("eth-updown-15m"), Some("ETH".to_string()));
        assert_eq!(detect_asset_from_text("Ethereum up?"), Some("ETH".to_string()));
    }

    #[test]
    fn test_detect_asset_sol() {
        assert_eq!(detect_asset_from_text("sol-updown-15m"), Some("SOL".to_string()));
        assert_eq!(detect_asset_from_text("Solana market"), Some("SOL".to_string()));
    }

    #[test]
    fn test_detect_asset_unknown() {
        assert_eq!(detect_asset_from_text("doge-updown"), None);
        assert_eq!(detect_asset_from_text("Will it rain?"), None);
    }

    #[test]
    fn test_binance_symbol_map() {
        assert_eq!(binance_symbol_to_asset("BTCUSDT"), Some("BTC"));
        assert_eq!(binance_symbol_to_asset("ETHUSDT"), Some("ETH"));
        assert_eq!(binance_symbol_to_asset("SOLUSDT"), Some("SOL"));
        assert_eq!(binance_symbol_to_asset("DOGEUSDT"), None);
    }
}

//! Startup latency probe — picks the lowest-RTT Polymarket endpoint.
//!
//! At startup, fires concurrent HTTP GET /time requests to each candidate
//! endpoint, measures round-trip time, and selects the fastest one.
//! All HTTP and WebSocket clients are then configured to use that endpoint.

use reqwest::Client;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Result of the latency probe — the winning endpoint and its median RTT
#[derive(Debug, Clone)]
pub struct SelectedEndpoint {
    /// Base URL to use for all CLOB API calls
    pub url: String,
    /// Median round-trip time in milliseconds
    pub rtt_ms: u64,
}

/// Candidate endpoints to probe.
/// The primary CLOB URL is always first (used as fallback if all probes fail).
const CANDIDATES: &[&str] = &[
    "https://clob.polymarket.com",
    "https://clob.polymarket.com", // probe twice — different DNS resolution paths
];

const PROBE_COUNT: usize = 3;
const PROBE_TIMEOUT_MS: u64 = 2000;

/// Probe all candidate endpoints and return the one with the lowest median RTT.
///
/// Fires `PROBE_COUNT` sequential GET /time requests per candidate (in parallel
/// across candidates), computes the median RTT, and selects the winner.
pub async fn probe_best_endpoint() -> SelectedEndpoint {
    let client = match build_probe_client() {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to build probe client, using default endpoint: {}", e);
            return SelectedEndpoint {
                url: CANDIDATES[0].to_string(),
                rtt_ms: 0,
            };
        }
    };

    info!("Probing {} candidate endpoint(s)...", CANDIDATES.len());

    let mut results: Vec<(String, u64)> = Vec::new();

    // Deduplicate candidates while preserving order
    let mut seen = std::collections::HashSet::new();
    let unique_candidates: Vec<&str> = CANDIDATES
        .iter()
        .copied()
        .filter(|c| seen.insert(*c))
        .collect();

    // Probe each unique candidate
    let probe_futures: Vec<_> = unique_candidates
        .iter()
        .map(|&url| probe_endpoint(&client, url))
        .collect();

    let probe_results = futures_util::future::join_all(probe_futures).await;

    for (url, maybe_rtt) in unique_candidates.iter().zip(probe_results) {
        match maybe_rtt {
            Some(rtt) => {
                info!(url = *url, rtt_ms = rtt, "Endpoint probe result");
                results.push((url.to_string(), rtt));
            }
            None => {
                warn!(url = *url, "Endpoint probe failed or timed out");
            }
        }
    }

    // Select the endpoint with lowest median RTT
    if let Some((url, rtt)) = results.into_iter().min_by_key(|(_, rtt)| *rtt) {
        info!(
            url = %url,
            rtt_ms = rtt,
            "Selected endpoint (lowest RTT)"
        );
        SelectedEndpoint { url, rtt_ms: rtt }
    } else {
        warn!("All endpoint probes failed — using default endpoint");
        SelectedEndpoint {
            url: CANDIDATES[0].to_string(),
            rtt_ms: 0,
        }
    }
}

/// Probe a single endpoint with multiple samples and return the median RTT.
async fn probe_endpoint(client: &Client, base_url: &str) -> Option<u64> {
    let url = format!("{}/time", base_url);
    let mut samples: Vec<u64> = Vec::with_capacity(PROBE_COUNT);

    for _ in 0..PROBE_COUNT {
        let start = Instant::now();
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 200 => {
                samples.push(start.elapsed().as_millis() as u64);
            }
            Ok(resp) => {
                warn!(
                    url = %url,
                    status = resp.status().as_u16(),
                    "Probe got unexpected status"
                );
                samples.push(start.elapsed().as_millis() as u64);
            }
            Err(e) => {
                warn!(url = %url, error = %e, "Probe request failed");
            }
        }
    }

    if samples.is_empty() {
        return None;
    }

    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    Some(median)
}

fn build_probe_client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .timeout(Duration::from_millis(PROBE_TIMEOUT_MS))
        .connect_timeout(Duration::from_millis(1000))
        .tcp_nodelay(true)
        .use_rustls_tls()
        .build()
}

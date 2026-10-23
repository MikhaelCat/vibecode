//! Overpass API access with a mirror list and a real time budget.
//!
//! Overpass is the only source that can filter by tag across a whole city in
//! one request, so it answers what the small map-API tiles cannot: the venues
//! outside the centre, and sparse tags such as camp sites. It is also the least
//! predictable source — the public instances queue heavy queries, and from this
//! network only two of the four mirrors answer at all, in 15-45 seconds.
//!
//! Two rules follow from that. Every request carries the configured
//! User-Agent, because the main instance answers `406 Not Acceptable` to a bare
//! HTTP client. And the caller decides how long to wait: Overpass runs next to
//! the fast sources and only its answer within the deadline is used.

use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::Value;

use crate::tools::providers::OVERPASS_SLOTS;

/// Public instances to try, in order. The main instance is the most complete;
/// the second one answers from the Russian network when the others queue.
pub const ENDPOINTS: &[&str] = &[
    "https://overpass-api.de/api/interpreter",
    "https://maps.mail.ru/osm/tools/overpass/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
    "https://overpass.private.coffee/api/interpreter",
];

/// One mirror may hold the request no longer than this before the next one is
/// tried; the caller's deadline always wins over it.
const MIRROR_TIMEOUT: Duration = Duration::from_secs(7);

/// Runs one Overpass query and returns the parsed JSON, or `None` when no
/// mirror answered inside the deadline. A mirror that answers with an error or
/// with something that is not Overpass output is skipped, not trusted.
///
/// The client carries the configured User-Agent: the main instance answers
/// `406 Not Acceptable` to a request without one.
pub async fn query(http: Client, body: &str, deadline: Instant, budget: Duration) -> Option<Value> {
    let _permit = OVERPASS_SLOTS.acquire().await.ok()?;
    for endpoint in ENDPOINTS {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .min(budget);
        if left.is_zero() {
            return None;
        }
        let response = tokio::time::timeout(
            left.min(MIRROR_TIMEOUT),
            http.post(*endpoint).form(&[("data", body)]).send(),
        )
        .await
        .ok()
        .and_then(Result::ok);
        let Some(response) = response else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(value) = response.json::<Value>().await else {
            continue;
        };
        if value.get("elements").and_then(Value::as_array).is_some() {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_are_unique_and_https() {
        let mut endpoints = ENDPOINTS.to_vec();
        endpoints.sort_unstable();
        endpoints.dedup();
        assert_eq!(endpoints.len(), ENDPOINTS.len());
        assert!(ENDPOINTS.iter().all(|url| url.starts_with("https://")));
    }
}

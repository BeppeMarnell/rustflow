use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use futures_util::{stream, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Default)]
struct Observation {
    queue_ms: Option<i64>,
    first_token_ms: Option<i64>,
    completion_ms: Option<i64>,
    error: bool,
    provider_throttles: usize,
}

fn elapsed_ms(start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    end.signed_duration_since(start).num_milliseconds().max(0)
}

fn percentile(values: &[i64], percentage: usize) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() * percentage).div_ceil(100).saturating_sub(1);
    sorted.get(index).copied()
}

async fn one(
    client: Client,
    base: String,
    key: String,
    version: Uuid,
    index: usize,
) -> Result<Observation> {
    let created = client.post(format!("{base}/v1/runs"))
        .bearer_auth(&key)
        .json(&json!({"flow_version_id": version, "input":{"message":format!("Load test request {index}")}}))
        .send().await?;
    if !created.status().is_success() {
        return Err(anyhow!("run creation returned {}", created.status()));
    }
    let body: Value = created.json().await?;
    let run_id = body["run_id"].as_str().context("missing run_id")?;
    let mut finished = false;
    let mut error = false;
    for _ in 0..600 {
        let response: Value = client
            .get(format!("{base}/v1/runs/{run_id}"))
            .bearer_auth(&key)
            .send()
            .await?
            .json()
            .await?;
        match response["status"].as_str() {
            Some("succeeded") => {
                finished = true;
                break;
            }
            Some("failed" | "cancelled") => {
                error = true;
                finished = true;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    if !finished {
        return Err(anyhow!("run timed out"));
    }
    let timeline: Value = client
        .get(format!("{base}/v1/runs/{run_id}/timeline"))
        .bearer_auth(&key)
        .send()
        .await?
        .json()
        .await?;
    let events = timeline["events"].as_array().context("missing events")?;
    let time = |kind: &str| -> Option<DateTime<Utc>> {
        events
            .iter()
            .find(|event| event["event_type"] == kind)
            .and_then(|event| event["occurred_at"].as_str())
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|value| value.with_timezone(&Utc))
    };
    let queued = time("run.queued");
    let started = time("run.started");
    let first_token = time("token");
    let completed = time("run.completed").or_else(|| time("run.failed"));
    Ok(Observation {
        queue_ms: queued.zip(started).map(|(a, b)| elapsed_ms(a, b)),
        first_token_ms: queued.zip(first_token).map(|(a, b)| elapsed_ms(a, b)),
        completion_ms: queued.zip(completed).map(|(a, b)| elapsed_ms(a, b)),
        error,
        provider_throttles: events
            .iter()
            .filter(|event| {
                event["event_type"] == "model.failure" && event["metadata"]["throttled"] == true
            })
            .count(),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let base = std::env::var("CHATFLOW_API_URL").context("CHATFLOW_API_URL required")?;
    let key = std::env::var("CHATFLOW_API_KEY").context("CHATFLOW_API_KEY required")?;
    let version: Uuid = std::env::var("CHATFLOW_FLOW_VERSION_ID")?.parse()?;
    let concurrency: usize = std::env::var("LOAD_CONCURRENCY")
        .unwrap_or_else(|_| "20".into())
        .parse()?;
    let count: usize = std::env::var("LOAD_COUNT")
        .unwrap_or_else(|_| "100".into())
        .parse()?;
    if !(1..=200).contains(&concurrency) || count == 0 {
        return Err(anyhow!("invalid load settings"));
    }
    let client = Client::new();
    let observations = stream::iter(0..count)
        .map(|index| one(client.clone(), base.clone(), key.clone(), version, index))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    let failures = observations
        .iter()
        .filter(|result| result.as_ref().map_or(true, |o| o.error))
        .count();
    let successful = observations
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect::<Vec<_>>();
    let queue = successful
        .iter()
        .filter_map(|o| o.queue_ms)
        .collect::<Vec<_>>();
    let first = successful
        .iter()
        .filter_map(|o| o.first_token_ms)
        .collect::<Vec<_>>();
    let completion = successful
        .iter()
        .filter_map(|o| o.completion_ms)
        .collect::<Vec<_>>();
    let throttles: usize = successful.iter().map(|o| o.provider_throttles).sum();
    let report = json!({
        "target_concurrency": concurrency, "requests": count,
        "errors": failures, "error_rate": failures as f64 / count as f64,
        "queue_time_p95_ms": percentile(&queue, 95),
        "first_token_latency_p95_ms": percentile(&first, 95),
        "completion_latency_p95_ms": percentile(&completion, 95),
        "provider_throttles": throttles,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentile_handles_small_and_empty_samples() {
        assert_eq!(percentile(&[], 95), None);
        assert_eq!(percentile(&[8], 95), Some(8));
        assert_eq!(percentile(&[1, 3, 2, 4, 5], 95), Some(5));
        assert_eq!(percentile(&[1, 3, 2, 4, 5], 50), Some(3));
    }
}

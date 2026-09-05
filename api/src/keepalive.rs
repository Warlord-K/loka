use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

const INTERVAL: Duration = Duration::from_secs(240);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub fn spawn() -> anyhow::Result<()> {
    let Ok(base_url) = std::env::var("SELF_URL") else {
        return Ok(());
    };
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Ok(());
    }

    let target = format!("{base_url}/health");
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    info!(target = %target, interval_seconds = INTERVAL.as_secs(), "Self keep-alive enabled");

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            interval.tick().await;
            match client.get(&target).send().await {
                Ok(response) if response.status().is_success() => tracing::debug!(
                    target = %target,
                    status = %response.status(),
                    "Self keep-alive succeeded"
                ),
                Ok(response) => warn!(
                    target = %target,
                    status = %response.status(),
                    "Self keep-alive failed"
                ),
                Err(error) => warn!(target = %target, error = %error, "Self keep-alive failed"),
            }
        }
    });

    Ok(())
}

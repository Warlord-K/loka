use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use serde_json::Value;
use tracing::{info, warn};

pub type Rates = [f64; 3];

pub const SOURCE_URL: &str = "https://openrouter.ai/api/v1/models";
const REFRESH: Duration = Duration::from_secs(12 * 3600);
const RETRY: Duration = Duration::from_secs(30 * 60);
const EFFORT_SUFFIXES: [&str; 10] = ["fast", "thinking", "minimal", "none", "low", "medium", "high", "xhigh", "max", "latest"];

const SNAPSHOT: &[(&str, Rates)] = &[
    ("composer-2.5", [0.5, 0.05, 2.5]),
    ("composer-2.5-fast", [3.0, 0.3, 15.0]),
    ("composer-2", [0.5, 0.05, 2.5]),
    ("composer-2-fast", [3.0, 0.3, 15.0]),
    ("claude-fable-5.2", [8.0, 0.2, 40.0]),
    ("claude-fable-5.1", [10.0, 0.25, 50.0]),
    ("claude-fable-5", [10.0, 1.0, 50.0]),
    ("claude-opus-5.5", [4.0, 0.2, 20.0]),
    ("claude-opus-5", [5.0, 0.5, 25.0]),
    ("claude-sonnet-5.5", [2.0, 0.1, 10.0]),
    ("claude-sonnet-5", [2.0, 0.2, 10.0]),
    ("claude-haiku-5.5", [0.1, 0.01, 0.5]),
    ("gpt-5.6-sol", [4.4, 0.44, 22.0]),
    ("gpt-5.6-terra", [2.0, 0.2, 12.0]),
    ("gpt-5.6-luna", [0.2, 0.02, 1.2]),
    ("gpt-5.5", [5.0, 0.5, 30.0]),
    ("gpt-5.3-codex", [1.75, 0.175, 14.0]),
    ("gpt-5.2-codex", [1.75, 0.175, 14.0]),
    ("gpt-5.2", [1.75, 0.175, 14.0]),
    ("grok-4.7", [2.0, 0.5, 6.0]),
    ("grok-4.6", [2.0, 0.5, 6.0]),
    ("grok-4.5", [2.0, 0.5, 6.0]),
    ("kimi-k3", [0.19, 0.18, 16.0]),
];

#[derive(Default)]
struct Catalog {
    listed: HashMap<String, Rates>,
    overrides: HashMap<String, Rates>,
    fetched: bool,
}

fn catalog() -> &'static RwLock<Catalog> {
    static CATALOG: OnceLock<RwLock<Catalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let listed = SNAPSHOT.iter().map(|(name, rates)| (canonical(name), *rates)).collect();
        let overrides = std::env::var("WORLD_PRICING").ok().map(|raw| parse_overrides(&raw)).unwrap_or_default();
        RwLock::new(Catalog { listed, overrides, fetched: false })
    })
}

fn parse_overrides(raw: &str) -> HashMap<String, Rates> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) else {
        if !raw.trim().is_empty() {
            warn!("WORLD_PRICING is not a JSON object of model to [input, cached, output] dollars per million tokens");
        }
        return HashMap::new();
    };
    map.into_iter()
        .filter_map(|(model, rates)| {
            let rates = rates.as_array()?;
            let rate = |i: usize| rates.get(i).and_then(Value::as_f64).unwrap_or(0.0);
            Some((model.trim().to_ascii_lowercase(), [rate(0), rate(1), rate(2)]))
        })
        .collect()
}

pub(crate) fn canonical(name: &str) -> String {
    let name = name.trim().to_ascii_lowercase();
    let name = name.split('[').next().unwrap_or_default();
    let name = name.rsplit('/').next().unwrap_or_default();
    let name = name.strip_prefix("cursor-").unwrap_or(name);
    let mut parts: Vec<&str> = name.split('-').collect();
    if parts.last().is_some_and(|p| p.len() == 8 && p.bytes().all(|b| b.is_ascii_digit())) {
        parts.pop();
    }
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < parts.len() {
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(parts[i]);
        let single_digit = |p: &str| p.len() == 1 && p.bytes().all(|b| b.is_ascii_digit());
        if single_digit(parts[i]) && i + 1 < parts.len() && single_digit(parts[i + 1]) {
            out.push('.');
            out.push_str(parts[i + 1]);
            i += 1;
        }
        i += 1;
    }
    out
}

fn candidates(model: &str) -> Vec<String> {
    let mut name = canonical(model);
    let mut out = vec![name.clone()];
    while let Some((head, tail)) = name.rsplit_once('-') {
        if !EFFORT_SUFFIXES.contains(&tail) || head.is_empty() {
            break;
        }
        name = head.to_string();
        out.push(name.clone());
    }
    out
}

pub fn rates(model: &str) -> Option<Rates> {
    let model = model.trim();
    if model.is_empty() || model.eq_ignore_ascii_case("auto") || model.eq_ignore_ascii_case("default") {
        return None;
    }
    let catalog = catalog().read().unwrap();
    if let Some(rates) = catalog.overrides.get(&model.to_ascii_lowercase()) {
        return Some(*rates);
    }
    candidates(model).iter().find_map(|name| catalog.overrides.get(name).or_else(|| catalog.listed.get(name)).copied())
}

pub fn cost(model: &str, input: u64, cached: u64, output: u64) -> f64 {
    let Some([i, c, o]) = rates(model) else {
        return 0.0;
    };
    (input as f64 * i + cached as f64 * c + output as f64 * o) / 1_000_000.0
}

pub fn has_price(model: &str) -> bool {
    rates(model).is_some()
}

fn per_million(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    let per_token = value.as_f64().or_else(|| value.as_str()?.parse().ok())?;
    (per_token >= 0.0).then_some(per_token * 1_000_000.0)
}

fn parse_listing(body: &Value) -> HashMap<String, Rates> {
    let mut out = HashMap::new();
    for model in body.get("data").and_then(Value::as_array).into_iter().flatten() {
        let Some(id) = model.get("id").and_then(Value::as_str) else { continue };
        if id.starts_with('~') || id.contains(':') {
            continue;
        }
        let pricing = model.get("pricing");
        let field = |key: &str| per_million(pricing.and_then(|p| p.get(key)));
        let (Some(input), Some(output)) = (field("prompt"), field("completion")) else { continue };
        if input == 0.0 && output == 0.0 {
            continue;
        }
        out.entry(canonical(id)).or_insert([input, field("input_cache_read").unwrap_or(input), output]);
    }
    out
}

async fn refresh_once(client: &reqwest::Client) -> anyhow::Result<usize> {
    let bytes = client.get(SOURCE_URL).send().await?.error_for_status()?.bytes().await?;
    let body: Value = serde_json::from_slice(&bytes)?;
    let listed = parse_listing(&body);
    if listed.is_empty() {
        anyhow::bail!("the price list was empty");
    }
    let count = listed.len();
    let mut catalog = catalog().write().unwrap();
    for (name, rates) in listed {
        catalog.listed.insert(name, rates);
    }
    catalog.fetched = true;
    Ok(count)
}

pub fn fetch_enabled() -> bool {
    !matches!(std::env::var("WORLD_PRICING_FETCH").ok().as_deref().map(str::trim), Some("off" | "0" | "false" | "no"))
}

pub fn fetched() -> bool {
    catalog().read().unwrap().fetched
}

pub fn spawn_refresh() {
    let _ = catalog();
    if !fetch_enabled() {
        return;
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let client = match reqwest::Client::builder().timeout(Duration::from_secs(20)).user_agent("loka-pricing").build() {
            Ok(client) => client,
            Err(error) => return warn!(%error, "Could not build the price list client"),
        };
        loop {
            let wait = match refresh_once(&client).await {
                Ok(count) => {
                    info!(count, "Model price list refreshed");
                    REFRESH
                }
                Err(error) => {
                    warn!(error = %format!("{error:#}"), "Could not refresh the model price list, using built-in prices");
                    RETRY
                }
            };
            tokio::time::sleep(wait).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_names_match_the_price_list() {
        assert_eq!(canonical("claude-opus-5-5"), "claude-opus-5.5");
        assert_eq!(canonical("anthropic/claude-opus-5.5"), "claude-opus-5.5");
        assert_eq!(canonical("claude-opus-5-5[context=300k,effort=medium,fast=false]"), "claude-opus-5.5");
        assert_eq!(canonical("cursor-grok-4.6-high"), "grok-4.6-high");
        assert_eq!(canonical("claude-sonnet-4-5-20250929"), "claude-sonnet-4.5");
        assert_eq!(canonical("fal/kimi-k3"), "kimi-k3");
        assert_eq!(canonical("gpt-5.3-codex"), "gpt-5.3-codex");
    }

    #[test]
    fn effort_and_speed_suffixes_fall_back_to_the_base_model() {
        assert_eq!(rates("claude-opus-5-5-high-fast"), rates("claude-opus-5-5"));
        assert_eq!(rates("claude-opus-5-thinking-xhigh"), Some([5.0, 0.5, 25.0]));
        assert_eq!(rates("gpt-5.3-codex-xhigh"), Some([1.75, 0.175, 14.0]));
        assert_eq!(rates("composer-2.5-fast"), Some([3.0, 0.3, 15.0]));
        assert_eq!(rates("composer-2.5"), Some([0.5, 0.05, 2.5]));
        assert_eq!(rates("auto"), None);
        assert_eq!(rates("made-up-model"), None);
    }

    #[test]
    fn costs_use_dollars_per_million_tokens() {
        let dollars = cost("composer-2.5", 1_000_000, 1_000_000, 1_000_000);
        assert!((dollars - 3.05).abs() < 1e-9, "{dollars}");
        assert_eq!(cost("auto", 1000, 0, 1000), 0.0);
    }

    #[test]
    fn reads_the_public_price_list() {
        let listed = parse_listing(&json!({"data": [
            {"id": "anthropic/claude-opus-9", "pricing": {"prompt": "0.000004", "completion": "0.00002", "input_cache_read": "0.0000002"}},
            {"id": "anthropic/claude-opus-9:batch", "pricing": {"prompt": "0.000002", "completion": "0.00001"}},
            {"id": "~anthropic/claude-latest", "pricing": {"prompt": "0.1", "completion": "0.1"}},
            {"id": "x-ai/grok-free", "pricing": {"prompt": "0", "completion": "0"}},
            {"id": "x-ai/grok-9", "pricing": {"prompt": "0.000002", "completion": "0.000006"}}
        ]}));
        assert_eq!(listed.len(), 2);
        let opus = listed["claude-opus-9"];
        assert!((opus[0] - 4.0).abs() < 1e-9 && (opus[1] - 0.2).abs() < 1e-9 && (opus[2] - 20.0).abs() < 1e-9);
        assert!((listed["grok-9"][1] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn overrides_take_exact_names_and_dollars_per_million() {
        let parsed = parse_overrides(r#"{"My-Model": [1, 0.5, 2], "bad": "x"}"#);
        assert_eq!(parsed.get("my-model"), Some(&[1.0, 0.5, 2.0]));
        assert!(!parsed.contains_key("bad"));
    }

    #[tokio::test]
    #[ignore]
    async fn live_price_list_covers_cursor_and_codex_models() {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().unwrap();
        let count = refresh_once(&client).await.unwrap();
        assert!(count > 50, "{count}");
        for model in ["claude-opus-5-5-high", "gpt-5.3-codex-xhigh", "gpt-5.6-sol-high", "grok-4.7-low", "claude-sonnet-5-thinking-high", "composer-2.5"] {
            assert!(has_price(model), "{model}");
            eprintln!("PRICE {model} {:?}", rates(model));
        }
    }
}

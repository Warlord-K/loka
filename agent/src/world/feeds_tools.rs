use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use loka_shared::feed_auth::AuthState;
use loka_shared::feed_intake::{self, IntakeChange, Outcome};
use loka_shared::feeds::{self, AgentFeed, Fetched, FeedItem, NewFeed};
use loka_shared::world::{WorldAgent, WorldStore};
use serde_json::{json, Value};

pub fn tools(tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let agent = s("Agent name or slug, optional, defaults to you; another agent needs manage_agents");
    let feed = s("Feed id (or its first 8 characters) or name, from list_feeds");
    let lines = |d: &str| json!({ "type": "array", "items": { "type": "string" }, "description": d });
    let source = s("For webhooks, required: what will send to it. github, gitlab, alertmanager and grafana come with ready-made checks; anything else is a short name like Statuspage or \"our deploy script\". If you do not know what will send it, ask your person before making the webhook");
    let auth = s("For webhooks: secret (default; every call must carry the feed's secret) or address (calls from allowed_ips are accepted without a secret, for senders that cannot send one; needs allowed_ips and only works while your verified owner asks for it in this turn)");
    let require = lines("For webhooks: checks every call must pass or it is turned away before you see it. One per entry: a field that must be there (\"action\"), a field with allowed values (\"repository.full_name=acme/api|acme/web\"), a field containing text (\"title~deploy\"), or a header (\"header:x-github-event=pull_request|issues\"). Fields are paths into the JSON body; * walks a list (\"alerts.*.labels.severity=critical\")");
    let skip = lines("For webhooks: calls matching any of these are dropped quietly, same syntax as require, e.g. \"sender.type=Bot\"");
    let keep = lines("For webhooks: the only fields that reach you, as paths like \"pull_request.title\" or \"header:x-github-event\"; everything else in the call is thrown away. Empty list goes back to the whole body cut to 1500 characters");
    let title = s("For webhooks: one-line title built from fields, e.g. \"Deploy {status} on {environment}\"; none clears it");
    let link = lines("For webhooks: fields that may hold the link to show, first one present wins");
    let json_only = json!({ "type": "boolean", "description": "For webhooks: true turns away any call whose body is not JSON" });
    vec![
        tool(
            "add_feed",
            "Start watching a feed so new items wake you as one feed.item event per batch, instead of checking on a timer. kind rss (default when url is set): any RSS, Atom or JSON Feed URL; it is fetched now to check it works, everything already in it counts as seen, and only items that appear later reach you. Use this when your person says \"watch this feed\". Webhook feeds are part of Loka Pro, since this app runs on your person's Mac where outside services cannot reach it.",
            json!({
                "url": s("RSS, Atom or JSON Feed URL, for kind rss"),
                "kind": s("rss"),
                "name": s("Short name, defaults to the feed's title"),
                "filter": s("Optional comma-separated keywords: an item is kept when any appears in its title, summary or link; terms starting with - drop items, e.g. \"critical, high, -wordpress\""),
                "every_minutes": { "type": "integer", "description": "How often to check an rss feed, 1 to 1440 minutes, default 15" },
                "format": s("For webhooks: auto (default; detects Alertmanager, else JSON, else text), alertmanager, json or text"),
                "allowed_ips": s("For webhooks, optional: comma-separated IP addresses or CIDR ranges the sender posts from, e.g. \"203.0.113.0/24, 2001:db8::/32\"; calls from anywhere else are refused even with the secret"),
                "source": source.clone(),
                "auth": auth.clone(),
                "require": require.clone(),
                "skip": skip.clone(),
                "keep": keep.clone(),
                "title": title.clone(),
                "link": link.clone(),
                "json_only": json_only.clone(),
                "agent": agent.clone()
            }),
            &[],
        ),
        tool("list_feeds", "List the feeds you (or another agent of your person) watch: kind, source or webhook URL, filter, how often, last item, errors. For webhooks also who sends it, how the sender is verified, its checks, the fields kept, the field names its last call carried (names only, never values) and how many calls its checks turned away. Secrets are never listed.", json!({ "agent": agent.clone() }), &[]),
        tool("remove_feed", "Stop watching a feed for good. A removed webhook URL stops working.", json!({ "feed": feed.clone(), "agent": agent.clone() }), &["feed"]),
        tool(
            "update_feed",
            "Change a feed you (or another agent of your person) watch without losing it: rename it, change or clear its filter, how often an rss feed is checked, a webhook's format, allowed sender addresses, sender, checks and kept fields, or rotate a webhook's secret (the old one stops working at once and your person sees the new one, once, in the app). A webhook keeps its URL. A list you pass replaces the old one.",
            json!({
                "feed": feed.clone(),
                "name": s("New name, optional"),
                "filter": s("New comma-separated keywords like add_feed takes; none or an empty string clears it"),
                "every_minutes": { "type": "integer", "description": "How often to check an rss feed, 1 to 1440 minutes" },
                "format": s("For webhooks: auto, alertmanager, json or text"),
                "allowed_ips": s("For webhooks: comma-separated IP addresses or CIDR ranges allowed to post; none or an empty string allows any address (the secret is still required)"),
                "rotate_secret": { "type": "boolean", "description": "For webhooks: true makes a new secret; only do this when your person asks or the secret leaked" },
                "source": s("For webhooks: who sends it; changing it to a different sender starts from that sender's ready-made checks"),
                "auth": auth,
                "require": require,
                "skip": skip,
                "keep": keep,
                "title": title,
                "link": link,
                "json_only": json_only,
                "agent": agent.clone()
            }),
            &["feed"],
        ),
        tool("pause_feed", "Pause a feed (no polling, webhook posts are dropped) or resume it with paused false.", json!({ "feed": feed.clone(), "paused": { "type": "boolean", "description": "true to pause (default), false to resume" }, "agent": agent.clone() }), &["feed"]),
        tool(
            "test_feed",
            "Try a feed without delivering anything. For rss: fetch it now and show its latest items and which the filter would keep; pass a watched feed or a url you are thinking of adding. For a webhook: pass sample (a JSON body like the sender posts) and optionally sample_headers to see whether its checks would turn the call away, drop it, or pass it on, and exactly what would reach you.",
            json!({ "feed": feed, "url": s("A feed URL to try before adding it"), "limit": { "type": "integer", "description": "How many items to show, default 5, at most 20" }, "sample": s("For a webhook feed: an example body, as JSON text"), "sample_headers": { "type": "object", "description": "For a webhook feed: example headers, like {\"x-github-event\": \"pull_request\"}" }, "agent": agent }),
            &[],
        ),
    ]
}

fn item_lines(items: &[FeedItem], filter: Option<&str>, limit: usize) -> String {
    if items.is_empty() {
        return "(the feed has no items right now)".into();
    }
    items
        .iter()
        .take(limit)
        .map(|i| {
            let mut line = format!("- {}", i.title);
            if let Some(at) = i.published {
                line.push_str(&format!(" ({})", at.format("%Y-%m-%d %H:%M UTC")));
            }
            if filter.is_some() && !feeds::matches_filter(filter, i) {
                line.push_str(" [filtered out]");
            }
            if let Some(link) = &i.link {
                line.push_str(&format!("\n  {link}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn every(secs: i32) -> String {
    match secs {
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s => format!("{}m", s / 60),
    }
}

fn describe(f: &AgentFeed, owner_name: Option<&str>) -> String {
    let mut out = format!("- {} [{}] {}", f.name, &f.id.to_string()[..8], f.kind);
    if let Some(who) = owner_name {
        out.push_str(&format!(" for {who}"));
    }
    match f.kind.as_str() {
        "rss" => out.push_str(&format!(" {} every {}", f.url.as_deref().unwrap_or(""), every(f.poll_secs))),
        _ => {
            let intake = f.intake();
            out.push_str(&format!(" ({} format) {}", f.format, f.webhook_url().unwrap_or_default()));
            if !intake.source.is_empty() {
                out.push_str(&format!(", sent by {}", intake.source));
            }
            out.push_str(match f.auth_state(Utc::now()) {
                AuthState::Verified if f.by_address() => ", verified by sender address, no secret needed",
                AuthState::Waiting if f.by_address() => ", verified by sender address, no call yet",
                AuthState::Verified => ", sender uses the secret",
                AuthState::Waiting => ", no verified call yet",
                AuthState::Grace => ", NEEDS THE SENDER UPDATED: it posts without the secret, which stops working when the grace period ends",
                AuthState::Refusing => ", REFUSING its sender: calls come without the secret; the sender must be updated with it",
            });
            if f.secret_ciphertext.is_some() && f.secret_shown_at.is_none() {
                out.push_str(" (your person can see the secret once in the app)");
            }
            if !f.allowed_ips.is_empty() {
                out.push_str(&format!(", only from {}", f.allowed_ips.join(" ")));
            }
            out.push_str(&format!("\n  checks: {}", intake.describe()));
            if !f.last_fields.is_empty() {
                out.push_str(&format!("\n  field names in its last call: {}", f.last_fields.join(", ")));
            }
            if f.turned_away > 0 {
                out.push_str(&format!(
                    "\n  {} calls turned away by its checks{}",
                    f.turned_away,
                    f.turned_away_why.as_deref().map(|why| format!(", last because {why}")).unwrap_or_default()
                ));
            }
        }
    }
    if let Some(filter) = &f.filter {
        out.push_str(&format!(", filter: {filter}"));
    }
    if f.paused {
        out.push_str(", PAUSED");
    }
    out.push_str(&format!(", {} items delivered", f.item_count));
    if let Some(at) = f.last_item_at {
        out.push_str(&format!(", last {}", at.format("%Y-%m-%d %H:%M UTC")));
    }
    if let Some(e) = &f.last_error {
        out.push_str(&format!(", last error: {e}"));
    }
    out
}

const SECRET_PLACEHOLDER: &str = "<webhook secret from Loka>";

fn alertmanager_snippet(f: &AgentFeed, agent: &WorldAgent) -> String {
    let url = f.webhook_url().unwrap_or_default();
    let receiver = format!("world-{}", agent.slug);
    format!(
        "Webhook URL for {name}:\n{url}\n\nEvery call must carry the feed's secret, or it is refused. You do not have the secret and must never ask for it in chat: your person sees it once in Loka (the agent's Settings > Feeds, or the Observatory) and pastes it into the sender themself. Leave {SECRET_PLACEHOLDER} as it is in anything you write.\n\nAlertmanager (alertmanager.yml):\n```yaml\nreceivers:\n  - name: {receiver}\n    webhook_configs:\n      - url: {url}\n        send_resolved: true\n        max_alerts: 50\n        http_config:\n          authorization:\n            type: Bearer\n            credentials: {SECRET_PLACEHOLDER}\n\nroute:\n  routes:\n    - receiver: {receiver}\n      matchers:\n        - severity=~\"critical|warning\"\n      continue: true\n```\nAdd the receiver, and either the route above (under your existing route) or set it as the top-level receiver. Reload Alertmanager afterwards.\n\nGitHub webhooks: put the secret in the webhook's Secret field (it is checked as X-Hub-Signature-256). Any other tool can POST JSON or plain text with the secret as a bearer token, e.g.\ncurl -X POST -H 'Authorization: Bearer {SECRET_PLACEHOLDER}' -H 'Content-Type: application/json' -d '{{\"title\": \"deploy finished\"}}' {url}",
        name = agent.name
    )
}

fn lines_arg(args: &Value, key: &str) -> Option<Vec<String>> {
    match args.get(key)? {
        Value::Null => Some(Vec::new()),
        Value::Array(list) => Some(list.iter().filter_map(Value::as_str).map(String::from).collect()),
        Value::String(text) if text.trim().is_empty() || text.trim().eq_ignore_ascii_case("none") => Some(Vec::new()),
        Value::String(text) => Some(text.split('\n').map(String::from).collect()),
        _ => None,
    }
}

fn intake_change(args: &Value) -> IntakeChange {
    IntakeChange {
        source: args.get("source").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from),
        require: lines_arg(args, "require"),
        skip: lines_arg(args, "skip"),
        keep: lines_arg(args, "keep"),
        title: args.get("title").and_then(Value::as_str).map(String::from),
        link: lines_arg(args, "link"),
        json_only: args.get("json_only").and_then(Value::as_bool),
    }
}

fn by_address_arg(args: &Value) -> Result<Option<bool>> {
    match args.get("auth").and_then(Value::as_str).map(|a| a.trim().to_lowercase()).as_deref() {
        None | Some("") => Ok(None),
        Some("secret") => Ok(Some(false)),
        Some("address") => Ok(Some(true)),
        Some(other) => bail!("auth is secret or address, not {other}"),
    }
}

fn try_sample(f: &AgentFeed, args: &Value) -> Result<String> {
    let body = match args.get("sample") {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => bail!("give sample, an example body like the sender posts"),
    };
    let sample_headers: Vec<(String, String)> = args
        .get("sample_headers")
        .and_then(Value::as_object)
        .map(|map| map.iter().filter_map(|(k, v)| Some((k.to_lowercase(), v.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    let header = |name: &str| sample_headers.iter().find(|(k, _)| k == &name.to_lowercase()).map(|(_, v)| v.clone());
    Ok(match feed_intake::apply(&f.intake(), &f.format, &header, body.as_bytes()) {
        Outcome::Invalid(why) => format!("\"{}\" would turn this call away (HTTP 422): {why}. Nothing would reach you.", f.name),
        Outcome::Skipped(why) => format!("\"{}\" would accept this call and drop it quietly: {why}. Nothing would reach you.", f.name),
        Outcome::Items(items) => {
            let shown: Vec<String> = items
                .iter()
                .map(|i| {
                    let mut line = format!("- {}", i.title);
                    if !feeds::matches_filter(f.filter.as_deref(), i) {
                        line.push_str(" [dropped by the keyword filter]");
                    }
                    if let Some(link) = &i.link {
                        line.push_str(&format!("\n  link: {link}"));
                    }
                    if !i.summary.is_empty() {
                        line.push_str(&format!("\n  {}", i.summary.replace('\n', "\n  ")));
                    }
                    line
                })
                .collect();
            format!("\"{}\" would pass this call on. This is everything that would reach you:\n{}", f.name, shown.join("\n"))
        }
    })
}

fn ranges_arg(args: &Value) -> Option<Vec<String>> {
    match args.get("allowed_ips")? {
        Value::Null => Some(Vec::new()),
        Value::Array(list) => Some(list.iter().filter_map(Value::as_str).map(String::from).collect()),
        Value::String(text) if text.trim().is_empty() || text.trim().eq_ignore_ascii_case("none") => Some(Vec::new()),
        Value::String(text) => Some(vec![text.clone()]),
        _ => None,
    }
}

async fn find(store: &WorldStore, target: &WorldAgent, others: bool, key: &str) -> Result<AgentFeed> {
    let mut pool = store.agent_feeds(target.id).await?;
    if others {
        pool.extend(store.owner_feeds(target.owner_user_id).await?.into_iter().filter(|f| f.agent_id != target.id));
    }
    let k = key.trim().to_lowercase();
    pool.iter()
        .find(|f| f.id.to_string() == k || (k.len() >= 4 && f.id.to_string().starts_with(&k)))
        .or_else(|| pool.iter().find(|f| f.name.to_lowercase() == k))
        .or_else(|| pool.iter().find(|f| f.url.as_deref().is_some_and(|u| u.to_lowercase() == k)))
        .cloned()
        .ok_or_else(|| anyhow!("no feed called {key}; use list_feeds"))
}

pub async fn call(store: &WorldStore, caller: &WorldAgent, target: &WorldAgent, others: bool, name: &str, args: &Value) -> Result<String> {
    let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let whose = if target.id == caller.id { "You".to_string() } else { target.name.clone() };
    match name {
        "add_feed" => {
            let kind = match (arg("kind"), arg("url")) {
                (Some(k), _) => k.to_lowercase(),
                (None, Some(_)) => "rss".into(),
                (None, None) => bail!("give a url to watch an RSS or Atom feed"),
            };
            if kind == "webhook" {
                bail!("webhook feeds are part of Loka Pro: this app runs on your person's Mac, where outside services cannot reach it. Use an RSS feed, a watch or a schedule instead");
            }
            let filter = arg("filter");
            let poll_secs = feeds::clamp_poll_secs(args.get("every_minutes").and_then(Value::as_i64).unwrap_or(15) * 60);
            match kind.as_str() {
                "rss" => {
                    let url = arg("url").ok_or_else(|| anyhow!("url is required for an rss feed"))?;
                    let url = feeds::check_url(url)?.to_string();
                    if store.agent_feeds(target.id).await?.iter().any(|f| f.url.as_deref() == Some(url.as_str())) {
                        bail!("{whose} already watch {url}; use list_feeds");
                    }
                    let (title, items) = match feeds::fetch(&url, None, None).await? {
                        Fetched::Feed { title, items, .. } => (title, items),
                        Fetched::NotModified => (None, Vec::new()),
                    };
                    let name = arg("name").map(String::from).or(title).unwrap_or_else(|| {
                        reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| "feed".into())
                    });
                    let feed = store
                        .create_feed(NewFeed {
                            agent_id: target.id,
                            owner_user_id: target.owner_user_id,
                            created_by_agent: Some(caller.id),
                            kind: "rss",
                            name: &name,
                            url: Some(&url),
                            format: "auto",
                            filter,
                            poll_secs,
                            seen: items.iter().map(|i| i.id.clone()).collect(),
                            primed: true,
                            allowed_ips: Vec::new(),
                            by_address: false,
                            intake: feed_intake::Intake::default(),
                        })
                        .await?;
                    let watcher = if target.id == caller.id { "you".to_string() } else { target.name.clone() };
                    Ok(format!(
                        "Watching \"{}\" [{}] for {watcher}, checked every {}{}. The {} items in it now count as seen; only new ones arrive, grouped into one feed.item event per check.\nLatest items right now:\n{}",
                        feed.name,
                        &feed.id.to_string()[..8],
                        every(feed.poll_secs),
                        feed.filter.as_deref().map(|f| format!(" with filter \"{f}\"")).unwrap_or_default(),
                        items.len(),
                        item_lines(&items, feed.filter.as_deref(), 3)
                    ))
                }
                "webhook" => {
                    let change = intake_change(args);
                    let Some(source) = change.source.clone() else {
                        bail!("say what will send to this webhook with source (github, gitlab, alertmanager, grafana, or a short name for anything else). If your person has not told you, ask them what will send it and what they want to hear about, then make it");
                    };
                    let preset = feed_intake::for_source(&source);
                    let intake = change.onto(&preset.intake)?;
                    let format = arg("format").map(str::to_lowercase).filter(|f| f != "auto").unwrap_or_else(|| preset.format.to_string());
                    let by_address = by_address_arg(args)?.unwrap_or(false);
                    let name = arg("name").map(String::from).unwrap_or_else(|| intake.source.clone());
                    let name = name.as_str();
                    let unshaped = !preset.known && !intake.shapes_calls();
                    let feed = store
                        .create_feed(NewFeed {
                            agent_id: target.id,
                            owner_user_id: target.owner_user_id,
                            created_by_agent: Some(caller.id),
                            kind: "webhook",
                            name,
                            url: None,
                            format: &format,
                            filter,
                            poll_secs: feeds::DEFAULT_POLL_SECS,
                            seen: Vec::new(),
                            primed: true,
                            allowed_ips: ranges_arg(args).unwrap_or_default(),
                            by_address,
                            intake,
                        })
                        .await?;
                    let mut out = format!(
                        "Webhook feed \"{}\" [{}] for {} is ready, sent by {}.\nChecks: {}.\n\n",
                        feed.name,
                        &feed.id.to_string()[..8],
                        target.name,
                        feed.intake().source,
                        feed.intake().describe()
                    );
                    if by_address {
                        out.push_str(&format!(
                            "Webhook URL:\n{}\n\nCalls from {} are accepted without a secret; calls from anywhere else are refused. Tell your person to point the sender at the URL.",
                            feed.webhook_url().unwrap_or_default(),
                            feed.allowed_ips.join(", ")
                        ));
                    } else {
                        out.push_str(&alertmanager_snippet(&feed, target));
                        out.push_str(&format!("\n\nFor this sender: {}", preset.sender_setup));
                        if !feed.allowed_ips.is_empty() {
                            out.push_str(&format!("\n\nOnly calls from {} are accepted.", feed.allowed_ips.join(", ")));
                        }
                    }
                    if unshaped {
                        out.push_str("\n\nThis sender has no checks yet, so the whole body of each call reaches you, cut to 1500 characters. Narrow it: ask your person which fields matter, or after the first real call read its field names with list_feeds, then update_feed with require and keep. test_feed with a sample shows what a call would turn into.");
                    }
                    if feeds::public_base().is_none() {
                        out.push_str("\n\nThis Loka has no public address configured (WORLD_PUBLIC_API_URL), so replace the first part of the URL with the address of its API.");
                    }
                    Ok(out)
                }
                other => bail!("unknown kind {other}; use rss or webhook"),
            }
        }
        "list_feeds" => {
            let feeds = store.agent_feeds(target.id).await?;
            if feeds.is_empty() {
                return Ok(format!("{whose} watch no feeds. Use add_feed with an RSS URL, or kind webhook for alerts."));
            }
            Ok(feeds.iter().map(|f| describe(f, None)).collect::<Vec<_>>().join("\n"))
        }
        "remove_feed" => {
            let feed = find(store, target, others, arg("feed").ok_or_else(|| anyhow!("feed is required"))?).await?;
            store.delete_feed(feed.id).await?;
            Ok(format!("Stopped watching \"{}\".{}", feed.name, if feed.kind == "webhook" { " Its webhook URL no longer works; remove it from Alertmanager too." } else { "" }))
        }
        "pause_feed" => {
            let feed = find(store, target, others, arg("feed").ok_or_else(|| anyhow!("feed is required"))?).await?;
            let paused = args.get("paused").and_then(Value::as_bool).unwrap_or(true);
            store.set_feed_paused(feed.id, paused).await?;
            Ok(format!("\"{}\" is {}.", feed.name, if paused { "paused" } else { "running again" }))
        }
        "update_feed" => {
            let feed = find(store, target, others, arg("feed").ok_or_else(|| anyhow!("feed is required"))?).await?;
            let filter = match args.get("filter") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(v) => {
                    let text = v.as_str().map(str::trim).unwrap_or("");
                    Some(if text.is_empty() || text.eq_ignore_ascii_case("none") { None } else { Some(text) })
                }
            };
            let poll_secs = args.get("every_minutes").and_then(Value::as_i64).map(|m| feeds::clamp_poll_secs(m * 60));
            if poll_secs.is_some() && feed.kind != "rss" {
                bail!("\"{}\" is a webhook feed, so there is no check interval to change", feed.name);
            }
            let format = arg("format").map(str::to_lowercase);
            if format.is_some() && feed.kind != "webhook" {
                bail!("format only applies to webhook feeds");
            }
            let ranges = ranges_arg(args);
            let rotate = args.get("rotate_secret").and_then(Value::as_bool) == Some(true);
            let change = intake_change(args);
            let by_address = by_address_arg(args)?;
            if (ranges.is_some() || rotate || by_address.is_some() || !change.is_empty()) && feed.kind != "webhook" {
                bail!("allowed_ips, rotate_secret, auth, source, require, skip, keep, title, link and json_only only apply to webhook feeds");
            }
            if arg("name").is_none() && filter.is_none() && poll_secs.is_none() && format.is_none() && ranges.is_none() && !rotate && by_address.is_none() && change.is_empty() {
                bail!("give name, filter, every_minutes, format, allowed_ips, rotate_secret, auth, source, require, skip, keep, title, link or json_only");
            }
            let intake = match change.is_empty() {
                true => None,
                false => {
                    let renamed = change.source.as_deref().map(feed_intake::clean_source).filter(|s| !s.eq_ignore_ascii_case(&feed.intake().source));
                    let preset_format = renamed.as_deref().map(|source| feed_intake::for_source(source).format).filter(|f| *f != "auto" && format.is_none());
                    Some((change.onto(&feed.intake())?, preset_format))
                }
            };
            if let Some(ranges) = &ranges {
                feeds::check_ranges(ranges)?;
            }
            let mut updated = store
                .update_feed(feed.id, arg("name"), filter, poll_secs, format.as_deref())
                .await?
                .ok_or_else(|| anyhow!("that feed is gone"))?;
            if by_address == Some(false) {
                updated = store.set_feed_by_address(feed.id, false).await?.ok_or_else(|| anyhow!("that feed is gone"))?;
            }
            if let Some(ranges) = ranges {
                updated = store.set_feed_ip_ranges(feed.id, &ranges).await?.ok_or_else(|| anyhow!("that feed is gone"))?;
            }
            if by_address == Some(true) {
                updated = store.set_feed_by_address(feed.id, true).await?.ok_or_else(|| anyhow!("that feed is gone"))?;
            }
            if let Some((intake, preset_format)) = intake {
                updated = store.set_feed_intake(feed.id, &intake, preset_format).await?.ok_or_else(|| anyhow!("that feed is gone"))?;
            }
            let mut note = String::new();
            if rotate {
                store.rotate_feed_secret(feed.id, false).await?;
                updated = store.get_feed(feed.id).await?.ok_or_else(|| anyhow!("that feed is gone"))?;
                note = "\nThe secret was rotated: the old one stopped working now. Your person sees the new one once in the app and must update the sender with it; you never see it.".into();
            }
            Ok(format!("Updated:\n{}{note}", describe(&updated, None)))
        }
        "test_feed" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5).clamp(1, 20) as usize;
            let (url, filter, label) = match (arg("feed"), arg("url")) {
                (Some(key), _) => {
                    let feed = find(store, target, others, key).await?;
                    if feed.kind == "webhook" && args.get("sample").is_some() {
                        return try_sample(&feed, args);
                    }
                    if feed.kind == "webhook" {
                        return Ok(format!(
                            "\"{}\" is a webhook feed ({} format), so there is nothing to fetch. Pass sample to see what a call would turn into. Your person can post a real test with the secret they see in the app:\ncurl -X POST -H 'Authorization: Bearer {SECRET_PLACEHOLDER}' -H 'Content-Type: application/json' -d '{{\"title\": \"test from curl\"}}' {}\nIt has delivered {} items{}.",
                            feed.name,
                            feed.format,
                            feed.webhook_url().unwrap_or_default(),
                            feed.item_count,
                            feed.last_item_at.map(|a| format!(", last at {}", a.format("%Y-%m-%d %H:%M UTC"))).unwrap_or_default()
                        ));
                    }
                    (feed.url.clone().unwrap_or_default(), feed.filter.clone(), format!("\"{}\"", feed.name))
                }
                (None, Some(url)) => (url.to_string(), arg("filter").map(String::from), url.to_string()),
                (None, None) => bail!("give a feed or a url"),
            };
            match feeds::fetch(&url, None, None).await? {
                Fetched::Feed { title, items, .. } => Ok(format!(
                    "{label}{} has {} items. Latest {}:\n{}",
                    title.map(|t| format!(" ({t})")).unwrap_or_default(),
                    items.len(),
                    limit.min(items.len()),
                    item_lines(&items, filter.as_deref(), limit)
                )),
                Fetched::NotModified => Ok(format!("{label} has not changed.")),
            }
        }
        other => bail!("unknown feed tool {other}"),
    }
}

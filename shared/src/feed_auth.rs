use std::net::IpAddr;

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use serde_json::json;
use uuid::Uuid;

use crate::environment;
use crate::feeds::{AgentFeed, FEED_COLUMNS};
use crate::world::{NewEvent, WorldStore};

pub const SECRET_PREFIX: &str = "lkwh_";
pub const GRACE_DAYS: i64 = 7;
pub const MAX_IP_RANGES: usize = 32;
pub const FAILURES_BEFORE_NOTE: i32 = 5;
pub const REFUSED_EVENT: &str = "feed.refused";
pub const UNVERIFIED_EVENT: &str = "feed.unverified";
const SECRET_NAME: &str = "feed-webhook-secret";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Bearer,
    Basic,
    Header,
    Signature,
    Address,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Bearer => "bearer",
            Method::Basic => "basic",
            Method::Header => "header",
            Method::Signature => "signature",
            Method::Address => "address",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accepted(Method),
    Grace,
    WrongAddress,
    UnsureAddress,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sender {
    pub ip: Option<IpAddr>,
    pub certain: bool,
}

impl Sender {
    pub fn at(ip: Option<IpAddr>) -> Sender {
        Sender { ip, certain: true }
    }
}

pub const TOKEN_HEADERS: &[&str] = &["x-loka-token", "x-gitlab-token", "x-webhook-token"];
pub const ADDRESS_ONLY_MIN_BITS_V4: u8 = 8;
pub const ADDRESS_ONLY_MIN_BITS_V6: u8 = 16;
pub const UNSURE_ADDRESS: &str = "This Loka sits behind a proxy and has not been told which header carries the real sender address, so a feed checked by address alone refuses every call. Set WORLD_CLIENT_IP_HEADER on the API (cf-connecting-ip, x-real-ip, x-forwarded-for or peer), or switch the feed back to its secret.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    Verified,
    Waiting,
    Grace,
    Refusing,
}

impl AgentFeed {
    pub fn in_grace(&self, now: DateTime<Utc>) -> bool {
        self.authed_at.is_none() && self.auth_grace_until.is_some_and(|until| until > now)
    }

    pub fn auth_state(&self, now: DateTime<Utc>) -> AuthState {
        if self.by_address() {
            return if self.authed_at.is_some() { AuthState::Verified } else { AuthState::Waiting };
        }
        if self.authed_at.is_some() {
            AuthState::Verified
        } else if self.in_grace(now) {
            AuthState::Grace
        } else if self.auth_grace_until.is_some() || self.unauth_at.is_some() {
            AuthState::Refusing
        } else {
            AuthState::Waiting
        }
    }

    fn secret(&self) -> Option<String> {
        let (ciphertext, nonce) = (self.secret_ciphertext.as_deref()?, self.secret_nonce.as_deref()?);
        let plain = environment::open(self.id, SECRET_NAME, ciphertext, nonce).ok()?;
        String::from_utf8(plain).ok()
    }

    pub fn check_request(&self, sender: Sender, header: impl Fn(&str) -> Option<String>, body: &[u8], now: DateTime<Utc>) -> Verdict {
        let proof = self.secret().and_then(|secret| proof(&secret, header, body));
        decide(self, sender, proof, now)
    }
}

pub fn decide(feed: &AgentFeed, sender: Sender, proof: Option<Method>, now: DateTime<Utc>) -> Verdict {
    if !ip_allowed(&feed.allowed_ips, sender.ip) {
        return Verdict::WrongAddress;
    }
    match proof {
        Some(method) => Verdict::Accepted(method),
        None if feed.by_address() && !feed.allowed_ips.is_empty() => {
            if sender.certain {
                Verdict::Accepted(Method::Address)
            } else {
                Verdict::UnsureAddress
            }
        }
        None if feed.in_grace(now) => Verdict::Grace,
        None => Verdict::Refused,
    }
}

pub fn address_only_ranges(ranges: &[String]) -> Result<()> {
    if ranges.is_empty() {
        bail!("checking by sender address needs at least one address range, or anyone could post; add the sender's addresses or keep the secret");
    }
    for raw in ranges {
        let Some((net, bits)) = parse_range(raw) else {
            bail!("{raw:?} is not an IP address or range");
        };
        let least = if net.is_ipv4() { ADDRESS_ONLY_MIN_BITS_V4 } else { ADDRESS_ONLY_MIN_BITS_V6 };
        if bits < least {
            bail!("{raw} is too wide to stand in for a secret; use ranges of /{least} or narrower");
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn new_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new().fill(&mut bytes).map_err(|_| anyhow!("could not make a webhook secret"))?;
    Ok(format!("{SECRET_PREFIX}{}", hex(&bytes)))
}

fn seal(feed_id: Uuid, secret: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let key = environment::master_key().map_err(|_| anyhow!("webhook feeds need WORLD_SECRETS_KEY set on this Loka, so their secret can be stored encrypted"))?;
    environment::encrypt(key, feed_id, SECRET_NAME, secret.as_bytes())
}

pub(crate) fn seal_new_secret(feed_id: Uuid) -> Result<(Vec<u8>, Vec<u8>)> {
    seal(feed_id, &new_secret()?)
}

pub(crate) fn rotation_name() -> &'static str {
    SECRET_NAME
}

pub fn signature(secret: &str, body: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    format!("sha256={}", hex(hmac::sign(&key, body).as_ref()))
}

pub fn proof(secret: &str, header: impl Fn(&str) -> Option<String>, body: &[u8]) -> Option<Method> {
    if secret.len() < 16 {
        return None;
    }
    if let Some(auth) = header("authorization") {
        let auth = auth.trim();
        if let Some((scheme, rest)) = auth.split_once(' ') {
            let rest = rest.trim();
            if scheme.eq_ignore_ascii_case("bearer") && same(rest.as_bytes(), secret.as_bytes()) {
                return Some(Method::Bearer);
            }
            if scheme.eq_ignore_ascii_case("basic") {
                let password = STANDARD
                    .decode(rest)
                    .ok()
                    .and_then(|raw| String::from_utf8(raw).ok())
                    .and_then(|pair| pair.split_once(':').map(|(_, p)| p.to_string()));
                if password.is_some_and(|p| same(p.as_bytes(), secret.as_bytes())) {
                    return Some(Method::Basic);
                }
            }
        }
    }
    for name in TOKEN_HEADERS {
        if header(name).is_some_and(|value| same(value.trim().as_bytes(), secret.as_bytes())) {
            return Some(Method::Header);
        }
    }
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    for name in ["x-loka-signature", "x-hub-signature-256"] {
        let Some(value) = header(name) else { continue };
        let Some(digest) = value.trim().strip_prefix("sha256=").and_then(|h| unhex(h.trim())) else { continue };
        if hmac::verify(&key, body, &digest).is_ok() {
            return Some(Method::Signature);
        }
    }
    None
}

fn parse_range(raw: &str) -> Option<(IpAddr, u8)> {
    let raw = raw.trim();
    let (addr, bits) = match raw.split_once('/') {
        Some((addr, bits)) => (addr.trim(), Some(bits.trim())),
        None => (raw, None),
    };
    let ip = addr.parse::<IpAddr>().ok()?.to_canonical();
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let bits = match bits {
        Some(b) => b.parse::<u8>().ok()?,
        None => max,
    };
    (bits <= max).then(|| (masked(ip, bits), bits))
}

fn masked(ip: IpAddr, bits: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits as u32) };
            IpAddr::V4((u32::from(v4) & mask).into())
        }
        IpAddr::V6(v6) => {
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits as u32) };
            IpAddr::V6((u128::from(v6) & mask).into())
        }
    }
}

fn in_range(ip: IpAddr, (net, bits): (IpAddr, u8)) -> bool {
    let ip = ip.to_canonical();
    ip.is_ipv4() == net.is_ipv4() && masked(ip, bits) == net
}

pub fn clean_ranges(ranges: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in ranges.iter().flat_map(|r| r.split([',', ' ', '\n'])).map(str::trim).filter(|r| !r.is_empty()) {
        let Some((net, bits)) = parse_range(raw) else {
            bail!("{raw:?} is not an IP address or range like 203.0.113.0/24 or 2001:db8::/32");
        };
        if bits == 0 {
            bail!("{raw} would allow every address; leave the list empty instead");
        }
        let text = format!("{net}/{bits}");
        if !out.contains(&text) {
            out.push(text);
        }
    }
    if out.len() > MAX_IP_RANGES {
        bail!("at most {MAX_IP_RANGES} address ranges per feed");
    }
    Ok(out)
}

pub fn ip_allowed(ranges: &[String], ip: Option<IpAddr>) -> bool {
    if ranges.is_empty() {
        return true;
    }
    let Some(ip) = ip else { return false };
    ranges.iter().filter_map(|r| parse_range(r)).any(|range| in_range(ip, range))
}

impl WorldStore {
    pub async fn ensure_feed_secrets(&self) -> Result<usize> {
        if !environment::configured() {
            return Ok(0);
        }
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM agent_feeds WHERE kind = 'webhook' AND secret_ciphertext IS NULL")
            .fetch_all(self.pool())
            .await?;
        let mut made = 0;
        for id in ids {
            let (ciphertext, nonce) = seal_new_secret(id)?;
            made += sqlx::query(
                "UPDATE agent_feeds SET secret_ciphertext = $2, secret_nonce = $3, secret_set_at = NOW(), secret_shown_at = NULL
                 WHERE id = $1 AND secret_ciphertext IS NULL",
            )
            .bind(id)
            .bind(ciphertext)
            .bind(nonce)
            .execute(self.pool())
            .await?
            .rows_affected() as usize;
        }
        Ok(made)
    }

    pub async fn reveal_feed_secret(&self, id: Uuid) -> Result<Option<String>> {
        let feed = sqlx::query_as::<_, AgentFeed>(&format!(
            "UPDATE agent_feeds SET secret_shown_at = NOW()
             WHERE id = $1 AND kind = 'webhook' AND secret_shown_at IS NULL AND secret_ciphertext IS NOT NULL
             RETURNING {FEED_COLUMNS}"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        match feed {
            None => Ok(None),
            Some(feed) => feed.secret().map(Some).ok_or_else(|| anyhow!("the stored webhook secret could not be read; rotate it")),
        }
    }

    pub async fn rotate_feed_secret(&self, id: Uuid, reveal: bool) -> Result<Option<String>> {
        let secret = new_secret()?;
        let (ciphertext, nonce) = seal(id, &secret)?;
        let done = sqlx::query(
            "UPDATE agent_feeds SET secret_ciphertext = $2, secret_nonce = $3, secret_set_at = NOW(),
               secret_shown_at = CASE WHEN $4 THEN NOW() ELSE NULL END,
               auth_grace_until = NULL, authed_at = NULL, unauth_at = NULL, grace_noted_at = NULL,
               auth_failures = 0, auth_failed_at = NULL, auth_noted_at = NULL, updated_at = NOW()
             WHERE id = $1 AND kind = 'webhook'",
        )
        .bind(id)
        .bind(ciphertext)
        .bind(nonce)
        .bind(reveal)
        .execute(self.pool())
        .await?
        .rows_affected()
            > 0;
        if !done {
            bail!("only webhook feeds have a secret");
        }
        Ok(reveal.then_some(secret))
    }

    pub async fn set_feed_by_address(&self, id: Uuid, by_address: bool) -> Result<Option<AgentFeed>> {
        let Some(feed) = self.get_feed(id).await? else {
            return Ok(None);
        };
        if feed.kind != "webhook" {
            bail!("only webhook feeds check their sender");
        }
        if by_address {
            address_only_ranges(&feed.allowed_ips)?;
        }
        Ok(sqlx::query_as::<_, AgentFeed>(&format!(
            "UPDATE agent_feeds SET auth_mode = $2, auth_failures = 0, auth_failed_at = NULL, auth_noted_at = NULL,
               auth_grace_until = CASE WHEN $2 = 'address' THEN NULL ELSE auth_grace_until END,
               authed_at = CASE WHEN auth_mode = $2 THEN authed_at ELSE NULL END,
               last_error = CASE WHEN auth_mode = $2 THEN last_error ELSE NULL END,
               updated_at = NOW()
             WHERE id = $1 AND kind = 'webhook' AND ($2 = 'secret' OR cardinality(allowed_ips) > 0)
             RETURNING {FEED_COLUMNS}"
        ))
        .bind(id)
        .bind(if by_address { "address" } else { "secret" })
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn set_feed_ip_ranges(&self, id: Uuid, ranges: &[String]) -> Result<Option<AgentFeed>> {
        let ranges = clean_ranges(ranges)?;
        if self.get_feed(id).await?.is_some_and(|feed| feed.by_address()) {
            address_only_ranges(&ranges).map_err(|e| anyhow!("this feed is checked by sender address alone: {e}"))?;
        }
        Ok(sqlx::query_as::<_, AgentFeed>(&format!(
            "UPDATE agent_feeds SET allowed_ips = $2, updated_at = NOW()
             WHERE id = $1 AND kind = 'webhook' AND (auth_mode = 'secret' OR cardinality($2::text[]) > 0) RETURNING {FEED_COLUMNS}"
        ))
        .bind(id)
        .bind(&ranges)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn note_feed_verified(&self, id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE agent_feeds SET authed_at = NOW(), auth_grace_until = NULL, auth_failures = 0,
               last_error = CASE WHEN auth_noted_at IS NOT NULL OR grace_noted_at IS NOT NULL THEN NULL ELSE last_error END,
               auth_noted_at = NULL
             WHERE id = $1 AND (authed_at IS NULL OR authed_at < NOW() - INTERVAL '1 minute' OR auth_grace_until IS NOT NULL OR auth_failures > 0)",
        )
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn note_feed_unverified(&self, feed: &AgentFeed) -> Result<()> {
        let first: Option<bool> = sqlx::query_scalar(
            "UPDATE agent_feeds SET unauth_at = NOW(),
               grace_noted_at = COALESCE(grace_noted_at, NOW()),
               last_error = CASE WHEN grace_noted_at IS NULL THEN $2 ELSE last_error END
             WHERE id = $1
             RETURNING (grace_noted_at = NOW())",
        )
        .bind(feed.id)
        .bind(format!(
            "The sender posts without the webhook secret. Calls like this stop working on {}; update the sender with the secret.",
            feed.auth_grace_until.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(|| "the end of the grace period".into())
        ))
        .fetch_optional(self.pool())
        .await?;
        if first == Some(true) {
            self.note_owner(feed, UNVERIFIED_EVENT, "unverified").await?;
        }
        Ok(())
    }

    pub async fn note_feed_refusal(&self, feed: &AgentFeed, verdict: Verdict) -> Result<()> {
        let wrong_address = verdict == Verdict::WrongAddress;
        let message = match verdict {
            Verdict::WrongAddress => "Calls from addresses outside this feed's allowed ranges were refused.",
            Verdict::UnsureAddress => UNSURE_ADDRESS,
            _ => "Calls without the webhook secret were refused. If they come from your sender, add the secret to it.",
        };
        let note: Option<bool> = sqlx::query_scalar(
            "UPDATE agent_feeds SET
               auth_failures = CASE WHEN auth_failed_at > NOW() - INTERVAL '1 hour' THEN auth_failures + 1 ELSE 1 END,
               auth_failed_at = NOW()
             WHERE id = $1
             RETURNING auth_failures >= $2 AND (auth_noted_at IS NULL OR auth_noted_at < NOW() - INTERVAL '1 day')",
        )
        .bind(feed.id)
        .bind(FAILURES_BEFORE_NOTE)
        .fetch_optional(self.pool())
        .await?;
        if note != Some(true) {
            return Ok(());
        }
        let claimed = sqlx::query(
            "UPDATE agent_feeds SET auth_noted_at = NOW(), last_error = $2
             WHERE id = $1 AND (auth_noted_at IS NULL OR auth_noted_at < NOW() - INTERVAL '1 day')",
        )
        .bind(feed.id)
        .bind(message)
        .execute(self.pool())
        .await?
        .rows_affected()
            > 0;
        if claimed {
            let reason = match verdict {
                Verdict::UnsureAddress => "unsure_address",
                _ if wrong_address => "address",
                _ => "secret",
            };
            self.note_owner(feed, REFUSED_EVENT, reason).await?;
        }
        Ok(())
    }

    async fn note_owner(&self, feed: &AgentFeed, event: &str, reason: &str) -> Result<()> {
        self.emit(
            NewEvent::new(event).agent(feed.agent_id).payload(json!({
                "feed_id": feed.id,
                "feed": feed.name,
                "reason": reason,
                "owner_user_id": feed.owner_user_id,
                "quiet": true,
            })),
            None,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn feed(secret: Option<&str>) -> AgentFeed {
        environment::initialize_for_tests();
        let id = Uuid::new_v4();
        let sealed = secret.map(|s| seal(id, s).unwrap());
        AgentFeed {
            id,
            agent_id: Uuid::new_v4(),
            owner_user_id: None,
            created_by_agent: None,
            kind: "webhook".into(),
            name: "hook".into(),
            url: None,
            token: Some("t".into()),
            format: "auto".into(),
            filter: None,
            poll_secs: 900,
            next_poll_at: None,
            last_polled_at: None,
            last_item_at: None,
            last_error: None,
            etag: None,
            last_modified: None,
            seen: Vec::new(),
            primed: true,
            item_count: 0,
            paused: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            secret_ciphertext: sealed.as_ref().map(|s| s.0.clone()),
            secret_nonce: sealed.map(|s| s.1),
            secret_set_at: None,
            secret_shown_at: None,
            allowed_ips: Vec::new(),
            auth_grace_until: None,
            authed_at: None,
            unauth_at: None,
            auth_mode: "secret".into(),
            intake: json!({}),
            last_fields: Vec::new(),
            turned_away: 0,
            turned_away_at: None,
            turned_away_why: None,
        }
    }

    fn headers<'a>(pairs: &'a [(&'a str, String)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    }

    const SECRET: &str = "lkwh_0123456789abcdef0123456789abcdef";
    const BODY: &[u8] = br#"{"title":"deploy done"}"#;

    #[test]
    fn accepts_each_proof() {
        let f = feed(Some(SECRET));
        let now = Utc::now();
        let check = |pairs: Vec<(&str, String)>| f.check_request(Sender::at(None), headers(&pairs), BODY, now);
        assert_eq!(check(vec![("Authorization", format!("Bearer {SECRET}"))]), Verdict::Accepted(Method::Bearer));
        assert_eq!(check(vec![("authorization", format!("bearer  {SECRET} "))]), Verdict::Accepted(Method::Bearer));
        let basic = STANDARD.encode(format!("alertmanager:{SECRET}"));
        assert_eq!(check(vec![("Authorization", format!("Basic {basic}"))]), Verdict::Accepted(Method::Basic));
        assert_eq!(check(vec![("X-Loka-Signature", signature(SECRET, BODY))]), Verdict::Accepted(Method::Signature));
        assert_eq!(check(vec![("X-Hub-Signature-256", signature(SECRET, BODY))]), Verdict::Accepted(Method::Signature));
    }

    #[test]
    fn refuses_wrong_or_missing_proofs() {
        let f = feed(Some(SECRET));
        let now = Utc::now();
        let check = |pairs: Vec<(&str, String)>| f.check_request(Sender::at(None), headers(&pairs), BODY, now);
        assert_eq!(check(vec![]), Verdict::Refused);
        assert_eq!(check(vec![("Authorization", "Bearer lkwh_wrong0000000000000000000000000".into())]), Verdict::Refused);
        assert_eq!(check(vec![("Authorization", format!("Bearer {}", &SECRET[..SECRET.len() - 1]))]), Verdict::Refused);
        assert_eq!(check(vec![("Authorization", format!("Token {SECRET}"))]), Verdict::Refused);
        let user_only = STANDARD.encode(format!("{SECRET}:"));
        assert_eq!(check(vec![("Authorization", format!("Basic {user_only}"))]), Verdict::Refused);
        assert_eq!(check(vec![("X-Loka-Signature", signature(SECRET, b"other body"))]), Verdict::Refused);
        assert_eq!(check(vec![("X-Loka-Signature", signature("lkwh_another_secret_value_here", BODY))]), Verdict::Refused);
        assert_eq!(check(vec![("X-Hub-Signature", signature(SECRET, BODY))]), Verdict::Refused);
        assert_eq!(check(vec![("X-Loka-Signature", "sha256=zz".into())]), Verdict::Refused);
        let none = feed(None);
        assert_eq!(none.check_request(Sender::at(None), headers(&[]), BODY, now), Verdict::Refused);
    }

    #[test]
    fn address_ranges_gate_even_valid_secrets() {
        let mut f = feed(Some(SECRET));
        f.allowed_ips = clean_ranges(&["203.0.113.0/24, 2001:db8::/32".into()]).unwrap();
        let now = Utc::now();
        let good = vec![("Authorization", format!("Bearer {SECRET}"))];
        let at = |ip: &str| f.check_request(Sender::at(ip.parse().ok()), headers(&good), BODY, now);
        assert_eq!(at("203.0.113.9"), Verdict::Accepted(Method::Bearer));
        assert_eq!(at("::ffff:203.0.113.9"), Verdict::Accepted(Method::Bearer));
        assert_eq!(at("2001:db8:1::5"), Verdict::Accepted(Method::Bearer));
        assert_eq!(at("198.51.100.1"), Verdict::WrongAddress);
        assert_eq!(at("2001:db9::1"), Verdict::WrongAddress);
        assert_eq!(f.check_request(Sender::at(None), headers(&good), BODY, now), Verdict::WrongAddress);
        assert_eq!(f.check_request(Sender::at("198.51.100.1".parse().ok()), headers(&[]), BODY, now), Verdict::WrongAddress);
    }

    #[test]
    fn address_alone_can_stand_in_for_the_secret() {
        let mut f = feed(Some(SECRET));
        f.auth_mode = "address".into();
        f.allowed_ips = clean_ranges(&["203.0.113.0/24".into()]).unwrap();
        let now = Utc::now();
        let from = |ip: &str, certain: bool| f.check_request(Sender { ip: ip.parse().ok(), certain }, headers(&[]), BODY, now);
        assert_eq!(from("203.0.113.9", true), Verdict::Accepted(Method::Address));
        assert_eq!(from("198.51.100.1", true), Verdict::WrongAddress);
        assert_eq!(from("203.0.113.9", false), Verdict::UnsureAddress, "a guessed address never replaces the secret");
        let bearer = vec![("Authorization", format!("Bearer {SECRET}"))];
        assert_eq!(f.check_request(Sender { ip: "203.0.113.9".parse().ok(), certain: false }, headers(&bearer), BODY, now), Verdict::Accepted(Method::Bearer));
        assert_eq!(f.auth_state(now), AuthState::Waiting);
        let mut open = f.clone();
        open.allowed_ips.clear();
        assert_eq!(open.check_request(Sender::at("203.0.113.9".parse().ok()), headers(&[]), BODY, now), Verdict::Refused, "address mode without ranges falls back to the secret");
        assert!(address_only_ranges(&[]).is_err());
        assert!(address_only_ranges(&["10.0.0.0/8".into(), "2001:db8::/32".into()]).is_ok());
        assert!(address_only_ranges(&["8.0.0.0/7".into()]).is_err());
        assert!(address_only_ranges(&["2000::/3".into()]).is_err());
        let plain = feed(Some(SECRET));
        assert_eq!(plain.check_request(Sender::at("203.0.113.9".parse().ok()), headers(&[]), BODY, now), Verdict::Refused);
        let token = vec![("X-Gitlab-Token", SECRET.to_string())];
        assert_eq!(plain.check_request(Sender::at(None), headers(&token), BODY, now), Verdict::Accepted(Method::Header));
        let wrong = vec![("X-Loka-Token", "lkwh_wrong0000000000000000000000000".to_string())];
        assert_eq!(plain.check_request(Sender::at(None), headers(&wrong), BODY, now), Verdict::Refused);
    }

    #[test]
    fn cleans_ranges() {
        assert_eq!(clean_ranges(&["10.1.2.3/8".into(), "1.2.3.4".into(), "1.2.3.4/32".into()]).unwrap(), vec!["10.0.0.0/8", "1.2.3.4/32"]);
        assert_eq!(clean_ranges(&["2001:db8::1".into()]).unwrap(), vec!["2001:db8::1/128"]);
        assert!(clean_ranges(&["0.0.0.0/0".into()]).is_err());
        assert!(clean_ranges(&["10.0.0.0/33".into()]).is_err());
        assert!(clean_ranges(&["example.com".into()]).is_err());
        assert!(clean_ranges(&[]).unwrap().is_empty());
        let many: Vec<String> = (0..33).map(|i| format!("10.0.{i}.0/24")).collect();
        assert!(clean_ranges(&many).is_err());
    }

    #[test]
    fn grace_window_lets_old_senders_in_until_it_ends() {
        let mut f = feed(Some(SECRET));
        let now = Utc::now();
        f.auth_grace_until = Some(now + Duration::days(GRACE_DAYS));
        assert_eq!(f.check_request(Sender::at(None), headers(&[]), BODY, now), Verdict::Grace);
        assert_eq!(f.auth_state(now), AuthState::Grace);
        f.allowed_ips = vec!["203.0.113.0/24".into()];
        assert_eq!(f.check_request(Sender::at("198.51.100.1".parse().ok()), headers(&[]), BODY, now), Verdict::WrongAddress);
        f.allowed_ips.clear();
        assert_eq!(f.check_request(Sender::at(None), headers(&[]), BODY, now + Duration::days(GRACE_DAYS) + Duration::seconds(1)), Verdict::Refused);
        assert_eq!(f.auth_state(now + Duration::days(8)), AuthState::Refusing);
        f.authed_at = Some(now);
        assert_eq!(f.check_request(Sender::at(None), headers(&[]), BODY, now), Verdict::Refused, "once the sender uses the secret, the grace is over");
        assert_eq!(f.auth_state(now), AuthState::Verified);
        let fresh = feed(Some(SECRET));
        assert_eq!(fresh.check_request(Sender::at(None), headers(&[]), BODY, now), Verdict::Refused, "new feeds get no grace");
        assert_eq!(fresh.auth_state(now), AuthState::Waiting);
    }

    #[test]
    fn secrets_are_bound_to_their_feed() {
        let a = feed(Some(SECRET));
        let mut b = feed(None);
        b.secret_ciphertext = a.secret_ciphertext.clone();
        b.secret_nonce = a.secret_nonce.clone();
        assert_eq!(a.secret().as_deref(), Some(SECRET));
        assert!(b.secret().is_none());
        assert!(new_secret().unwrap().starts_with(SECRET_PREFIX));
        assert_ne!(new_secret().unwrap(), new_secret().unwrap());
    }

    async fn setup(pool: &sqlx::PgPool, tag: &str) -> Result<(Uuid, Uuid)> {
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("hook-{tag}@test.dev"))
            .fetch_one(pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Hook', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("hook-{tag}"))
        .bind(user)
        .fetch_one(pool)
        .await?;
        Ok((user, agent))
    }

    #[tokio::test]
    async fn webhook_secrets_reveal_once_rotate_and_grace() -> Result<()> {
        let Ok(url) = std::env::var("FEEDS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        environment::initialize_for_tests();
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let (user, agent) = setup(&pool, &tag).await?;
        let hook = store
            .create_feed(crate::feeds::NewFeed {
                agent_id: agent,
                owner_user_id: Some(user),
                created_by_agent: Some(agent),
                kind: "webhook",
                name: "Alerts",
                url: None,
                format: "auto",
                filter: None,
                poll_secs: 900,
                seen: Vec::new(),
                primed: true,
                allowed_ips: vec!["203.0.113.7".into()],
                by_address: false,
                intake: crate::feed_intake::Intake::default(),
            })
            .await?;
        assert_eq!(hook.allowed_ips, vec!["203.0.113.7/32"]);
        assert!(hook.secret_ciphertext.is_some() && hook.auth_grace_until.is_none());
        let view = hook.view();
        assert!(view.get("secret_ciphertext").is_none() && view["secret_unseen"] == json!(true));
        assert!(!view.to_string().contains(SECRET_PREFIX));

        let secret = store.reveal_feed_secret(hook.id).await?.expect("first reveal shows it");
        assert!(store.reveal_feed_secret(hook.id).await?.is_none(), "the secret shows only once");
        let stored = store.get_feed(hook.id).await?.unwrap();
        let ip = "203.0.113.7".parse().ok();
        let bearer = vec![("Authorization", format!("Bearer {secret}"))];
        assert_eq!(stored.check_request(Sender::at(ip), headers(&bearer), BODY, Utc::now()), Verdict::Accepted(Method::Bearer));
        assert_eq!(stored.check_request(Sender::at("203.0.113.8".parse().ok()), headers(&bearer), BODY, Utc::now()), Verdict::WrongAddress);

        assert!(store.rotate_feed_secret(hook.id, false).await?.is_none());
        let rotated = store.get_feed(hook.id).await?.unwrap();
        assert_eq!(rotated.check_request(Sender::at(ip), headers(&bearer), BODY, Utc::now()), Verdict::Refused, "the old secret stops working");
        let fresh = store.reveal_feed_secret(hook.id).await?.unwrap();
        assert_ne!(fresh, secret);
        let shown = store.rotate_feed_secret(hook.id, true).await?.unwrap();
        assert!(store.reveal_feed_secret(hook.id).await?.is_none());
        assert!(shown.starts_with(SECRET_PREFIX));

        sqlx::query(
            "UPDATE agent_feeds SET secret_ciphertext = NULL, secret_nonce = NULL, secret_shown_at = NULL, allowed_ips = '{}',
               auth_grace_until = NOW() + INTERVAL '7 days' WHERE id = $1",
        )
        .bind(hook.id)
        .execute(&pool)
        .await?;
        assert!(store.ensure_feed_secrets().await? >= 1);
        let legacy = store.get_feed(hook.id).await?.unwrap();
        assert!(legacy.secret_ciphertext.is_some() && legacy.secret_shown_at.is_none());
        assert_eq!(legacy.check_request(Sender::at(None), headers(&[]), BODY, Utc::now()), Verdict::Grace);
        assert_eq!(store.ingest_webhook(&legacy, &|_| None, BODY, crate::feeds::Checked::Unverified).await?, crate::feeds::Intaken::Delivered(1));
        let marked: serde_json::Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'feed.item' ORDER BY id DESC LIMIT 1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(marked["unverified"], json!(true));
        store.note_feed_unverified(&legacy).await?;
        store.note_feed_unverified(&legacy).await?;
        let notes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE agent_id = $1 AND type = $2")
            .bind(agent)
            .bind(UNVERIFIED_EVENT)
            .fetch_one(&pool)
            .await?;
        assert_eq!(notes, 1, "the grace note is written once");

        for _ in 0..FAILURES_BEFORE_NOTE + 3 {
            store.note_feed_refusal(&legacy, Verdict::Refused).await?;
        }
        let refusals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE agent_id = $1 AND type = $2")
            .bind(agent)
            .bind(REFUSED_EVENT)
            .fetch_one(&pool)
            .await?;
        assert_eq!(refusals, 1, "repeated refusals leave one quiet note");
        let quiet: serde_json::Value = sqlx::query_scalar("SELECT payload FROM world_events WHERE agent_id = $1 AND type = $2")
            .bind(agent)
            .bind(REFUSED_EVENT)
            .fetch_one(&pool)
            .await?;
        assert_eq!(quiet["quiet"], json!(true));

        let legacy_secret = store.reveal_feed_secret(hook.id).await?.unwrap();
        store.note_feed_verified(hook.id).await?;
        let verified = store.get_feed(hook.id).await?.unwrap();
        assert!(verified.auth_grace_until.is_none() && verified.last_error.is_none());
        assert_eq!(verified.check_request(Sender::at(None), headers(&[]), BODY, Utc::now()), Verdict::Refused, "a verified sender ends the grace");
        let ok = vec![("X-Hub-Signature-256", signature(&legacy_secret, BODY))];
        assert_eq!(verified.check_request(Sender::at(None), headers(&ok), BODY, Utc::now()), Verdict::Accepted(Method::Signature));

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await?;
        Ok(())
    }
}

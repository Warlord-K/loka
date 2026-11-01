use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::world::{InboxItem, NewEvent, WorldAgent, WorldStore, RESET_INBOX_KIND};

pub const EVENT: &str = "agent.context";
pub const COMPACT_INBOX_KIND: &str = "context.compact";
pub const NO_COMPACT: &str = "This CLI cannot compact a session. Reset the context instead.";
pub const NOTHING_TO_COMPACT: &str = "The main session has not started yet, so there is nothing to compact.";
pub const PAUSED: &str = "The agent is paused. Resume it first.";
pub const AUTO_MIN_TOKENS: i64 = 10_000;
pub const AUTO_TOKENS_INVALID: &str =
    "auto_context_tokens must be a whole number of at least 10000, or null to turn it off.";
pub const AUTO_TOKENS_OVER_MAX: &str = "auto_context_tokens must be below max_tokens, the model's context window.";
pub const AUTO_ACTION_INVALID: &str = "auto_context_action must be \"compact\" or \"reset\".";
pub const AUTO_NOTHING_SENT: &str = "Send auto_context_tokens, auto_context_action or both.";
pub const AUTO_FELL_BACK: &str = "This CLI cannot compact a session, so the context was reset instead.";

pub fn window(cli: &str, model: &str) -> Option<i64> {
    let raw = model.trim().to_ascii_lowercase();
    if raw.ends_with("[1m]") || raw.ends_with("-1m") {
        return Some(1_000_000);
    }
    let name = crate::pricing::canonical(&raw);
    let family = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    if name.is_empty() || matches!(name.as_str(), "auto" | "default") {
        return match cli {
            "claude" => Some(200_000),
            "codex" => Some(272_000),
            _ => None,
        };
    }
    if family(&["claude", "opus", "sonnet", "haiku"]) {
        Some(200_000)
    } else if family(&["gpt-4.1"]) {
        Some(1_047_576)
    } else if family(&["gpt-5", "codex", "o3", "o4"]) {
        Some(272_000)
    } else if family(&["gemini"]) {
        Some(1_048_576)
    } else if family(&["grok"]) {
        Some(256_000)
    } else if family(&["kimi"]) {
        Some(262_144)
    } else {
        None
    }
}

pub fn compact_unsupported(cli: &str) -> Option<&'static str> {
    (!matches!(cli, "claude" | "codex" | "omp")).then_some(NO_COMPACT)
}

pub fn used(context_tokens: u64, input_tokens: u64, cached_input_tokens: u64) -> i64 {
    match context_tokens {
        0 => (input_tokens + cached_input_tokens) as i64,
        measured => measured as i64,
    }
}

pub fn is_compact_item(item: &InboxItem) -> bool {
    item.kind == COMPACT_INBOX_KIND
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoAction {
    #[default]
    Compact,
    Reset,
}

impl AutoAction {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "compact" => Some(Self::Compact),
            "reset" => Some(Self::Reset),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Reset => "reset",
        }
    }

    pub fn for_cli(self, cli: &str) -> Self {
        match self {
            Self::Compact if compact_unsupported(cli).is_some() => Self::Reset,
            other => other,
        }
    }

    pub fn fallback_reason(self, cli: &str) -> Option<&'static str> {
        (self.for_cli(cli) != self).then_some(AUTO_FELL_BACK)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AutoContext {
    pub tokens: Option<i64>,
    pub action: AutoAction,
}

impl AutoContext {
    pub fn due(&self, used: i64, pending: bool, cli: &str) -> Option<AutoAction> {
        let limit = self.tokens.filter(|tokens| *tokens > 0)?;
        (!pending && used > 0 && used >= limit).then(|| self.action.for_cli(cli))
    }
}

pub fn valid_auto_tokens(tokens: Option<i64>, max_tokens: Option<i64>) -> Result<Option<i64>, &'static str> {
    match tokens {
        None => Ok(None),
        Some(t) if !(AUTO_MIN_TOKENS..=i64::from(i32::MAX)).contains(&t) => Err(AUTO_TOKENS_INVALID),
        Some(t) if max_tokens.is_some_and(|max| max > 0 && t >= max) => Err(AUTO_TOKENS_OVER_MAX),
        Some(t) => Ok(Some(t)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AutoPatch {
    pub tokens: Option<Option<i64>>,
    pub action: Option<AutoAction>,
}

pub fn parse_auto_patch(body: &Value) -> Result<AutoPatch, &'static str> {
    let tokens = match body.get("auto_context_tokens") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Number(n)) => Some(Some(n.as_i64().ok_or(AUTO_TOKENS_INVALID)?)),
        Some(_) => return Err(AUTO_TOKENS_INVALID),
    };
    let action = match body.get("auto_context_action") {
        None => None,
        Some(Value::String(raw)) => Some(AutoAction::parse(raw).ok_or(AUTO_ACTION_INVALID)?),
        Some(_) => return Err(AUTO_ACTION_INVALID),
    };
    if tokens.is_none() && action.is_none() {
        return Err(AUTO_NOTHING_SENT);
    }
    Ok(AutoPatch { tokens, action })
}

#[derive(Debug, Clone)]
pub enum AutoUpdate {
    NotOwner,
    Invalid(&'static str),
    Saved { view: ContextView, changed: bool },
}

pub fn partition_compact_items(items: Vec<InboxItem>) -> (Vec<InboxItem>, Vec<InboxItem>) {
    items.into_iter().partition(is_compact_item)
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextView {
    pub agent_id: Uuid,
    pub session_id: Option<Uuid>,
    pub used_tokens: i64,
    pub max_tokens: Option<i64>,
    pub model: String,
    pub cli: String,
    pub updated_at: Option<DateTime<Utc>>,
    pub compact_supported: bool,
    pub compact_unsupported_reason: Option<String>,
    pub pending: Option<String>,
    pub auto_context_tokens: Option<i64>,
    pub auto_context_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactRequest {
    Queued,
    AlreadyPending,
    Refused(&'static str),
}

impl WorldStore {
    pub async fn context_view(&self, agent: &WorldAgent) -> Result<ContextView> {
        let main = self.main_session(agent.id).await?;
        let pending = sqlx::query_scalar::<_, String>(
            "SELECT kind FROM agent_inbox WHERE agent_id = $1 AND kind = ANY($2) AND status IN ('pending', 'processing')
             ORDER BY (kind = $3) DESC, id LIMIT 1",
        )
        .bind(agent.id)
        .bind(vec![COMPACT_INBOX_KIND, RESET_INBOX_KIND])
        .bind(RESET_INBOX_KIND)
        .fetch_optional(self.pool())
        .await?;
        let (cli, model) = match &main {
            Some(session) => (session.backend.clone(), session.model.clone()),
            None => (agent.cli.clone(), agent.model.clone()),
        };
        let auto = self.auto_context(agent.id).await?;
        let started = main.as_ref().is_some_and(|session| session.cli_session_id.is_some());
        let reason = compact_unsupported(&cli).or((!started).then_some(NOTHING_TO_COMPACT));
        Ok(ContextView {
            agent_id: agent.id,
            session_id: main.as_ref().map(|session| session.id),
            used_tokens: main.as_ref().map_or(0, |session| session.context_tokens.max(0)),
            max_tokens: window(&cli, &model),
            updated_at: main.as_ref().map(|session| session.updated_at),
            compact_supported: reason.is_none(),
            compact_unsupported_reason: reason.map(str::to_string),
            pending: pending.map(|kind| if kind == RESET_INBOX_KIND { "reset" } else { "compact" }.to_string()),
            auto_context_tokens: auto.tokens,
            auto_context_action: auto.action.as_str().to_string(),
            model,
            cli,
        })
    }

    pub async fn auto_context(&self, agent_id: Uuid) -> Result<AutoContext> {
        let row: Option<(Option<i32>, String)> =
            sqlx::query_as("SELECT auto_context_tokens, auto_context_action FROM agent_profiles WHERE id = $1")
                .bind(agent_id)
                .fetch_optional(self.pool())
                .await?;
        Ok(row.map_or_else(AutoContext::default, |(tokens, action)| AutoContext {
            tokens: tokens.map(i64::from),
            action: AutoAction::parse(&action).unwrap_or_default(),
        }))
    }

    pub async fn update_auto_context(&self, agent: &WorldAgent, user_id: Uuid, body: &Value) -> Result<AutoUpdate> {
        if agent.owner_user_id != Some(user_id) {
            return Ok(AutoUpdate::NotOwner);
        }
        let patch = match parse_auto_patch(body) {
            Ok(patch) => patch,
            Err(reason) => return Ok(AutoUpdate::Invalid(reason)),
        };
        let view = self.context_view(agent).await?;
        let tokens = match patch.tokens {
            Some(sent) => match valid_auto_tokens(sent, view.max_tokens) {
                Ok(tokens) => tokens,
                Err(reason) => return Ok(AutoUpdate::Invalid(reason)),
            },
            None => view.auto_context_tokens,
        };
        let action = patch.action.unwrap_or_else(|| AutoAction::parse(&view.auto_context_action).unwrap_or_default());
        let changed = self.set_auto_context(agent.id, AutoContext { tokens, action }).await?;
        if changed {
            self.clear_auto_compact_floor(agent.id).await?;
            let _ = self.announce_context(agent.id).await;
        }
        Ok(AutoUpdate::Saved { view: self.context_view(agent).await?, changed })
    }

    pub async fn set_auto_context(&self, agent_id: Uuid, auto: AutoContext) -> Result<bool> {
        let tokens = auto.tokens.map(|tokens| i32::try_from(tokens).unwrap_or(i32::MAX));
        let action = auto.action.as_str();
        let changed = sqlx::query(
            "UPDATE agent_profiles SET auto_context_tokens = $2, auto_context_action = $3
             WHERE id = $1 AND (auto_context_tokens IS DISTINCT FROM $2 OR auto_context_action IS DISTINCT FROM $3)",
        )
        .bind(agent_id)
        .bind(tokens)
        .bind(action)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(changed > 0)
    }

    pub async fn auto_context_cleanup(&self, agent_id: Uuid, run_id: Uuid) -> Result<Option<AutoAction>> {
        let Some(agent) = self.get_agent(agent_id).await? else {
            return Ok(None);
        };
        if agent.paused_reason.is_some() {
            return Ok(None);
        }
        let auto = self.auto_context(agent.id).await?;
        let Some(limit) = auto.tokens else {
            return Ok(None);
        };
        let view = self.context_view(&agent).await?;
        let Some(session) = view.session_id else {
            return Ok(None);
        };
        if view.used_tokens < limit {
            self.clear_auto_compact_floor(agent.id).await?;
        }
        let Some(action) = auto.due(view.used_tokens, view.pending.is_some(), &view.cli) else {
            return Ok(None);
        };
        let fallback_reason = auto.action.fallback_reason(&view.cli);
        let detail = json!({
            "origin": "auto",
            "auto_context_tokens": limit,
            "used_tokens": view.used_tokens,
            "max_tokens": view.max_tokens,
            "requested_action": auto.action.as_str(),
            "action": action.as_str(),
            "fallback_reason": fallback_reason,
        });
        let queued = match action {
            AutoAction::Compact => {
                self.should_auto_compact(session, run_id, view.used_tokens, limit - 1).await?
                    && self.request_compact(&agent, detail.clone()).await? == CompactRequest::Queued
            }
            AutoAction::Reset => self.request_context_reset(agent.id, detail.clone()).await?,
        };
        if !queued {
            return Ok(None);
        }
        tracing::info!(
            agent = %agent.slug,
            limit,
            used = view.used_tokens,
            action = action.as_str(),
            fallback = fallback_reason.unwrap_or(""),
            "Queued automatic context cleanup"
        );
        let _ = self.announce_context_with(agent.id, Some(detail)).await;
        Ok(Some(action))
    }

    pub async fn announce_context(&self, agent_id: Uuid) -> Result<()> {
        self.announce_context_with(agent_id, None).await
    }

    async fn announce_context_with(&self, agent_id: Uuid, auto: Option<Value>) -> Result<()> {
        let Some(agent) = self.get_agent(agent_id).await? else {
            return Ok(());
        };
        let view = self.context_view(&agent).await?;
        let mut payload = serde_json::to_value(&view)?;
        if let Value::Object(map) = &mut payload {
            map.insert("quiet".into(), json!(true));
            if let Some(auto) = auto {
                map.insert("auto".into(), auto);
            }
        }
        let mut event = NewEvent::new(EVENT).agent(agent_id).payload(payload);
        if let Some(session) = view.session_id {
            event = event.session(session);
        }
        self.emit(event, None).await?;
        Ok(())
    }

    pub async fn request_compact(&self, agent: &WorldAgent, payload: Value) -> Result<CompactRequest> {
        if agent.paused_reason.is_some() {
            return Ok(CompactRequest::Refused(PAUSED));
        }
        let view = self.context_view(agent).await?;
        if let Some(reason) = compact_unsupported(&view.cli) {
            return Ok(CompactRequest::Refused(reason));
        }
        if !view.compact_supported {
            return Ok(CompactRequest::Refused(NOTHING_TO_COMPACT));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(agent.id)
            .fetch_one(&mut *tx)
            .await?;
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM agent_inbox WHERE agent_id = $1 AND kind = ANY($2) AND status IN ('pending', 'processing'))",
        )
        .bind(agent.id)
        .bind(vec![COMPACT_INBOX_KIND, RESET_INBOX_KIND])
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            return Ok(CompactRequest::AlreadyPending);
        }
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, $2, $3)")
            .bind(agent.id)
            .bind(COMPACT_INBOX_KIND)
            .bind(&payload)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(CompactRequest::Queued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_per_cli_and_model() {
        let cases = [
            ("claude", "opus", Some(200_000)),
            ("claude", "claude-opus-5-5", Some(200_000)),
            ("claude", "sonnet[1m]", Some(1_000_000)),
            ("claude", "claude-sonnet-4-5-20250929", Some(200_000)),
            ("claude", "", Some(200_000)),
            ("codex", "gpt-5.3-codex", Some(272_000)),
            ("codex", "gpt-5-codex-high", Some(272_000)),
            ("codex", "default", Some(272_000)),
            ("cursor", "auto", None),
            ("cursor", "cursor-sonnet-4.5", Some(200_000)),
            ("cursor", "gpt-5", Some(272_000)),
            ("cursor", "composer-2.5", None),
            ("omp", "fal/kimi-k3", Some(262_144)),
            ("omp", "openrouter/google/gemini-3-pro", Some(1_048_576)),
            ("omp", "xai/grok-5", Some(256_000)),
            ("opencode", "anthropic/claude-sonnet-4-5", Some(200_000)),
            ("opencode", "openai/gpt-4.1", Some(1_047_576)),
            ("opencode", "local/llama-4", None),
        ];
        for (cli, model, expected) in cases {
            assert_eq!(window(cli, model), expected, "{cli} {model}");
        }
    }

    #[test]
    fn compaction_follows_the_cli() {
        for cli in ["claude", "codex", "omp"] {
            assert_eq!(compact_unsupported(cli), None, "{cli}");
        }
        for cli in ["cursor", "opencode", "grok"] {
            assert_eq!(compact_unsupported(cli), Some(NO_COMPACT), "{cli}");
        }
    }

    #[test]
    fn auto_tokens_must_be_at_least_ten_thousand_and_below_the_window() {
        assert_eq!(valid_auto_tokens(None, Some(200_000)), Ok(None));
        assert_eq!(valid_auto_tokens(Some(10_000), Some(200_000)), Ok(Some(10_000)));
        assert_eq!(valid_auto_tokens(Some(199_999), Some(200_000)), Ok(Some(199_999)));
        assert_eq!(valid_auto_tokens(Some(5_000_000), None), Ok(Some(5_000_000)));
        assert_eq!(valid_auto_tokens(Some(5_000_000), Some(0)), Ok(Some(5_000_000)));
        assert_eq!(valid_auto_tokens(Some(200_000), Some(200_000)), Err(AUTO_TOKENS_OVER_MAX));
        assert_eq!(valid_auto_tokens(Some(200_001), Some(200_000)), Err(AUTO_TOKENS_OVER_MAX));
        assert_eq!(valid_auto_tokens(Some(9_999), Some(5_000)), Err(AUTO_TOKENS_INVALID));
        for bad in [i64::MIN, -1, 0, 1, 9_999, i64::from(i32::MAX) + 1] {
            assert_eq!(valid_auto_tokens(Some(bad), None), Err(AUTO_TOKENS_INVALID), "{bad}");
        }
    }

    #[test]
    fn auto_actions_parse_only_compact_or_reset() {
        assert_eq!(AutoAction::parse("compact"), Some(AutoAction::Compact));
        assert_eq!(AutoAction::parse("reset"), Some(AutoAction::Reset));
        for bad in ["", "Compact", "clear", " reset"] {
            assert_eq!(AutoAction::parse(bad), None, "{bad:?}");
        }
        for action in [AutoAction::Compact, AutoAction::Reset] {
            assert_eq!(AutoAction::parse(action.as_str()), Some(action));
        }
    }

    #[test]
    fn auto_patches_need_a_field_and_refuse_a_null_action() {
        assert_eq!(parse_auto_patch(&json!({})), Err(AUTO_NOTHING_SENT));
        assert_eq!(parse_auto_patch(&json!({ "other": 1 })), Err(AUTO_NOTHING_SENT));
        assert_eq!(parse_auto_patch(&json!({ "auto_context_action": null })), Err(AUTO_ACTION_INVALID));
        assert_eq!(parse_auto_patch(&json!({ "auto_context_action": "clear" })), Err(AUTO_ACTION_INVALID));
        assert_eq!(parse_auto_patch(&json!({ "auto_context_tokens": "50000" })), Err(AUTO_TOKENS_INVALID));
        assert_eq!(parse_auto_patch(&json!({ "auto_context_tokens": 5.5 })), Err(AUTO_TOKENS_INVALID));
        assert_eq!(
            parse_auto_patch(&json!({ "auto_context_tokens": null })),
            Ok(AutoPatch { tokens: Some(None), action: None })
        );
        assert_eq!(
            parse_auto_patch(&json!({ "auto_context_tokens": 50_000, "auto_context_action": "reset" })),
            Ok(AutoPatch { tokens: Some(Some(50_000)), action: Some(AutoAction::Reset) })
        );
    }

    #[test]
    fn auto_action_defaults_to_compact() {
        assert_eq!(AutoContext::default().action, AutoAction::Compact);
        assert_eq!(AutoAction::default(), AutoAction::Compact);
    }

    #[test]
    fn auto_context_fires_at_the_limit() {
        let auto = AutoContext { tokens: Some(160_000), action: AutoAction::Compact };
        assert_eq!(auto.due(159_999, false, "claude"), None);
        assert_eq!(auto.due(160_000, false, "claude"), Some(AutoAction::Compact));
        assert_eq!(auto.due(190_000, false, "claude"), Some(AutoAction::Compact));
        let reset = AutoContext { tokens: Some(10_000), action: AutoAction::Reset };
        assert_eq!(reset.due(9_999, false, "codex"), None);
        assert_eq!(reset.due(10_000, false, "codex"), Some(AutoAction::Reset));
    }

    #[test]
    fn auto_context_waits_for_a_limit_and_no_pending_cleanup() {
        let off = AutoContext { tokens: None, action: AutoAction::Compact };
        assert_eq!(off.due(199_000, false, "claude"), None);
        let on = AutoContext { tokens: Some(100_000), action: AutoAction::Compact };
        assert_eq!(on.due(199_000, true, "claude"), None);
        assert_eq!(on.due(0, false, "claude"), None);
        let default_action = AutoContext { tokens: Some(100_000), ..AutoContext::default() };
        assert_eq!(default_action.due(150_000, false, "claude"), Some(AutoAction::Compact));
    }

    #[test]
    fn auto_compact_falls_back_to_reset_on_clis_that_cannot_compact() {
        let compact = AutoContext { tokens: Some(1_000), action: AutoAction::Compact };
        let reset = AutoContext { tokens: Some(1_000), action: AutoAction::Reset };
        for cli in ["claude", "codex", "omp"] {
            assert_eq!(compact.due(1_500, false, cli), Some(AutoAction::Compact), "{cli}");
            assert_eq!(reset.due(1_500, false, cli), Some(AutoAction::Reset), "{cli}");
        }
        for cli in ["claude", "codex", "omp"] {
            assert_eq!(AutoAction::Compact.fallback_reason(cli), None, "{cli}");
        }
        for cli in ["cursor", "opencode"] {
            assert_eq!(compact.due(1_500, false, cli), Some(AutoAction::Reset), "{cli}");
            assert_eq!(reset.due(1_500, false, cli), Some(AutoAction::Reset), "{cli}");
            assert_eq!(AutoAction::Compact.fallback_reason(cli), Some(AUTO_FELL_BACK), "{cli}");
            assert_eq!(AutoAction::Reset.fallback_reason(cli), None, "{cli}");
        }
    }

    #[test]
    fn tokens_prefer_the_last_call_context() {
        let cases = [
            ("claude", 91_000, 3, 90_000, 91_000),
            ("codex", 40_000, 120_000, 300_000, 40_000),
            ("omp", 117, 50, 60, 117),
            ("cursor", 0, 12_000, 30_000, 42_000),
            ("cursor acp", 0, 0, 0, 0),
        ];
        for (cli, context, input, cached, expected) in cases {
            assert_eq!(used(context, input, cached), expected, "{cli}");
        }
    }

    fn inbox(id: i64, kind: &str) -> InboxItem {
        InboxItem {
            id,
            agent_id: Uuid::nil(),
            kind: kind.into(),
            event_seq: None,
            payload: json!({}),
            status: "processing".into(),
            attempts: 1,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn compact_items_split_from_the_rest() {
        let (compact, rest) =
            partition_compact_items(vec![inbox(1, "message"), inbox(2, COMPACT_INBOX_KIND), inbox(3, RESET_INBOX_KIND)]);
        assert_eq!(compact.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(rest.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 3]);
    }
}

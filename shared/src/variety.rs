use anyhow::Result;
use serde_json::Value;
use uuid::Uuid;

use crate::starter_team::{strongest, CliModels};
use crate::world::{ReviewerChoice, WorldStore};

pub const CREW_PREVIEW: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Helper,
    Reviewer,
    Seat,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pins {
    pub quest: Option<ReviewerChoice>,
    pub agent: Option<ReviewerChoice>,
    pub rule: Option<ReviewerChoice>,
}

impl Pins {
    pub fn rule_only(rule: Option<ReviewerChoice>) -> Self {
        Self { rule, ..Self::default() }
    }

    fn first(&self) -> Option<&ReviewerChoice> {
        self.quest.as_ref().or(self.agent.as_ref()).or(self.rule.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Pick {
    pub cli: String,
    pub model: String,
}

impl Pick {
    pub fn new(cli: &str, model: &str) -> Self {
        Self { cli: cli.to_string(), model: model.to_string() }
    }

    pub fn choice(&self) -> ReviewerChoice {
        ReviewerChoice { cli: Some(self.cli.clone()), model: self.model.clone() }
    }

    fn from_choice(choice: &ReviewerChoice, base: &Pick) -> Self {
        Self { cli: choice.cli.clone().unwrap_or_else(|| base.cli.clone()), model: choice.model.clone() }
    }
}

pub const RULE_MODEL_ERROR: &str = "model must be {\"cli\", \"model\"} with a model, or {} to clear the pin";

pub fn rule_model(value: &Value) -> std::result::Result<Option<ReviewerChoice>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Object(o) if o.get("model").and_then(Value::as_str).is_none_or(|m| m.trim().is_empty()) => Ok(None),
        Value::Object(o) => ReviewerChoice::new(o.get("cli").and_then(Value::as_str), o.get("model").and_then(Value::as_str).unwrap_or(""))
            .map(Some)
            .map_err(|e| e.replace("a reviewer", "a pinned rule").replace("reviewer CLI", "CLI")),
        _ => Err(RULE_MODEL_ERROR.into()),
    }
}

pub fn stated(entries: Option<&Value>, slot: usize) -> Option<ReviewerChoice> {
    entries?.as_array()?.get(slot).and_then(ReviewerChoice::from_value)
}

fn lineup(base: &Pick, available: &[CliModels]) -> Vec<Pick> {
    let mut line = vec![base.clone()];
    for entry in available.iter().filter(|c| c.cli != base.cli) {
        if let Some(model) = strongest(&entry.models) {
            line.push(Pick::new(&entry.cli, model));
        }
    }
    line
}

pub fn pick(role: Role, slot: usize, pins: &Pins, base: &Pick, available: &[CliModels]) -> Pick {
    if let Some(choice) = pins.first() {
        return Pick::from_choice(choice, base);
    }
    let line = lineup(base, available);
    match role {
        Role::Reviewer => line.get(1).cloned().unwrap_or_else(|| {
            let others: Vec<String> = available
                .iter()
                .filter(|c| c.cli == base.cli)
                .flat_map(|c| c.models.iter().cloned())
                .filter(|m| *m != base.model)
                .collect();
            strongest(&others).map(|model| Pick::new(&base.cli, model)).unwrap_or_else(|| base.clone())
        }),
        Role::Helper | Role::Seat => line[slot % line.len()].clone(),
    }
}

impl WorldStore {
    pub async fn rule_model_pin(&self, owner: Option<Uuid>, org: Option<Uuid>) -> Result<Option<ReviewerChoice>> {
        let Some(owner) = owner else { return Ok(None) };
        let pin = sqlx::query_scalar::<_, Value>(
            "SELECT model FROM world_rules
             WHERE user_id = $1 AND enabled AND model IS NOT NULL
               AND (org_id IS NULL OR $2::uuid IS NULL OR org_id = $2)
             ORDER BY position, created_at LIMIT 1",
        )
        .bind(owner)
        .bind(org)
        .fetch_optional(self.pool())
        .await?;
        Ok(pin.as_ref().and_then(ReviewerChoice::from_value))
    }

    pub async fn set_rule_model(&self, id: Uuid, user_id: Uuid, global_org: Option<Uuid>, model: Option<&ReviewerChoice>) -> Result<Option<crate::world::WorldRule>> {
        Ok(sqlx::query_as::<_, crate::world::WorldRule>(
            "UPDATE world_rules SET model = $4, updated_at = NOW()
             WHERE id = $1 AND scope <> 'allow' AND (user_id = $2 OR (user_id IS NULL AND org_id = $3))
             RETURNING *",
        )
        .bind(id)
        .bind(user_id)
        .bind(global_org)
        .bind(model.map(ReviewerChoice::to_value))
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn signed_in_clis(&self, owner: Option<Uuid>, device: Option<Uuid>, base: &Pick) -> Result<Vec<CliModels>> {
        let (Some(owner), Some(device)) = (owner, device) else { return Ok(Vec::new()) };
        let mut signals = crate::readiness::gather(self.pool(), owner).await?;
        signals.own.retain(|runner| runner.id == device);
        if signals.own.is_empty() {
            return Ok(Vec::new());
        }
        Ok(crate::starter_team::available_clis(&signals, &base.cli, &base.model))
    }

    pub async fn default_reviewer(&self, agent: &crate::world::WorldAgent, quest_cli: &str, quest_model: &str) -> Result<Pick> {
        let base = Pick::new(quest_cli, quest_model);
        let pins = Pins::rule_only(self.rule_model_pin(agent.owner_user_id, agent.org_id).await?);
        if pins.rule.is_some() {
            return Ok(pick(Role::Reviewer, 0, &pins, &base, &[]));
        }
        let available = self.signed_in_clis(agent.owner_user_id, agent.runner_id, &base).await?;
        Ok(pick(Role::Reviewer, 0, &pins, &base, &available))
    }

    pub async fn crew_preview(&self, agent: &crate::world::WorldAgent) -> Result<Vec<Pick>> {
        let base = Pick::new(&agent.cli, &agent.model);
        let pins = Pins::rule_only(self.rule_model_pin(agent.owner_user_id, agent.org_id).await?);
        let available = self.signed_in_clis(agent.owner_user_id, agent.runner_id, &base).await?;
        Ok((0..CREW_PREVIEW).map(|slot| pick(Role::Helper, slot, &pins, &base, &available)).collect())
    }

    pub async fn reviewer_for(&self, agent: &crate::world::WorldAgent, session: &crate::world::AgentSession) -> Result<(String, String)> {
        let chosen = session
            .reviewer
            .as_ref()
            .and_then(ReviewerChoice::from_value)
            .or_else(|| agent.reviewer.as_ref().and_then(ReviewerChoice::from_value));
        if chosen.is_some() {
            return Ok(session.review_target(agent.reviewer.as_ref()));
        }
        let picked = self.default_reviewer(agent, &session.backend, &session.model).await?;
        Ok((picked.cli, picked.model))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn available() -> Vec<CliModels> {
        vec![
            CliModels { cli: "claude".into(), models: vec!["opus".into(), "sonnet".into(), "haiku".into()] },
            CliModels { cli: "codex".into(), models: vec!["gpt-5.5".into(), "gpt-5.5-mini".into()] },
            CliModels { cli: "omp".into(), models: vec!["fal/kimi-k3".into()] },
        ]
    }

    fn choice(cli: &str, model: &str) -> ReviewerChoice {
        ReviewerChoice { cli: Some(cli.into()), model: model.into() }
    }

    #[test]
    fn helpers_spread_across_signed_in_clis_and_wrap_around() {
        let base = Pick::new("claude", "opus");
        let picks: Vec<Pick> = (0..4).map(|slot| pick(Role::Helper, slot, &Pins::default(), &base, &available())).collect();
        assert_eq!(picks[0], base, "the first helper keeps the lead's own CLI and model");
        assert_eq!(picks[1], Pick::new("codex", "gpt-5.5"), "the next one takes another CLI's strongest model");
        assert_eq!(picks[2], Pick::new("omp", "fal/kimi-k3"));
        assert_eq!(picks[3], base, "it wraps around");
    }

    #[test]
    fn only_one_cli_falls_back_to_it_with_no_error() {
        let base = Pick::new("codex", "gpt-5.5");
        let only = vec![CliModels { cli: "codex".into(), models: vec!["gpt-5.5".into(), "gpt-5.5-mini".into()] }];
        for slot in 0..3 {
            assert_eq!(pick(Role::Helper, slot, &Pins::default(), &base, &only), base);
            assert_eq!(pick(Role::Seat, slot, &Pins::default(), &base, &[]), base, "nothing known still picks the base");
        }
        assert_eq!(pick(Role::Reviewer, 0, &Pins::default(), &base, &only), Pick::new("codex", "gpt-5.5-mini"), "a reviewer still takes a different model");
        let single = vec![CliModels { cli: "codex".into(), models: vec!["gpt-5.5".into()] }];
        assert_eq!(pick(Role::Reviewer, 0, &Pins::default(), &base, &single), base, "with nothing else it reviews on the same pick");
    }

    #[test]
    fn a_rule_pinning_a_model_wins_over_the_mix() {
        let base = Pick::new("claude", "opus");
        let pins = Pins::rule_only(Some(choice("codex", "gpt-5.5-mini")));
        for role in [Role::Helper, Role::Reviewer, Role::Seat] {
            for slot in 0..3 {
                assert_eq!(pick(role, slot, &pins, &base, &available()), Pick::new("codex", "gpt-5.5-mini"));
            }
        }
        let same_cli = Pins::rule_only(Some(ReviewerChoice { cli: None, model: "sonnet".into() }));
        assert_eq!(pick(Role::Helper, 1, &same_cli, &base, &available()), Pick::new("claude", "sonnet"), "a model with no cli stays on the base CLI");
    }

    #[test]
    fn a_user_override_beats_the_rule_and_the_mix() {
        let base = Pick::new("claude", "opus");
        let rule = Some(choice("codex", "gpt-5.5-mini"));
        let agent = Pins { agent: Some(choice("omp", "fal/kimi-k3")), rule: rule.clone(), ..Pins::default() };
        assert_eq!(pick(Role::Helper, 2, &agent, &base, &available()), Pick::new("omp", "fal/kimi-k3"), "the per-agent choice beats the rule");
        let quest = Pins { quest: Some(choice("claude", "haiku")), agent: Some(choice("omp", "fal/kimi-k3")), rule };
        assert_eq!(pick(Role::Helper, 2, &quest, &base, &available()), Pick::new("claude", "haiku"), "the per-quest choice beats both");
        let quest_list = json!([{}, {"cli": "codex", "model": "gpt-5.5"}]);
        assert_eq!(stated(Some(&quest_list), 0), None, "an empty entry lets Loka pick");
        assert_eq!(stated(Some(&quest_list), 1), Some(choice("codex", "gpt-5.5")));
    }

    #[test]
    fn the_reviewer_default_takes_another_cli_than_the_quest() {
        let quest = Pick::new("codex", "gpt-5.5");
        assert_eq!(pick(Role::Reviewer, 0, &Pins::default(), &quest, &available()), Pick::new("claude", "opus"));
        let claude_quest = Pick::new("claude", "opus");
        assert_eq!(pick(Role::Reviewer, 0, &Pins::default(), &claude_quest, &available()), Pick::new("codex", "gpt-5.5"));
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn the_reviewer_default_comes_from_the_agents_own_signed_in_clis() {
        let Ok(url) = std::env::var("QUESTIONS_TEST_DATABASE_URL") else { return };
        let max = std::env::var("DATABASE_MAX_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(2);
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(max).connect(&url).await.unwrap();
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (name, email) VALUES ('Ana', $1) RETURNING id").bind(format!("{tag}@variety.test")).fetch_one(&pool).await.unwrap();
        let runner = |name: &'static str, caps: Value| {
            let pool = pool.clone();
            async move {
                let token: Vec<u8> = Uuid::new_v4().as_bytes().iter().chain(Uuid::new_v4().as_bytes()).copied().collect();
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO world_runners (owner_user_id, name, platform, token_hash, status, capabilities, last_seen_at) VALUES ($1, $2, 'macos', $3, 'online', $4, NOW()) RETURNING id",
                )
                .bind(owner)
                .bind(name)
                .bind(token)
                .bind(caps)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let mac = runner("Studio", json!({
            "backends": { "claude": true, "codex": true, "cursor": true },
            "models": { "claude": ["opus", "sonnet"], "codex": ["gpt-5.5", "gpt-5.5-mini"], "cursor": ["auto"] },
            "tools": { "claude": { "installed": true, "signed_in": true }, "codex": { "installed": true, "signed_in": true }, "cursor": { "installed": true, "signed_in": false } }
        }))
        .await;
        runner("Laptop", json!({ "backends": { "omp": true }, "models": { "omp": ["fal/kimi-k3"] } })).await;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, avatar_emoji, cli, model, owner_user_id, visibility) VALUES ($1, 'Pip', 'a', 'codex', 'gpt-5.5', $2, 'private') RETURNING id",
        )
        .bind(format!("v-{}", &tag[..12]))
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(agent).bind(mac).execute(&pool).await.unwrap();
        let session = sqlx::query_as::<_, crate::world::AgentSession>(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'task', 'codex', 'gpt-5.5', 'active') RETURNING *",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await
        .unwrap();
        let profile = store.get_agent(agent).await.unwrap().unwrap();
        assert_eq!(store.reviewer_for(&profile, &session).await.unwrap(), ("claude".to_string(), "opus".to_string()), "a different CLI signed in on the agent's own Mac");
        let homeless = sqlx::query_as::<_, crate::world::AgentSession>(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'task', 'claude', 'opus', 'active') RETURNING *",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut cloud = profile.clone();
        cloud.runner_id = None;
        assert_eq!(store.reviewer_for(&cloud, &homeless).await.unwrap(), ("claude".to_string(), "opus".to_string()), "with no device the review keeps the quest's own CLI and model");
        assert!(store.signed_in_clis(Some(owner), None, &Pick::new("claude", "opus")).await.unwrap().is_empty(), "no device pools no other Mac's CLIs");
        let only_codex = Pick::new("codex", "gpt-5.5");
        let clis = store.signed_in_clis(Some(owner), Some(mac), &only_codex).await.unwrap();
        assert!(clis.iter().all(|c| c.cli != "cursor"), "signed out CLIs are never picked");
        assert!(clis.iter().all(|c| c.cli != "omp"), "CLIs on another device are never picked");
        let rule: Uuid = sqlx::query_scalar("INSERT INTO world_rules (user_id, scope, body) VALUES ($1, 'work', 'Use sonnet for side work') RETURNING id").bind(owner).fetch_one(&pool).await.unwrap();
        let pin = rule_model(&json!({ "cli": "claude", "model": "sonnet" })).unwrap();
        store.set_rule_model(rule, owner, None, pin.as_ref()).await.unwrap().unwrap();
        assert_eq!(store.reviewer_for(&profile, &session).await.unwrap(), ("claude".to_string(), "sonnet".to_string()), "a rule pinning a model wins over the mix");
        sqlx::query("UPDATE agent_profiles SET reviewer = $2 WHERE id = $1").bind(agent).bind(json!({ "cli": "codex", "model": "gpt-5.5-mini" })).execute(&pool).await.unwrap();
        let profile = store.get_agent(agent).await.unwrap().unwrap();
        assert_eq!(store.reviewer_for(&profile, &session).await.unwrap(), ("codex".to_string(), "gpt-5.5-mini".to_string()), "the agent's own reviewer choice wins over the rule");
        let allow: Uuid = sqlx::query_scalar("INSERT INTO world_rules (user_id, scope, body) VALUES ($1, 'allow', 'merge') RETURNING id").bind(owner).fetch_one(&pool).await.unwrap();
        assert!(store.set_rule_model(allow, owner, None, pin.as_ref()).await.unwrap().is_none(), "approved actions never pin a model");
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn the_crew_preview_comes_from_the_picker_for_every_device_state() {
        let Ok(url) = std::env::var("QUESTIONS_TEST_DATABASE_URL") else { return };
        let max = std::env::var("DATABASE_MAX_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(2);
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(max).connect(&url).await.unwrap();
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (name, email) VALUES ('Ana', $1) RETURNING id").bind(format!("{tag}@preview.test")).fetch_one(&pool).await.unwrap();
        let device = |caps: Value, seen_minutes: i64| {
            let pool = pool.clone();
            async move {
                let token: Vec<u8> = Uuid::new_v4().as_bytes().iter().chain(Uuid::new_v4().as_bytes()).copied().collect();
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO world_runners (owner_user_id, name, platform, token_hash, status, capabilities, last_seen_at)
                     VALUES ($1, 'Mac', 'macos', $2, 'online', $3, NOW() - make_interval(mins => $4::int)) RETURNING id",
                )
                .bind(owner)
                .bind(token)
                .bind(caps)
                .bind(seen_minutes as i32)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let agent = |runner: Option<Uuid>| {
            let pool = pool.clone();
            let store = store.clone();
            async move {
                let id: Uuid = sqlx::query_scalar(
                    "INSERT INTO agent_profiles (slug, name, avatar_emoji, cli, model, owner_user_id, visibility) VALUES ($1, 'Pip', 'a', 'claude', 'opus', $2, 'private') RETURNING id",
                )
                .bind(format!("p-{}", &Uuid::new_v4().simple().to_string()[..12]))
                .bind(owner)
                .fetch_one(&pool)
                .await
                .unwrap();
                sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(id).bind(runner).execute(&pool).await.unwrap();
                store.get_agent(id).await.unwrap().unwrap()
            }
        };
        let labels = |picks: Vec<Pick>| picks.into_iter().map(|p| format!("{} {}", p.cli, p.model)).collect::<Vec<_>>();
        let mixed = device(json!({ "backends": { "claude": true, "codex": true }, "models": { "claude": ["opus", "sonnet"], "codex": ["gpt-5.5", "gpt-5.5-mini"] } }), 0).await;
        let on_mixed = agent(Some(mixed)).await;
        assert_eq!(labels(store.crew_preview(&on_mixed).await.unwrap()), vec!["claude opus", "codex gpt-5.5", "claude opus", "codex gpt-5.5"], "four slots from the picker");
        let single = device(json!({ "backends": { "claude": true }, "models": { "claude": ["opus", "sonnet"] } }), 0).await;
        let on_single = agent(Some(single)).await;
        assert_eq!(labels(store.crew_preview(&on_single).await.unwrap()), vec!["claude opus"; 4], "one CLI keeps every slot on it");
        let asleep = device(json!({ "backends": { "claude": true, "codex": true }, "models": { "codex": ["gpt-5.5"] } }), 30).await;
        let on_asleep = agent(Some(asleep)).await;
        assert_eq!(labels(store.crew_preview(&on_asleep).await.unwrap()), vec!["claude opus"; 4], "an offline device gives the base picks, not an error");
        let homeless = agent(None).await;
        assert_eq!(labels(store.crew_preview(&homeless).await.unwrap()), vec!["claude opus"; 4], "no device gives the base picks");
        let rule: Uuid = sqlx::query_scalar("INSERT INTO world_rules (user_id, scope, body) VALUES ($1, 'all', 'One model') RETURNING id").bind(owner).fetch_one(&pool).await.unwrap();
        store.set_rule_model(rule, owner, None, rule_model(&json!({ "cli": "codex", "model": "gpt-5.5-mini" })).unwrap().as_ref()).await.unwrap().unwrap();
        assert_eq!(labels(store.crew_preview(&on_mixed).await.unwrap()), vec!["codex gpt-5.5-mini"; 4], "a rule pinning a model wins");
    }
}

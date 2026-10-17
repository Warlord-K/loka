use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::world::{emit_in, NewEvent, WorldStore};

pub const PREFIX: &str = "github.";
pub const BATCH_MAX: usize = 100;
const TITLE_MAX: usize = 300;
const SNIPPET_MAX: usize = 600;
const SEEN_DAYS: i64 = 45;

pub const EVENT_TYPES: &[(&str, &str)] = &[
    ("github.review_requested", "someone asked your person to review a pull request"),
    ("github.mention", "your person or their team was mentioned in an issue or pull request"),
    ("github.comment", "new comments or reviews on an issue or pull request your person opened or joined"),
    ("github.ci", "a GitHub Actions run your person triggered finished or failed"),
    ("github.assigned", "your person was assigned an issue or pull request"),
    ("github.state_change", "an issue or pull request your person follows was closed, reopened or merged"),
    ("github.other", "any other GitHub notification (subscriptions, releases, security alerts, invitations)"),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GithubNotification {
    pub id: String,
    pub updated_at: DateTime<Utc>,
    pub reason: String,
    pub repo: String,
    pub subject_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub number: Option<i64>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub actor: String,
    #[serde(default)]
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_api_url: Option<String>,
}

pub fn event_type(reason: &str, subject_type: &str) -> &'static str {
    match reason {
        "review_requested" => "github.review_requested",
        "mention" | "team_mention" => "github.mention",
        "assign" => "github.assigned",
        "ci_activity" => "github.ci",
        "state_change" => "github.state_change",
        "comment" | "author" | "manual" => "github.comment",
        _ if subject_type == "CheckSuite" => "github.ci",
        _ => "github.other",
    }
}

pub fn wants_snippet(reason: &str) -> bool {
    matches!(reason, "mention" | "team_mention" | "comment" | "author" | "manual" | "review_requested" | "assign")
}

fn clip(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn repo_name(value: &str) -> bool {
    let mut parts = value.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| !p.is_empty() && p.len() <= 100 && !p.bytes().all(|b| b == b'.') && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

fn login(value: &str) -> bool {
    !value.is_empty() && value.len() <= 60 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'[' | b']' | b'_'))
}

fn word(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub fn subject_link(api_url: &str, repo: &str, subject_type: &str) -> (Option<i64>, String) {
    let repo_url = format!("https://github.com/{repo}");
    let tail = api_url.strip_prefix(&format!("https://api.github.com/repos/{repo}/")).unwrap_or("");
    let mut parts = tail.split('/');
    let (kind, id) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let number = id.parse::<i64>().ok();
    match (kind, number) {
        ("pulls", Some(n)) => (Some(n), format!("{repo_url}/pull/{n}")),
        ("issues", Some(n)) => (Some(n), format!("{repo_url}/issues/{n}")),
        ("discussions", Some(n)) => (Some(n), format!("{repo_url}/discussions/{n}")),
        ("commits", _) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()) => (None, format!("{repo_url}/commit/{id}")),
        _ if subject_type == "CheckSuite" => (None, format!("{repo_url}/actions")),
        _ => (None, repo_url),
    }
}

pub fn from_api(raw: &Value) -> Option<GithubNotification> {
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    let id = s(raw.get("id"));
    let repo = s(raw.get("repository").and_then(|r| r.get("full_name")));
    let subject = raw.get("subject")?;
    let subject_type = s(subject.get("type"));
    let updated_at = DateTime::parse_from_rfc3339(raw.get("updated_at")?.as_str()?).ok()?.with_timezone(&Utc);
    let (number, url) = subject_link(subject.get("url").and_then(Value::as_str).unwrap_or(""), &repo, &subject_type);
    let comment_api_url = subject
        .get("latest_comment_url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://api.github.com/"))
        .map(str::to_string);
    let note = GithubNotification {
        id,
        updated_at,
        reason: s(raw.get("reason")),
        repo,
        subject_type,
        title: s(subject.get("title")),
        number,
        url,
        actor: String::new(),
        snippet: String::new(),
        comment_api_url,
    };
    note.validate().ok()?;
    Some(note)
}

pub fn ci_branch(title: &str) -> Option<String> {
    let rest = title.rsplit_once(" for ")?.1;
    let branch = rest.strip_suffix(" branch").unwrap_or(rest).trim();
    (!branch.is_empty() && branch.len() <= 200 && !branch.contains(char::is_whitespace)).then(|| branch.to_string())
}

impl GithubNotification {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.is_empty() || self.id.len() > 32 || !self.id.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid notification id");
        }
        if !repo_name(&self.repo) {
            return Err("invalid repository");
        }
        if !word(&self.reason, 40) || !word(&self.subject_type, 40) {
            return Err("invalid reason or subject type");
        }
        if !self.url.is_empty() && !self.url.starts_with("https://github.com/") {
            return Err("invalid link");
        }
        if !self.actor.is_empty() && !login(&self.actor) {
            return Err("invalid actor");
        }
        if self.number.is_some_and(|n| n <= 0) {
            return Err("invalid number");
        }
        Ok(())
    }

    pub fn event_type(&self) -> &'static str {
        event_type(&self.reason, &self.subject_type)
    }

    pub fn payload(&self, owner: Uuid) -> Value {
        let mut payload = json!({
            "owner_user_id": owner,
            "notification_id": self.id,
            "repo": self.repo.to_lowercase(),
            "repo_name": self.repo,
            "subject_type": self.subject_type,
            "title": clip(&self.title, TITLE_MAX),
            "url": self.url,
            "reason": self.reason,
            "updated_at": self.updated_at,
        });
        let fields = payload.as_object_mut().expect("object");
        if let Some(number) = self.number {
            fields.insert("number".into(), json!(number));
        }
        if !self.actor.is_empty() {
            fields.insert("actor".into(), json!(self.actor));
        }
        if !self.snippet.trim().is_empty() {
            fields.insert("snippet".into(), json!(clip(&self.snippet, SNIPPET_MAX)));
        }
        if self.event_type() == "github.ci" {
            if let Some(branch) = ci_branch(&self.title) {
                fields.insert("branch".into(), json!(branch));
            }
        }
        payload
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GithubRecorded {
    pub enabled: bool,
    pub primed: bool,
    pub created: usize,
    pub quests: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubFeedSettings {
    pub enabled: bool,
    pub primed_at: Option<DateTime<Utc>>,
    pub last_item_at: Option<DateTime<Utc>>,
}

impl WorldStore {
    pub async fn github_feed_settings(&self, user: Uuid) -> Result<GithubFeedSettings> {
        let row = sqlx::query_as::<_, (bool, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            "SELECT github_feed, github_feed_primed_at, github_feed_seen_at FROM users WHERE id = $1",
        )
        .bind(user)
        .fetch_optional(self.pool())
        .await?;
        let (enabled, primed_at, last_item_at) = row.unwrap_or((false, None, None));
        Ok(GithubFeedSettings { enabled, primed_at, last_item_at })
    }

    pub async fn set_github_feed(&self, user: Uuid, enabled: bool) -> Result<GithubFeedSettings> {
        sqlx::query(
            "UPDATE users SET github_feed = $2,
                    github_feed_primed_at = CASE WHEN $2 AND NOT github_feed THEN NULL ELSE github_feed_primed_at END
             WHERE id = $1",
        )
        .bind(user)
        .bind(enabled)
        .execute(self.pool())
        .await?;
        self.github_feed_settings(user).await
    }

    pub async fn github_feed_runner(&self, runner_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM world_runners w JOIN users u ON u.id = w.owner_user_id
               WHERE w.id = $1 AND w.platform = 'macos' AND u.github_feed AND u.disabled_at IS NULL
                 AND w.id = (
                   SELECT c.id FROM world_runners c
                   WHERE c.owner_user_id = w.owner_user_id AND c.platform = 'macos' AND c.status = 'online'
                     AND c.last_seen_at > NOW() - INTERVAL '75 seconds'
                     AND COALESCE(c.capabilities->'github_feed'->>'available', 'false') = 'true'
                   ORDER BY c.created_at, c.id LIMIT 1))",
        )
        .bind(runner_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn record_github(&self, owner: Uuid, items: &[GithubNotification]) -> Result<GithubRecorded> {
        let mut tx = self.pool().begin().await?;
        let state = sqlx::query_as::<_, (bool, Option<DateTime<Utc>>)>(
            "SELECT github_feed, github_feed_primed_at FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((true, primed_at)) = state else {
            tx.rollback().await?;
            return Ok(GithubRecorded::default());
        };
        let mut out = GithubRecorded { enabled: true, ..GithubRecorded::default() };
        let mut sorted: Vec<&GithubNotification> = items.iter().take(BATCH_MAX).collect();
        sorted.sort_by_key(|item| item.updated_at);
        let mark = |item: &GithubNotification| {
            sqlx::query("INSERT INTO github_seen (user_id, notification_id, updated_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                .bind(owner)
                .bind(item.id.clone())
                .bind(item.updated_at)
        };
        let Some(primed_at) = primed_at else {
            for item in &sorted {
                mark(item).execute(&mut *tx).await?;
            }
            let newest = sorted.last().map(|item| item.updated_at);
            sqlx::query("UPDATE users SET github_feed_primed_at = GREATEST(NOW(), $2), github_feed_seen_at = NOW() WHERE id = $1")
                .bind(owner)
                .bind(newest)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            out.primed = true;
            return Ok(out);
        };
        for item in sorted {
            let fresh = mark(item).execute(&mut *tx).await?.rows_affected() == 1;
            if !fresh || item.updated_at <= primed_at {
                continue;
            }
            let mut payload = item.payload(owner);
            let quests = quests_for(&mut tx, owner, item.event_type(), &payload).await?;
            if !quests.is_empty() {
                payload["task_ids"] = json!(quests);
            }
            let mut event = NewEvent::new(item.event_type());
            event.actor_user_id = Some(owner);
            event.task_id = quests.first().copied();
            event.payload = payload;
            let seq = emit_in(&mut tx, &event, None).await?;
            sqlx::query("UPDATE github_seen SET event_seq = $4 WHERE user_id = $1 AND notification_id = $2 AND updated_at = $3")
                .bind(owner)
                .bind(&item.id)
                .bind(item.updated_at)
                .bind(seq)
                .execute(&mut *tx)
                .await?;
            for quest in quests {
                if !out.quests.contains(&quest) {
                    out.quests.push(quest);
                }
            }
            out.created += 1;
        }
        if !out.quests.is_empty() {
            sqlx::query(
                "UPDATE task_milestones SET next_check_at = NOW(), updated_at = NOW()
                 WHERE task_id = ANY($1) AND kind = 'merge' AND status = 'active'",
            )
            .bind(&out.quests)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE users SET github_feed_seen_at = NOW() WHERE id = $1").bind(owner).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM github_seen WHERE user_id = $1 AND created_at < NOW() - make_interval(days => $2)")
            .bind(owner)
            .bind(SEEN_DAYS as i32)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(out)
    }
}

async fn quests_for(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, owner: Uuid, kind: &str, payload: &Value) -> Result<Vec<Uuid>> {
    let repo = payload.get("repo").and_then(Value::as_str).unwrap_or("");
    let number = payload.get("number").and_then(Value::as_i64);
    let branch = payload.get("branch").and_then(Value::as_str);
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT t.id FROM agent_tasks t
         JOIN agent_profiles p ON p.id = t.assigned_agent_id
         WHERE p.owner_user_id = $1 AND t.status <> 'cancelled'
           AND (
             EXISTS (SELECT 1 FROM agent_subscriptions s
                     WHERE s.task_id = t.id AND s.match <@ $5
                       AND (s.event_type = $2 OR (right(s.event_type, 1) = '.' AND starts_with($2, s.event_type))))
             OR ($4::bigint IS NOT NULL AND world_pr_watch_key(t.pr_url) = jsonb_build_object('repo', $3::text, 'number', $4::bigint))
             OR ($6::text IS NOT NULL AND t.pr_url IS NOT NULL AND t.branch = $6 AND (world_pr_watch_key(t.pr_url)->>'repo') = $3))
           AND EXISTS (SELECT 1 FROM task_milestones m WHERE m.task_id = t.id AND m.kind = 'merge' AND m.status = 'active')
         ORDER BY t.updated_at DESC NULLS LAST
         LIMIT 3",
    )
    .bind(owner)
    .bind(kind)
    .bind(repo)
    .bind(number)
    .bind(payload)
    .bind(branch)
    .fetch_all(&mut **tx)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(reason: &str, kind: &str, url: &str) -> Value {
        json!({
            "id": "1234567",
            "reason": reason,
            "updated_at": "2026-11-14T10:00:00Z",
            "repository": { "full_name": "Warlord-K/world" },
            "subject": { "type": kind, "title": "Fix the bell", "url": url, "latest_comment_url": "https://api.github.com/repos/Warlord-K/world/issues/comments/9" }
        })
    }

    #[test]
    fn reasons_map_to_event_types() {
        assert_eq!(event_type("review_requested", "PullRequest"), "github.review_requested");
        assert_eq!(event_type("mention", "Issue"), "github.mention");
        assert_eq!(event_type("team_mention", "Issue"), "github.mention");
        assert_eq!(event_type("comment", "PullRequest"), "github.comment");
        assert_eq!(event_type("author", "PullRequest"), "github.comment");
        assert_eq!(event_type("ci_activity", "CheckSuite"), "github.ci");
        assert_eq!(event_type("subscribed", "CheckSuite"), "github.ci");
        assert_eq!(event_type("assign", "Issue"), "github.assigned");
        assert_eq!(event_type("state_change", "PullRequest"), "github.state_change");
        assert_eq!(event_type("security_alert", "RepositoryVulnerabilityAlert"), "github.other");
        for (kind, _) in EVENT_TYPES {
            assert!(kind.starts_with(PREFIX));
            assert!(crate::world::normalize_event_type(kind).as_deref() == Some(*kind));
        }
    }

    #[test]
    fn parses_notifications_and_links() {
        let pr = from_api(&raw("comment", "PullRequest", "https://api.github.com/repos/Warlord-K/world/pulls/42")).unwrap();
        assert_eq!(pr.number, Some(42));
        assert_eq!(pr.url, "https://github.com/Warlord-K/world/pull/42");
        assert!(pr.comment_api_url.is_some());
        let issue = from_api(&raw("mention", "Issue", "https://api.github.com/repos/Warlord-K/world/issues/7")).unwrap();
        assert_eq!(issue.url, "https://github.com/Warlord-K/world/issues/7");
        let ci = from_api(&raw("ci_activity", "CheckSuite", "")).unwrap();
        assert_eq!(ci.number, None);
        assert_eq!(ci.url, "https://github.com/Warlord-K/world/actions");
        let mut bad = raw("comment", "PullRequest", "");
        bad["id"] = json!("12; drop");
        assert!(from_api(&bad).is_none());
        let mut bad = raw("comment", "PullRequest", "");
        bad["repository"]["full_name"] = json!("../etc");
        assert!(from_api(&bad).is_none());
    }

    #[test]
    fn payload_lowercases_repo_and_finds_ci_branches() {
        let owner = Uuid::new_v4();
        let mut ci = from_api(&raw("ci_activity", "CheckSuite", "")).unwrap();
        ci.title = "CI workflow run failed for loka/fix-bell branch".into();
        let payload = ci.payload(owner);
        assert_eq!(payload["repo"], json!("warlord-k/world"));
        assert_eq!(payload["repo_name"], json!("Warlord-K/world"));
        assert_eq!(payload["branch"], json!("loka/fix-bell"));
        assert_eq!(payload["owner_user_id"], json!(owner));
        assert!(payload.get("number").is_none());
        assert_eq!(ci_branch("Deploy workflow run cancelled for main branch").as_deref(), Some("main"));
        assert_eq!(ci_branch("Release v1"), None);
        let mut long = from_api(&raw("comment", "PullRequest", "https://api.github.com/repos/Warlord-K/world/pulls/42")).unwrap();
        long.snippet = "x".repeat(5000);
        assert_eq!(long.payload(owner)["snippet"].as_str().unwrap().chars().count(), SNIPPET_MAX);
        assert_eq!(long.payload(owner)["number"], json!(42));
    }

    async fn setup(pool: &sqlx::PgPool, tag: &str) -> Result<(Uuid, Uuid, Uuid)> {
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("gh-{tag}@test.dev"))
            .fetch_one(pool)
            .await?;
        crate::orgs::join_default_for_tests(pool, owner, None).await?;
        let mut agents = Vec::new();
        for name in ["coder", "watcher"] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $2, 'claude', 'sonnet', $3) RETURNING id",
            )
            .bind(format!("{name}-{tag}"))
            .bind(name)
            .bind(owner)
            .fetch_one(pool)
            .await?;
            agents.push(id);
        }
        Ok((owner, agents[0], agents[1]))
    }

    fn note(id: &str, reason: &str, number: i64, repo: &str, at: DateTime<Utc>) -> GithubNotification {
        GithubNotification {
            id: id.into(),
            updated_at: at,
            reason: reason.into(),
            repo: repo.into(),
            subject_type: "PullRequest".into(),
            title: "Fix the bell".into(),
            number: Some(number),
            url: format!("https://github.com/{repo}/pull/{number}"),
            actor: "ana".into(),
            snippet: "looks good".into(),
            comment_api_url: None,
        }
    }

    #[tokio::test]
    async fn github_feed_primes_dedupes_and_routes() -> Result<()> {
        let Ok(url) = std::env::var("GITHUB_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let (owner, coder, watcher) = setup(&pool, &tag).await?;
        let repo = format!("Acme-{tag}/World");
        let key = repo.to_lowercase();
        let old = Utc::now() - chrono::Duration::hours(2);

        let off = store.record_github(owner, &[note("1", "comment", 5, &repo, old)]).await?;
        assert!(!off.enabled, "nothing is stored while the feed is off");
        store.set_github_feed(owner, true).await?;
        let primed = store.record_github(owner, &[note("1", "comment", 5, &repo, old)]).await?;
        assert!(primed.primed && primed.created == 0, "the first batch only primes");
        let events = |pool: sqlx::PgPool, owner: Uuid| async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM world_events WHERE type LIKE 'github.%' AND payload->>'owner_user_id' = $1")
                .bind(owner.to_string())
                .fetch_one(&pool)
                .await
        };
        assert_eq!(events(pool.clone(), owner).await?, 0);

        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo, branch) VALUES ($3, 'Fix the bell', 'in_progress', $1, $2, 'loka/fix') RETURNING id",
        )
        .bind(coder)
        .bind(&repo)
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO task_milestones (task_id, kind, title, status, position, next_check_at) VALUES ($1, 'merge', 'Merged', 'active', 1, NOW() + INTERVAL '1 hour')")
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_tasks SET pr_url = $2, pr_number = 9 WHERE id = $1")
            .bind(task)
            .bind(format!("https://github.com/{repo}/pull/9"))
            .execute(&pool)
            .await?;
        let watches = store.watches(coder).await?;
        assert_eq!(watches.len(), 1, "publishing a PR watches it");
        assert_eq!(watches[0].task_id, Some(task));
        assert_eq!(watches[0].matcher, json!({ "repo": key, "number": 9 }));

        store.set_subscriptions(watcher, &["github.review_requested".to_string()], &[]).await?;
        store.add_watch(watcher, "github.", json!({ "repo": key, "number": 77 }), Some(7), None).await?;

        let now = Utc::now() + chrono::Duration::seconds(5);
        let batch = vec![
            note("2", "comment", 9, &repo, now),
            note("3", "review_requested", 12, &repo, now),
            note("4", "comment", 77, &repo, now),
            note("5", "comment", 13, &repo, now),
            note("1", "comment", 5, &repo, old),
        ];
        let first = store.record_github(owner, &batch).await?;
        assert_eq!(first.created, 4, "the primed item stays quiet");
        assert_eq!(first.quests, vec![task]);
        let again = store.record_github(owner, &batch).await?;
        assert_eq!(again.created, 0, "the same id and update time is never sent twice");
        let mut newer = note("2", "comment", 9, &repo, now + chrono::Duration::seconds(30));
        newer.snippet = "one more thing".into();
        assert_eq!(store.record_github(owner, &[newer]).await?.created, 1, "new activity on the same thread is new");

        let due: bool = sqlx::query_scalar("SELECT next_check_at <= NOW() FROM task_milestones WHERE task_id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert!(due, "PR activity on a quest checks its merge watch now");
        let in_room: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'github.comment'")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(in_room, 2);

        let inbox = |agent: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (String, Value)>("SELECT kind, payload FROM agent_inbox WHERE agent_id = $1 AND starts_with(kind, 'github.') ORDER BY id")
                    .bind(agent)
                    .fetch_all(&pool)
                    .await
            }
        };
        assert!(inbox(coder).await?.is_empty(), "quest watches go to the task session through the merge watch, not the main inbox");
        let got = inbox(watcher).await?;
        let kinds: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, vec!["github.review_requested", "github.comment"]);
        assert_eq!(got[1].1["watch"], json!({ "repo": key, "number": 77 }));

        let ready = sqlx::query_scalar::<_, bool>(&format!("SELECT {} FROM agent_profiles a WHERE a.id = $1", crate::world::inbox_ready_sql("a")))
            .bind(watcher)
            .fetch_one(&pool)
            .await?;
        assert!(!ready, "a GitHub burst settles before it wakes the agent");

        sqlx::query("UPDATE task_milestones SET status = 'done' WHERE task_id = $1").bind(task).execute(&pool).await?;
        assert!(store.watches(coder).await?.is_empty(), "a merged PR stops being watched");

        let removed = store.remove_watches(watcher, "github.", Some(&json!({ "repo": key, "number": 77 }))).await?;
        assert_eq!(removed, 1);
        assert_eq!(store.subscriptions(watcher).await?, vec!["github.review_requested".to_string()]);
        store.set_github_feed(owner, false).await?;
        store.set_github_feed(owner, true).await?;
        assert!(store.github_feed_settings(owner).await?.primed_at.is_none(), "turning it back on primes again");
        Ok(())
    }
}

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use loka_shared::access::Viewer;
use loka_shared::models::AgentTask;
use loka_shared::quest_questions::{self as questions, Outcome, Settle, Who};
use loka_shared::rooms::RoomMember;
use loka_shared::world::WorldAgent;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::UserId;
use crate::routes::world::{internal, quest_task};
use crate::AppState;

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, error: impl ToString) -> Refusal {
    (status, Json(json!({ "error": error.to_string() })))
}

fn code(status: StatusCode) -> Refusal {
    refuse(status, status.canonical_reason().unwrap_or("refused"))
}

fn failed(error: anyhow::Error) -> Refusal {
    code(internal(error))
}

pub(crate) async fn room_access(state: &AppState, viewer: &Viewer, task_id: Uuid) -> Result<(AgentTask, bool), StatusCode> {
    match quest_task(state, viewer, task_id).await {
        Err(StatusCode::FORBIDDEN) => {
            let task = state.db.get_task(task_id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
            if state.world.room_owner(task_id).await.map_err(internal)? == Some(viewer.user) {
                Ok((task, false))
            } else if state.world.open_rooms(&[task_id], viewer).await.map_err(internal)?.contains(&task_id) {
                Ok((task, true))
            } else {
                Err(StatusCode::FORBIDDEN)
            }
        }
        other => other.map(|task| (task, false)),
    }
}

pub(crate) async fn canonical_room_access(state: &AppState, viewer: &Viewer, task_id: Uuid) -> Result<(AgentTask, bool), StatusCode> {
    let requested = room_access(state, viewer, task_id).await?;
    let canonical = state.world.room_of(task_id).await.map_err(internal)?;
    if canonical == task_id {
        Ok(requested)
    } else {
        room_access(state, viewer, canonical).await
    }
}

async fn owner_task(state: &AppState, viewer: &Viewer, task_id: Uuid) -> Result<AgentTask, Refusal> {
    let task = canonical_room_access(state, viewer, task_id).await.map_err(code)?.0;
    let owner = state.world.room_owner(task.id).await.map_err(failed)?;
    if owner != Some(viewer.user) {
        return Err(refuse(StatusCode::FORBIDDEN, "Only the quest's owner can change its room"));
    }
    Ok(task)
}

async fn owner_question(state: &AppState, user: Uuid, id: Uuid) -> Result<(), Refusal> {
    match state.world.question_for_owner(id, user).await.map_err(failed)? {
        Some(_) => Ok(()),
        None => Err(refuse(StatusCode::NOT_FOUND, "No such question of your agents")),
    }
}

async fn name_of(state: &AppState, user: Uuid) -> String {
    state.world.user_display_name(user).await.ok().flatten().unwrap_or_else(|| "someone".into())
}

pub async fn list_members(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(task_id): Path<Uuid>,
) -> Result<Json<Value>, StatusCode> {
    let task = canonical_room_access(&state, &viewer, task_id).await?.0;
    let agents: Vec<Uuid> = state
        .world
        .list_agents_in(viewer.org)
        .await
        .map_err(internal)?
        .into_iter()
        .filter(|agent| loka_shared::access::can_use(&viewer, agent))
        .map(|agent| agent.id)
        .collect();
    let members = state.world.room_members_for(task.id, &agents).await.map_err(internal)?;
    Ok(Json(json!({ "members": members })))
}

#[derive(Deserialize)]
pub struct MemberBody {
    #[serde(default, alias = "kind")]
    member_kind: Option<String>,
    #[serde(default)]
    member_id: Option<Uuid>,
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    user_id: Option<Uuid>,
    #[serde(default)]
    role: Option<String>,
}

impl MemberBody {
    fn target(&self) -> Option<(&'static str, Uuid)> {
        let kind = match self.member_kind.as_deref() {
            Some("agent") => "agent",
            Some("person") => "person",
            Some(_) => return None,
            None if self.agent_id.is_some() => "agent",
            None if self.user_id.is_some() => "person",
            None => return None,
        };
        let id = self.member_id.or(if kind == "agent" { self.agent_id } else { self.user_id })?;
        Some((kind, id))
    }
}

pub async fn add_member(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<MemberBody>,
) -> Result<Json<RoomMember>, Refusal> {
    let task = owner_task(&state, &viewer, task_id).await?;
    let Some((kind, member)) = body.target() else {
        return Err(refuse(StatusCode::BAD_REQUEST, "Name an agent (agent_id) or a person (user_id)"));
    };
    if kind == "agent" {
        let agent = state.world.get_agent(member).await.map_err(failed)?;
        if !agent.is_some_and(|a| viewer.in_org(a.org_id)) {
            return Err(refuse(StatusCode::BAD_REQUEST, loka_shared::rooms::NOT_YOURS));
        }
    }
    match state.world.add_room_member(task.id, kind, member, body.role.as_deref(), ("person", Some(viewer.user))).await {
        Ok(member) => Ok(Json(member)),
        Err(error) if error.to_string() == "already a member" => Err(refuse(StatusCode::CONFLICT, error)),
        Err(error) => Err(refuse(StatusCode::BAD_REQUEST, error)),
    }
}

pub async fn remove_member(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path((task_id, kind, member_id)): Path<(Uuid, String, Uuid)>,
) -> Result<StatusCode, Refusal> {
    let task = owner_task(&state, &viewer, task_id).await?;
    match state.world.remove_room_member(task.id, &kind, member_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(refuse(StatusCode::NOT_FOUND, "Not a member")),
        Err(error) => Err(refuse(StatusCode::CONFLICT, error)),
    }
}

#[derive(Deserialize)]
pub struct RoomMessageBody {
    body: String,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    resolve_mentions: Option<bool>,
}

fn room_target<'a>(viewer: &Viewer, owner: Option<Uuid>, members: &[RoomMember], member_only: bool, agent: &'a WorldAgent) -> Option<&'a WorldAgent> {
    let member = members.iter().any(|member| member.member_kind == "agent" && member.member_id == agent.id);
    (!member_only || member).then_some(()).and_then(|_| questions::can_ask_room_agent(viewer, owner, agent).then_some(agent))
}

fn named_agent<'a>(key: &str, members: &[RoomMember], agents: &'a [&WorldAgent]) -> Option<&'a WorldAgent> {
    let key = key.trim().trim_start_matches('@');
    let mut matches: Vec<&WorldAgent> = agents
        .iter()
        .copied()
        .filter(|agent| agent.slug.eq_ignore_ascii_case(key) || agent.name.eq_ignore_ascii_case(key))
        .collect();
    for member in members {
        if !member.name.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(key)) {
            continue;
        }
        if member.member_kind != "agent" {
            return None;
        }
        if let Some(agent) = agents.iter().copied().find(|agent| agent.id == member.member_id) {
            matches.push(agent);
        }
    }
    matches.sort_unstable_by_key(|agent| agent.id);
    matches.dedup_by_key(|agent| agent.id);
    (matches.len() == 1).then(|| matches[0])
}

pub async fn post_room_message(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<RoomMessageBody>,
) -> Result<Json<Value>, Refusal> {
    let (task, member_only) = canonical_room_access(&state, &viewer, task_id).await.map_err(code)?;
    let room = state.world.room_asking(task.id).await.map_err(failed)?.ok_or_else(|| code(StatusCode::NOT_FOUND))?;
    let owner = state.world.room_owner(room.room).await.map_err(failed)?;
    let members = state.world.room_members(room.room).await.map_err(failed)?;
    let agents = state.world.list_agents_in(viewer.org).await.map_err(failed)?;
    let eligible: Vec<&WorldAgent> = agents
        .iter()
        .filter_map(|agent| room_target(&viewer, owner, &members, member_only, agent))
        .collect();
    let mut to: Vec<WorldAgent> = Vec::new();
    for raw in &body.to {
        let by_id = raw.parse::<Uuid>().ok().and_then(|id| eligible.iter().copied().find(|agent| agent.id == id));
        let agent = if body.resolve_mentions == Some(false) { by_id } else { by_id.or_else(|| named_agent(raw, &members, &eligible)) };
        let Some(agent) = agent
        else {
            return Err(refuse(StatusCode::BAD_REQUEST, questions::ROOM_TARGET_REFUSAL));
        };
        if !to.iter().any(|target| target.id == agent.id) {
            to.push(agent.clone());
        }
    }
    if body.resolve_mentions != Some(false) {
        for key in questions::mentions(&body.body) {
            let Some(agent) = named_agent(&key, &members, &eligible) else {
                continue;
            };
            if !to.iter().any(|target| target.id == agent.id) {
                to.push(agent.clone());
            }
        }
    }
    let name = name_of(&state, user.0).await;
    let post = state.world.post_room_message(&room, &viewer, &name, &body.body, &to).await.map_err(|e| refuse(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "message": post.message_id, "asked": post.asked })))
}

#[derive(Deserialize)]
pub struct QuestionQuery {
    #[serde(default)]
    status: Option<String>,
}

pub async fn list_questions(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(task_id): Path<Uuid>,
    Query(q): Query<QuestionQuery>,
) -> Result<Json<Value>, StatusCode> {
    let task = canonical_room_access(&state, &viewer, task_id).await?.0;
    let open_only = q.status.as_deref() == Some("open");
    let list = state.world.room_questions_for(task.id, open_only, &viewer).await.map_err(internal)?;
    let decisions = state.world.decision_summary_for(task.id, &viewer).await.map_err(internal)?;
    Ok(Json(json!({ "questions": list, "decisions": decisions })))
}

#[derive(Deserialize)]
pub struct AnswerBody {
    answer: String,
    #[serde(default)]
    refs: Vec<String>,
}

pub async fn answer_question(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<AnswerBody>,
) -> Result<Json<Value>, Refusal> {
    owner_question(&state, user.0, id).await?;
    let text = body.answer.trim();
    if text.is_empty() {
        return Err(refuse(StatusCode::BAD_REQUEST, "Write an answer"));
    }
    let name = name_of(&state, user.0).await;
    let settle = Settle { refs: body.refs, ..Settle::new(Outcome::Answered, text, Who::person(user.0, &name)) };
    match state.world.settle_question(id, settle).await.map_err(failed)? {
        Some(settled) => Ok(Json(json!(settled))),
        None => Err(refuse(StatusCode::CONFLICT, "That question is already settled")),
    }
}

#[derive(Deserialize)]
pub struct PassUpBody {
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    suggestion: Option<String>,
}

pub async fn pass_up(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<PassUpBody>,
) -> Result<Json<Value>, Refusal> {
    owner_question(&state, user.0, id).await?;
    let name = name_of(&state, user.0).await;
    let why = body.why.map(|w| w.trim().to_ascii_lowercase()).filter(|w| questions::PERSON_KINDS.contains(&w.as_str())).unwrap_or_else(|| "other".into());
    let settle = Settle { why: Some(why), ..Settle::new(Outcome::PassedUp, body.suggestion.as_deref().unwrap_or(""), Who::person(user.0, &name)) };
    match state.world.settle_question(id, settle).await.map_err(failed)? {
        Some(settled) => Ok(Json(json!(settled))),
        None => Err(refuse(StatusCode::CONFLICT, "That question is already settled")),
    }
}

pub async fn park(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Refusal> {
    owner_question(&state, user.0, id).await?;
    let name = name_of(&state, user.0).await;
    match state.world.park_question(id, user.0, &name).await.map_err(failed)? {
        Some((todo, settled)) => Ok(Json(json!({ "todo_id": todo, "settled": settled }))),
        None => Err(refuse(StatusCode::CONFLICT, "That question is already settled")),
    }
}

pub async fn crew_preview(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, StatusCode> {
    let agent = state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let defaults = state.world.crew_preview(&agent).await.map_err(internal)?;
    Ok(Json(json!({ "defaults": defaults })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{middleware, routing::get, Router};

    async fn state(url: &str) -> anyhow::Result<AppState> {
        let db = loka_shared::db::Database::connect(url).await?;
        let pool = db.pool().clone();
        Ok(AppState {
            db,
            config: crate::config::ApiConfig {
                repos: vec![],
                backends: indexmap::IndexMap::new(),
                skills: vec![],
                mcp_servers: vec![],
            },
            world: loka_shared::world::WorldStore::new(pool.clone()),
            runners: loka_shared::runners::RunnerStore::new(pool),
            world_tx: tokio::sync::broadcast::channel(8).0,
            signal_tx: tokio::sync::broadcast::channel(8).0,
            runner_tx: crate::runner_wakes::RunnerWakes::default(),
            term: crate::terminal::TermHub::new(),
        })
    }

    async fn room_response(state: &AppState, user: Uuid, task: Uuid) -> anyhow::Result<(StatusCode, Value)> {
        let app = Router::new()
            .route("/world/tasks/{id}/room", get(crate::routes::world::quest_room))
            .layer(middleware::from_fn_with_state(state.clone(), crate::access::role_middleware))
            .layer(Extension(UserId(user)))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let serving = tokio::spawn(async move { axum::serve(listener, app).await });
        let result: anyhow::Result<(StatusCode, String)> = async {
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?;
            let response = client.get(format!("http://{address}/world/tasks/{task}/room")).send().await?;
            let status = response.status();
            let text = response.text().await?;
            Ok((status, text))
        }
        .await;
        serving.abort();
        let (status, text) = result?;
        let body = if text.is_empty() { Value::Null } else { serde_json::from_str(&text)? };
        Ok((status, body))
    }

    #[tokio::test]
    async fn part_room_keeps_exact_task_metadata_and_requires_part_access() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("ROOMS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let state = state(&url).await?;
        let pool = state.db.pool();
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Part owner') RETURNING id")
            .bind(format!("owner-{tag}@rooms.test"))
            .fetch_one(pool)
            .await?;
        let invited: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Room guest') RETURNING id")
            .bind(format!("guest-{tag}@rooms.test"))
            .fetch_one(pool)
            .await?;
        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name, created_by) VALUES ($1, 'Room route test', $2) RETURNING id")
            .bind(format!("room-{tag}"))
            .bind(owner)
            .fetch_one(pool)
            .await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'owner'), ($1, $3, 'member')")
            .bind(org)
            .bind(owner)
            .bind(invited)
            .execute(pool)
            .await?;
        sqlx::query("UPDATE users SET active_org_id = $1 WHERE id = ANY($2)")
            .bind(org)
            .bind(vec![owner, invited])
            .execute(pool)
            .await?;
        let parent_agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id, asking)
             VALUES ($1, 'Parent lead', 'claude', 'opus', $2, $3, 'crew') RETURNING id",
        )
        .bind(format!("parent-{tag}"))
        .bind(owner)
        .bind(org)
        .fetch_one(pool)
        .await?;
        let part_agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id)
             VALUES ($1, 'Part lead', 'codex', 'gpt-5.5', $2, $3) RETURNING id",
        )
        .bind(format!("part-{tag}"))
        .bind(owner)
        .bind(org)
        .fetch_one(pool)
        .await?;
        let parent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, org_id, title, description, status, assigned_agent_id, asking, crew_size, plan_first)
             VALUES ($1, $2, 'Parent quest', 'parent description', 'review', $3, 'crew', 3, TRUE) RETURNING id",
        )
        .bind(owner)
        .bind(org)
        .bind(parent_agent)
        .fetch_one(pool)
        .await?;
        let variants = json!([{ "model": "opus" }, { "cli": "codex", "model": "gpt-5.5" }]);
        let crew_variants = json!([{}, { "cli": "codex", "model": "gpt-5.5" }]);
        let part: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks
               (user_id, org_id, title, description, status, assigned_agent_id, parent_task_id,
                plan_first, attempts, attempt_variants, ci_feedback, pace, asking, crew_size, crew_variants)
             VALUES ($1, $2, 'Exact part', 'part description', 'failed', $3, $4,
                     FALSE, 2, $5, TRUE, 'urgent', 'ask_agent', 2, $6) RETURNING id",
        )
        .bind(owner)
        .bind(org)
        .bind(part_agent)
        .bind(parent)
        .bind(&variants)
        .bind(&crew_variants)
        .fetch_one(pool)
        .await?;
        sqlx::query(
            "INSERT INTO task_milestones (task_id, position, title, status)
             VALUES ($1, 0, 'Parent milestone', 'active'), ($2, 0, 'Part milestone', 'pending')",
        )
        .bind(parent)
        .bind(part)
        .execute(pool)
        .await?;
        let parent_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id)
             VALUES ($1, 'task', 'claude', 'opus', $2) RETURNING id",
        )
        .bind(parent_agent)
        .bind(parent)
        .fetch_one(pool)
        .await?;
        let part_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id)
             VALUES ($1, 'task', 'codex', 'gpt-5.5', $2) RETURNING id",
        )
        .bind(part_agent)
        .bind(part)
        .fetch_one(pool)
        .await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'task', 'queued', 'parent work'),
                    ($4, $5, $6, 'task', 'queued', 'part work')",
        )
        .bind(parent_session)
        .bind(parent_agent)
        .bind(parent)
        .bind(part_session)
        .bind(part_agent)
        .bind(part)
        .execute(pool)
        .await?;

        let (status, parent_body) = room_response(&state, owner, parent).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(parent_body["task"]["id"], json!(parent));
        assert_eq!(parent_body["task"]["title"], "Parent quest");
        assert_eq!(parent_body["task"]["assigned_agent_id"], json!(parent_agent));
        assert_eq!(parent_body["task"]["status"], "review");
        assert_eq!(parent_body["task"]["milestones"][0]["title"], "Parent milestone");
        assert_eq!(parent_body["task"]["parent"], Value::Null);
        assert_eq!(parent_body["task"]["plan_first"], true);
        assert_eq!(parent_body["room"]["task_id"], json!(parent));

        let (status, owner_body) = room_response(&state, owner, part).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(owner_body["task"]["id"], json!(part));
        assert_eq!(owner_body["task"]["title"], "Exact part");
        assert_eq!(owner_body["task"]["assigned_agent_id"], json!(part_agent));
        assert_eq!(owner_body["task"]["status"], "failed");
        assert_eq!(owner_body["task"]["milestones"][0]["title"], "Part milestone");
        assert_eq!(owner_body["task"]["parent"]["id"], json!(parent));
        assert_eq!(owner_body["task"]["parts"], json!([]));
        assert_eq!(owner_body["task"]["plan_first"], false);
        assert_eq!(owner_body["task"]["attempts"], 2);
        assert_eq!(owner_body["task"]["attempt_variants"], variants);
        assert_eq!(owner_body["task"]["ci_feedback"], true);
        assert_eq!(owner_body["task"]["pace"], "urgent");
        assert_eq!(owner_body["task"]["asking"], "ask_agent");
        assert_eq!(owner_body["task"]["crew_size"], 2);
        assert_eq!(owner_body["task"]["crew_variants"], crew_variants);
        assert_eq!(owner_body["room"]["task_id"], json!(parent));
        let reason = owner_body["task"]["resume"]["reason"].as_str().unwrap_or_default();
        assert!(reason.contains(&part_session.to_string()));
        assert!(!reason.contains(&parent_session.to_string()));

        sqlx::query(
            "INSERT INTO quest_room_members (quest_id, member_kind, member_id, role, added_by, added_by_kind)
             VALUES ($1, 'person', $2, 'guest', $3, 'person')",
        )
        .bind(parent)
        .bind(invited)
        .bind(owner)
        .execute(pool)
        .await?;
        let (status, _) = room_response(&state, invited, part).await?;
        assert_eq!(status, StatusCode::FORBIDDEN);

        sqlx::query(
            "INSERT INTO quest_room_members (quest_id, member_kind, member_id, role, added_by, added_by_kind)
             VALUES ($1, 'person', $2, 'guest', $3, 'person')",
        )
        .bind(part)
        .bind(invited)
        .bind(owner)
        .execute(pool)
        .await?;
        let (status, guest_body) = room_response(&state, invited, part).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(guest_body["task"]["id"], json!(part));
        assert_eq!(guest_body["task"]["resume"], Value::Null);
        assert_eq!(guest_body["room"]["task_id"], json!(parent));
        assert_eq!(guest_body["room"]["sessions"], json!([]));
        assert_eq!(guest_body["room"]["runs"], json!([]));
        assert_eq!(guest_body["room"]["events"], json!([]));

        sqlx::query("DELETE FROM orgs WHERE id = $1").bind(org).execute(pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![owner, invited]).execute(pool).await?;
        Ok(())
    }
}

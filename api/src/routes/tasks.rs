use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use loka_shared::models::{TaskKind, TaskOutputTarget};

use crate::auth::UserId;
use loka_shared::access::Role;
use loka_shared::world::{QuestAssignee, QuestEdit, QUEST_STATUSES};
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateTask {
    title: String,
    description: Option<String>,
    repo: Option<String>,
    priority: Option<String>,
    cli: Option<String>,
    model: Option<String>,
    kind: Option<String>,
    output_target: Option<String>,
    assignee_user_id: Option<Uuid>,
    assigned_agent_id: Option<Uuid>,
    planner_agent_id: Option<Uuid>,
    implementer_agent_id: Option<Uuid>,
    reviewer_agent_id: Option<Uuid>,
    external_source: Option<String>,
    external_id: Option<String>,
    external_url: Option<String>,
    #[serde(default)]
    milestones: Vec<loka_shared::milestones::NewMilestone>,
    #[serde(default)]
    wait_for_merge: bool,
    #[serde(default)]
    time_limit_minutes: Option<i64>,
    #[serde(default)]
    team_id: Option<Uuid>,
    #[serde(default, flatten)]
    quest: loka_shared::quests::QuestChoice,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<Vec<loka_shared::models::AgentTask>>, StatusCode> {
    let tasks = state
        .db
        .list_tasks_for_user(user.0)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(tasks))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<loka_shared::access::Viewer>,
    Json(body): Json<CreateTask>,
) -> Result<(StatusCode, Json<loka_shared::models::AgentTask>), StatusCode> {
    let title = body.title.trim();
    let limits = loka_shared::world::WorldStore::MIN_TIME_LIMIT_MINUTES..=loka_shared::world::WorldStore::MAX_TIME_LIMIT_MINUTES;
    if title.is_empty() || body.time_limit_minutes.is_some_and(|m| !limits.contains(&m)) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let quest = body.quest.validate().map_err(|_| StatusCode::BAD_REQUEST)?;
    let team = match body.team_id {
        Some(id) => Some(
            super::teams::owned_team(&state, user.0, viewer.role, id)
                .await
                .map_err(|(code, _)| if code == StatusCode::NOT_FOUND { StatusCode::BAD_REQUEST } else { code })?,
        ),
        None => None,
    };
    if let Some(team) = &team {
        let agents: Vec<Uuid> = state
            .world
            .team_members(team.id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .into_iter()
            .filter(|m| m.member_kind == "agent")
            .map(|m| m.member_id)
            .collect();
        if agents.is_empty() || body.assigned_agent_id.is_some_and(|a| !agents.contains(&a)) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    if let Some(agent_id) = body.assigned_agent_id {
        let agent = state
            .world
            .get_agent(agent_id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::BAD_REQUEST)?;
        if !loka_shared::access::can_manage(&viewer, &agent) {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    if let Some(person) = body.assignee_user_id {
        if !state.world.is_org_member(viewer.org(), person).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let kind = body
        .kind
        .as_deref()
        .unwrap_or(TaskKind::Code.as_str())
        .parse::<TaskKind>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let output_target = body
        .output_target
        .as_deref()
        .unwrap_or(TaskOutputTarget::None.as_str())
        .parse::<TaskOutputTarget>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let task = state
        .db
        .create_task_full(
            user.0,
            title,
            body.description.as_deref(),
            body.repo.as_deref(),
            body.priority.as_deref().unwrap_or("medium"),
            body.cli.as_deref(),
            body.model.as_deref(),
            body.assignee_user_id,
            if team.is_some() { None } else { body.assigned_agent_id },
            body.external_source.as_deref(),
            body.external_id.as_deref(),
            body.external_url.as_deref(),
            body.planner_agent_id,
            body.implementer_agent_id,
            body.reviewer_agent_id,
            Some(kind.as_str()),
            Some(output_target.as_str()),
            None,
            None,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut task = task;
    if task.org_id != viewer.org && task.assigned_agent_id.is_none() {
        sqlx::query("UPDATE agent_tasks SET org_id = $2 WHERE id = $1 AND assigned_agent_id IS NULL")
            .bind(task.id)
            .bind(viewer.org)
            .execute(state.world.pool())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        task.org_id = viewer.org;
    }

    if let Some(minutes) = body.time_limit_minutes {
        state
            .world
            .set_task_time_limit(task.id, Some(minutes))
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    }
    state.world.set_quest_options(task.id, &quest).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    if let Some(team) = &team {
        state
            .world
            .assign_quest_to_team(task.id, team, body.assigned_agent_id)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    }
    let mut milestones = body.milestones;
    if body.wait_for_merge && body.repo.is_some() {
        milestones.push(loka_shared::milestones::NewMilestone {
            title: loka_shared::milestones::merge_title().to_string(),
            kind: "merge".to_string(),
        });
    }
    if !milestones.is_empty() {
        loka_shared::milestones::MilestoneStore::new(state.world.pool().clone())
            .add(task.id, &milestones)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    }

    Ok((StatusCode::CREATED, Json(task)))
}

const QUEST_KEYS: [&str; 8] = ["status", "assigned_agent_id", "assignee_user_id", "time_limit_minutes", "plan_first", "attempts", "attempt_variants", "ci_feedback"];

fn uuid_field(body: &std::collections::HashMap<String, Value>, key: &str) -> Result<Option<Option<Uuid>>, StatusCode> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) if s.is_empty() => Ok(Some(None)),
        Some(Value::String(s)) => s.parse().map(|id| Some(Some(id))).map_err(|_| StatusCode::BAD_REQUEST),
        Some(_) => Err(StatusCode::BAD_REQUEST),
    }
}

async fn can_manage_agent(state: &AppState, viewer: &loka_shared::access::Viewer, agent_id: Uuid) -> Result<bool, StatusCode> {
    let agent = state.world.get_agent(agent_id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(agent.is_some_and(|a| loka_shared::access::can_manage(viewer, &a)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(mut body): Json<std::collections::HashMap<String, Value>>,
) -> Result<Json<Value>, StatusCode> {
    let internal = |e: anyhow::Error| {
        tracing::error!(error = %e, "quest update failed");
        StatusCode::INTERNAL_SERVER_ERROR
    };
    let task = state.db.get_task(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let viewer = crate::access::viewer_for(&state, user.0).await?;
    if !viewer.in_org(task.org_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let legacy = task.org_id == Some(loka_shared::orgs::DEFAULT_ORG) && task.assigned_agent_id.is_none() && task.assignee_user_id.is_none();
    let owner = task.user_id == user.0 || (legacy && state.db.is_shared_workspace());
    let world = match task.assigned_agent_id {
        _ if role == Role::Admin => true,
        Some(agent) => can_manage_agent(&state, &viewer, agent).await?,
        None => task.assignee_user_id == Some(user.0),
    };
    if !world && task.user_id != user.0 && (!owner || task.assigned_agent_id.is_some()) {
        return Err(StatusCode::NOT_FOUND);
    }
    if !owner && body.keys().any(|k| !QUEST_KEYS.contains(&k.as_str())) {
        return Err(StatusCode::FORBIDDEN);
    }

    let mut edit = QuestEdit::default();
    if let Some(status) = body.remove("status") {
        let status = status.as_str().ok_or(StatusCode::BAD_REQUEST)?.to_string();
        if !QUEST_STATUSES.contains(&status.as_str()) {
            return Err(StatusCode::BAD_REQUEST);
        }
        edit.status = Some(status);
    }
    let agent = uuid_field(&body, "assigned_agent_id")?;
    let person = uuid_field(&body, "assignee_user_id")?;
    body.remove("assigned_agent_id");
    body.remove("assignee_user_id");
    edit.assignee = match (agent, person) {
        (Some(Some(a)), _) => Some(QuestAssignee::Agent(a)),
        (_, Some(Some(p))) => Some(QuestAssignee::Person(p)),
        (Some(None), _) | (_, Some(None)) => Some(QuestAssignee::Nobody),
        (None, None) => None,
    };
    match edit.assignee {
        Some(QuestAssignee::Agent(a)) if a != task.assigned_agent_id.unwrap_or_default() => {
            if !can_manage_agent(&state, &viewer, a).await? {
                return Err(StatusCode::FORBIDDEN);
            }
        }
        Some(QuestAssignee::Person(p)) => {
            state.world.user_display_name(p).await.map_err(internal)?.ok_or(StatusCode::BAD_REQUEST)?;
        }
        _ => {}
    }

    let time_limit = if let Some(limit) = body.remove("time_limit_minutes") {
        let minutes = match limit {
            Value::Null => None,
            value => Some(value.as_i64().ok_or(StatusCode::BAD_REQUEST)?),
        };
        Some(minutes)
    } else {
        None
    };
    let options = ["plan_first", "attempts", "attempt_variants", "ci_feedback", "pace", "context"];
    let quest = if options.iter().any(|k| body.contains_key(*k)) {
        let args = Value::Object(options.iter().filter_map(|k| body.remove(*k).map(|v| (k.to_string(), v))).collect());
        Some(loka_shared::quests::QuestChoice::from_args(&args)
            .and_then(|c| c.validate())
            .map_err(|_| StatusCode::BAD_REQUEST)?)
    } else {
        None
    };
    let quest_settings = quest.as_ref().filter(|choice| !choice.is_empty());
    if body.is_empty() && (time_limit.is_some() || quest_settings.is_some()) {
        state.world.set_task_settings(id, time_limit, quest_settings).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    } else if !body.is_empty() {
        let rows = match state.db.update_task_with_settings(id, user.0, &body, time_limit, quest_settings).await {
            Ok(rows) => rows,
            Err(error) => match error.downcast_ref::<loka_shared::db::UpdateTaskError>() {
                Some(loka_shared::db::UpdateTaskError::InvalidPullRequest | loka_shared::db::UpdateTaskError::InvalidSettings) => {
                    return Err(StatusCode::BAD_REQUEST)
                }
                Some(loka_shared::db::UpdateTaskError::PullRequestConflict) => return Err(StatusCode::CONFLICT),
                None => return Err(internal(error)),
            },
        };
        if rows == 0 {
            return Err(StatusCode::NOT_FOUND);
        }
    }
    if edit.status.is_none() && edit.assignee.is_none() {
        return Ok(Json(json!({ "ok": true })));
    }

    let name = state.world.user_display_name(user.0).await.map_err(internal)?.unwrap_or_else(|| "someone".into());
    let (status, running) = if edit.assignee.is_none() && edit.status.as_deref() == Some("cancelled") {
        let cancelled = state.world.cancel_task(id, &name, None).await.map_err(internal)?;
        ("cancelled".to_string(), cancelled.running)
    } else {
        let edited = state.world.edit_quest(id, &edit, user.0, &name).await.map_err(internal)?;
        (edited.status, edited.running)
    };
    let coordinator = loka_agent::runner_server::RunnerCoordinator::new(state.db.clone(), state.world.clone());
    for run in &running {
        if let Err(e) = coordinator.stop_run(*run, &name).await {
            warn!(run = %run, error = %e, "stopping quest run failed");
        }
    }
    Ok(Json(json!({ "ok": true, "status": status, "stopping_runs": running.len() })))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, StatusCode> {
    let rows = state
        .db
        .delete_task(id, user.0)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(json!({ "ok": true })))
}

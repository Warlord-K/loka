use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::{DateTime, Utc};
use loka_shared::access::Role;
use loka_shared::todos::{clean_proposals, NewTodo, Proposal, Todo, TodoActor, TodoPatch};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::error;
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "todo route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

#[derive(Deserialize)]
pub struct ListQuery {
    done_days: Option<i64>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Todo>>, StatusCode> {
    let days = q.done_days.unwrap_or(7).clamp(0, 90);
    Ok(Json(state.world.todos_for(user.0, days).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct CreateTodo {
    text: String,
    priority: Option<i16>,
    due_at: Option<DateTime<Utc>>,
    nudge_at: Option<DateTime<Utc>>,
    repeat_nudge: Option<bool>,
    source: Option<Value>,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<CreateTodo>,
) -> Result<Json<Todo>, StatusCode> {
    if body.text.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    state
        .world
        .add_todo(NewTodo {
            owner_user_id: user.0,
            text: body.text,
            priority: body.priority,
            added_by_user: Some(user.0),
            source: body.source,
            due_at: body.due_at,
            nudge_at: body.nudge_at,
            repeat_nudge: body.repeat_nudge,
            ..Default::default()
        })
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[derive(Deserialize)]
pub struct UpdateTodo {
    status: Option<String>,
    #[serde(flatten)]
    patch: TodoPatch,
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateTodo>,
) -> Result<Json<Todo>, StatusCode> {
    let todo = state.world.get_todo(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if todo.owner_user_id != user.0 {
        return Err(StatusCode::NOT_FOUND);
    }
    if body.status.as_deref().is_some_and(|s| !matches!(s, "open" | "done" | "dismissed")) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let by = TodoActor { agent: None, user: Some(user.0) };
    let p = &body.patch;
    let mut current = todo;
    if p.text.is_some() || p.priority.is_some() || p.due_at.is_some() || p.nudge_at.is_some() || p.repeat_nudge.is_some() || p.started.is_some() {
        current = state.world.update_todo(id, p, by).await.map_err(|_| StatusCode::BAD_REQUEST)?.ok_or(StatusCode::NOT_FOUND)?;
    }
    if let Some(status) = body.status.as_deref() {
        current = state.world.set_todo_status(id, status, by).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    }
    Ok(Json(current))
}

#[derive(Deserialize)]
pub struct Pick {
    index: usize,
    proposal: Option<Proposal>,
}

#[derive(Deserialize)]
pub struct Decide {
    #[serde(default)]
    index: usize,
    proposal: Option<Proposal>,
    picks: Option<Vec<Pick>>,
}

pub async fn decide(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<loka_shared::access::Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<Decide>,
) -> Result<Json<Todo>, StatusCode> {
    if role < Role::Write {
        return Err(StatusCode::FORBIDDEN);
    }
    let todo = state.world.get_todo(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if todo.owner_user_id != user.0 {
        return Err(StatusCode::NOT_FOUND);
    }
    if todo.decision.is_some() || todo.status != "open" {
        return Err(StatusCode::CONFLICT);
    }
    let proposals = clean_proposals(&todo.proposals);
    let mut picks = body.picks.unwrap_or_else(|| vec![Pick { index: body.index, proposal: body.proposal }]);
    picks.sort_by_key(|p| p.index);
    picks.dedup_by_key(|p| p.index);
    if picks.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut chosen = Vec::with_capacity(picks.len());
    for pick in picks {
        let original = proposals.get(pick.index).ok_or(StatusCode::BAD_REQUEST)?.clone();
        let made = match pick.proposal {
            Some(p) => Proposal { kind: original.kind.clone(), ..p }.cleaned().ok_or(StatusCode::BAD_REQUEST)?,
            None => original.clone(),
        };
        chosen.push((pick.index, made, original));
    }
    let adder = match todo.added_by_agent {
        Some(a) => state.world.get_agent(a).await.map_err(internal)?.filter(|a| a.owner_user_id == Some(user.0)).map(|a| a.id),
        None => None,
    };
    let agent = match adder {
        Some(a) => a,
        None => state.world.main_agent_for(user.0).await.map_err(internal)?.map(|a| a.id).ok_or(StatusCode::CONFLICT)?,
    };
    let edited = chosen.iter().any(|(_, made, original)| made != original);
    let label = chosen.iter().map(|(_, made, _)| made.label.as_str()).collect::<Vec<_>>().join(" + ");
    let picked: Vec<Value> = chosen
        .iter()
        .map(|(index, made, original)| json!({ "index": index, "kind": made.kind, "label": made.label, "edited": made != original }))
        .collect();
    let decision = json!({ "index": chosen[0].0, "kind": chosen[0].1.kind, "label": label, "edited": edited, "agent_id": agent, "at": Utc::now(), "picked": picked });
    let decided = state.world.decide_todo(id, user.0, decision).await.map_err(internal)?.ok_or(StatusCode::CONFLICT)?;
    let name = todo.text.replace(['[', ']'], "");
    let text = if chosen.len() == 1 {
        let (_, made, original) = &chosen[0];
        format!(
            "For my to-do [{}](todo:{}), go ahead with \"{}\". {}\n\nStrike the to-do off with complete_todo once it is done.",
            name,
            todo.short_id(),
            made.label,
            made.instruction(original)
        )
    } else {
        let steps = chosen
            .iter()
            .enumerate()
            .map(|(n, (_, made, original))| format!("{}. \"{}\". {}", n + 1, made.label, made.instruction(original)))
            .collect::<Vec<_>>()
            .join("\n\n");
        let skipped = proposals.len() - chosen.len();
        let skip = if skipped > 0 { format!(" Skip the other {skipped} suggestion{}.", if skipped == 1 { "" } else { "s" }) } else { String::new() };
        format!(
            "For my to-do [{}](todo:{}), go ahead with {} of these, each on its own (separate quests where they are quests).{}\n\n{}\n\nStrike the to-do off with complete_todo once all of them are done.",
            name,
            todo.short_id(),
            if chosen.len() == 2 { "both".to_string() } else { format!("all {}", chosen.len()) },
            skip,
            steps
        )
    };
    let message = serde_json::from_value(json!({ "body": text })).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Err(status) = crate::routes::world::post_message(State(state.clone()), Extension(user), Extension(role), Extension(viewer), Path(agent), Json(message)).await {
        state.world.clear_todo_decision(id).await.map_err(internal)?;
        return Err(status);
    }
    Ok(Json(decided))
}

pub async fn clear_proposals(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Todo>, StatusCode> {
    let todo = state.world.get_todo(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if todo.owner_user_id != user.0 {
        return Err(StatusCode::NOT_FOUND);
    }
    let by = TodoActor { agent: None, user: Some(user.0) };
    Ok(Json(state.world.set_todo_proposals(id, &[], by).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?))
}

#[derive(Deserialize)]
pub struct Order {
    ids: Vec<Uuid>,
}

pub async fn reorder(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<Order>,
) -> Result<StatusCode, StatusCode> {
    if body.ids.len() > 500 {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.world.reorder_todos(user.0, &body.ids).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

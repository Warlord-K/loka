use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use loka_shared::access::Role;
use loka_shared::canvas::{self, Canvas, Mark, NewMark};
use loka_shared::reports::{self, NewReport, Report, ReportActor, ReportBrief, ReportFilter, ReportPatch, Revision, RevisionBrief, StaleReport};
use serde::{Deserialize, Serialize};
use tracing::error;
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "report route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn invalid(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

fn status(code: StatusCode) -> (StatusCode, String) {
    (code, String::new())
}

async fn org_of(state: &AppState, user: Uuid) -> Result<Uuid, StatusCode> {
    crate::access::viewer_for(state, user).await?.org.ok_or(StatusCode::FORBIDDEN)
}

async fn readable(state: &AppState, user: Uuid, role: Role, id: Uuid) -> Result<Report, StatusCode> {
    let org = org_of(state, user).await?;
    let report = state.world.get_report(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if !reports::can_read(&report, user, role, org) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(report)
}

async fn editable(state: &AppState, user: Uuid, role: Role, id: Uuid) -> Result<Report, StatusCode> {
    let report = readable(state, user, role, id).await?;
    if role < Role::Write || !reports::can_edit(&report, user, role, report.org_id.unwrap_or_default()) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(report)
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(filter): Query<ReportFilter>,
) -> Result<Json<Vec<ReportBrief>>, (StatusCode, String)> {
    let org = org_of(&state, user.0).await.map_err(status)?;
    state.world.list_reports(user.0, org, &filter).await.map(Json).map_err(invalid)
}

#[derive(Deserialize)]
pub struct FindQuery {
    key: String,
}

pub async fn find(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(q): Query<FindQuery>,
) -> Result<Json<ReportBrief>, StatusCode> {
    let org = org_of(&state, user.0).await?;
    state.world.find_report_brief(user.0, org, &q.key).await.map_err(internal)?.map(Json).ok_or(StatusCode::NOT_FOUND)
}

#[derive(Deserialize)]
pub struct CreateReport {
    title: String,
    #[serde(default)]
    body: String,
    folder: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    visibility: Option<String>,
    #[serde(default)]
    pinned: bool,
    canvas: Option<serde_json::Value>,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<CreateReport>,
) -> Result<Json<Report>, (StatusCode, String)> {
    let new = NewReport {
        owner_user_id: user.0,
        by: ReportActor::User(user.0),
        title: body.title,
        body: body.body,
        folder: body.folder,
        tags: body.tags,
        visibility: body.visibility,
        pinned: body.pinned,
        task_id: None,
    };
    match body.canvas {
        Some(value) => {
            let canvas = Canvas::from_raw(&value, "person").map_err(invalid)?;
            state.world.create_canvas(new, canvas).await.map(Json).map_err(invalid)
        }
        None => state.world.create_report(new).await.map(Json).map_err(invalid),
    }
}

#[derive(Serialize)]
pub struct ReportDetail {
    report: Report,
    backlinks: Vec<ReportBrief>,
    outlinks: Vec<ReportBrief>,
    can_edit: bool,
    marks: Vec<Mark>,
    lesson: Option<Uuid>,
}

pub async fn get(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<Json<ReportDetail>, StatusCode> {
    let report = readable(&state, user.0, role, id).await?;
    let org = report.org_id.unwrap_or_default();
    let backlinks = state.world.report_backlinks(user.0, org, id).await.map_err(internal)?;
    let outlinks = state.world.report_outlinks(user.0, org, &report).await.map_err(internal)?;
    let can_edit = role >= Role::Write && reports::can_edit(&report, user.0, role, org);
    let marks = if report.kind == "canvas" { state.world.canvas_marks(id).await.map_err(internal)? } else { Vec::new() };
    let lesson = state.world.live_lesson(&report).await.map_err(internal)?;
    Ok(Json(ReportDetail { report, backlinks, outlinks, can_edit, marks, lesson }))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(patch): Json<ReportPatch>,
) -> Result<Json<Report>, Response> {
    editable(&state, user.0, role, id).await.map_err(|code| code.into_response())?;
    match state.world.update_report(id, ReportActor::User(user.0), &patch).await {
        Ok(Some(report)) => Ok(Json(report)),
        Ok(None) => Err(StatusCode::NOT_FOUND.into_response()),
        Err(e) => Err(match e.downcast::<StaleReport>() {
            Ok(StaleReport(current)) => (StatusCode::CONFLICT, Json(serde_json::json!({ "error": reports::STALE_REPORT, "report": current }))).into_response(),
            Err(e) => invalid(e).into_response(),
        }),
    }
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    editable(&state, user.0, role, id).await?;
    state.world.delete_report(id, ReportActor::User(user.0)).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revisions(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<RevisionBrief>>, StatusCode> {
    readable(&state, user.0, role, id).await?;
    state.world.report_revisions(id).await.map(Json).map_err(internal)
}

pub async fn revision(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path((id, revision)): Path<(Uuid, i32)>,
) -> Result<Json<Revision>, StatusCode> {
    readable(&state, user.0, role, id).await?;
    state.world.report_revision(id, revision).await.map_err(internal)?.map(Json).ok_or(StatusCode::NOT_FOUND)
}

pub async fn restore(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path((id, revision)): Path<(Uuid, i32)>,
) -> Result<Json<Report>, (StatusCode, String)> {
    editable(&state, user.0, role, id).await.map_err(status)?;
    state
        .world
        .restore_report_revision(id, revision, ReportActor::User(user.0))
        .await
        .map_err(invalid)?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, String::new()))
}

#[derive(Deserialize)]
pub struct CanvasChange {
    ops: serde_json::Value,
}

pub async fn change_canvas(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(body): Json<CanvasChange>,
) -> Result<Json<Report>, (StatusCode, String)> {
    editable(&state, user.0, role, id).await.map_err(status)?;
    let ops = canvas::parse_ops(&body.ops).map_err(invalid)?;
    state
        .world
        .change_canvas(id, ReportActor::User(user.0), ops)
        .await
        .map_err(invalid)?
        .map(|(report, _)| Json(report))
        .ok_or((StatusCode::NOT_FOUND, String::new()))
}

pub async fn add_mark(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(mark): Json<NewMark>,
) -> Result<Json<Mark>, (StatusCode, String)> {
    let report = editable(&state, user.0, role, id).await.map_err(status)?;
    state.world.add_canvas_mark(&report, user.0, mark).await.map(Json).map_err(invalid)
}

#[derive(Deserialize)]
pub struct MarkChange {
    resolved: bool,
    reply: Option<String>,
}

pub async fn change_mark(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path((id, mark)): Path<(Uuid, Uuid)>,
    Json(body): Json<MarkChange>,
) -> Result<Json<Mark>, (StatusCode, String)> {
    let report = editable(&state, user.0, role, id).await.map_err(status)?;
    state
        .world
        .resolve_canvas_mark(&report, mark, ReportActor::User(user.0), body.reply, !body.resolved)
        .await
        .map_err(invalid)?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, String::new()))
}

pub async fn delete_mark(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path((id, mark)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let report = editable(&state, user.0, role, id).await.map_err(status)?;
    match state.world.remove_canvas_mark(&report, mark, ReportActor::User(user.0)).await.map_err(invalid)? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err((StatusCode::NOT_FOUND, String::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn state(url: &str) -> anyhow::Result<AppState> {
        let db = loka_shared::db::Database::connect(url).await?;
        let pool = db.pool().clone();
        Ok(AppState {
            db,
            config: crate::config::ApiConfig { repos: vec![], backends: indexmap::IndexMap::new(), skills: vec![], mcp_servers: vec![] },
            world: loka_shared::world::WorldStore::new(pool.clone()),
            runners: loka_shared::runners::RunnerStore::new(pool),
            world_tx: tokio::sync::broadcast::channel(8).0,
            runner_tx: crate::runner_wakes::RunnerWakes::default(),
            term: crate::terminal::TermHub::new(),
        })
    }

    async fn member(state: &AppState, email: String) -> anyhow::Result<Uuid> {
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Mark tester') RETURNING id").bind(email).fetch_one(state.db.pool()).await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT (org_id, user_id) DO NOTHING")
            .bind(loka_shared::orgs::DEFAULT_ORG)
            .bind(user)
            .execute(state.db.pool())
            .await?;
        sqlx::query("UPDATE users SET active_org_id = $2 WHERE id = $1").bind(user).bind(loka_shared::orgs::DEFAULT_ORG).execute(state.db.pool()).await?;
        Ok(user)
    }

    #[tokio::test]
    async fn owners_delete_marks_and_others_cannot() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("REPORTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let state = state(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let owner = member(&state, format!("{tag}@marks.test")).await?;
        let other = member(&state, format!("{tag}-x@marks.test")).await?;
        let canvas = Canvas::from_raw(&json!({ "items": [{ "id": "n", "kind": "note", "text": "Wall" }] }), "person")?;
        let new = NewReport {
            owner_user_id: owner,
            by: ReportActor::User(owner),
            title: "Wall plan".into(),
            body: String::new(),
            folder: None,
            tags: vec![],
            visibility: None,
            pinned: false,
            task_id: None,
        };
        let report = state.world.create_canvas(new, canvas).await?;
        let mark = state.world.add_canvas_mark(&report, owner, NewMark { item_id: Some("n".into()), body: "taller".into(), ..Default::default() }).await?;
        let delete = |user: Uuid| delete_mark(State(state.clone()), Extension(UserId(user)), Extension(Role::Write), Path((report.id, mark.id)));

        assert_eq!(delete(other).await.unwrap_err().0, StatusCode::NOT_FOUND);
        assert_eq!(state.world.canvas_marks(report.id).await?.len(), 1);
        assert_eq!(delete(owner).await.map_err(|e| anyhow::anyhow!("{e:?}"))?, StatusCode::NO_CONTENT);
        assert!(state.world.canvas_marks(report.id).await?.is_empty());
        assert_eq!(delete(owner).await.unwrap_err().0, StatusCode::NOT_FOUND);

        sqlx::query("DELETE FROM world_events WHERE payload->>'owner_user_id' = $1").bind(owner.to_string()).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM org_members WHERE user_id = ANY($1)").bind(vec![owner, other]).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![owner, other]).execute(state.db.pool()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn stale_edits_get_a_conflict_with_the_current_version() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("REPORTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let state = state(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let owner = member(&state, format!("{tag}@stale.test")).await?;
        let new = NewReport {
            owner_user_id: owner,
            by: ReportActor::User(owner),
            title: "Launch notes".into(),
            body: "First draft".into(),
            folder: None,
            tags: vec![],
            visibility: None,
            pinned: false,
            task_id: None,
        };
        let report = state.world.create_report(new).await?;
        let edit = |body: &str, base: Option<chrono::DateTime<chrono::Utc>>| {
            let patch = ReportPatch { body: Some(body.into()), base_updated_at: base, ..Default::default() };
            update(State(state.clone()), Extension(UserId(owner)), Extension(Role::Write), Path(report.id), Json(patch))
        };

        let phone = edit("From the phone", Some(report.updated_at)).await.map_err(|e| anyhow::anyhow!("{:?}", e.status()))?.0;
        assert_eq!(phone.body, "From the phone");
        assert!(phone.updated_at > report.updated_at);

        let refused = edit("From the laptop", Some(report.updated_at)).await.unwrap_err();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(refused.into_body(), 1 << 20).await?)?;
        assert_eq!(body["error"], reports::STALE_REPORT);
        assert_eq!(body["report"]["body"], "From the phone");
        assert_eq!(body["report"]["updated_at"], json!(phone.updated_at));
        assert_eq!(state.world.get_report(report.id).await?.unwrap().body, "From the phone");

        let rebased = edit("From the laptop", Some(phone.updated_at)).await.map_err(|e| anyhow::anyhow!("{:?}", e.status()))?.0;
        assert_eq!(rebased.body, "From the laptop");
        assert_eq!(rebased.revision, phone.revision, "one author's quick edits share a revision, so the guard uses updated_at");
        let unguarded = edit("Old client", None).await.map_err(|e| anyhow::anyhow!("{:?}", e.status()))?.0;
        assert_eq!(unguarded.body, "Old client");

        let base = unguarded.updated_at;
        let racer = |body: &'static str| {
            let store = state.world.clone();
            async move { store.update_report(report.id, ReportActor::User(owner), &ReportPatch { body: Some(body.into()), base_updated_at: Some(base), ..Default::default() }).await }
        };
        let (a, b) = tokio::join!(racer("Racer A"), racer("Racer B"));
        let wins = [&a, &b].iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "exactly one writer on the same base wins");
        let loser = [a, b].into_iter().find_map(Result::err).unwrap();
        let winner = state.world.get_report(report.id).await?.unwrap();
        assert_eq!(loser.downcast_ref::<StaleReport>().map(|s| s.0.body.clone()), Some(winner.body.clone()));

        sqlx::query("DELETE FROM world_events WHERE payload->>'owner_user_id' = $1").bind(owner.to_string()).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM org_members WHERE user_id = $1").bind(owner).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(state.db.pool()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn unguarded_writes_build_on_the_locked_row() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("REPORTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let state = state(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let owner = member(&state, format!("{tag}@unguarded.test")).await?;
        let new = NewReport {
            owner_user_id: owner,
            by: ReportActor::User(owner),
            title: "Shared notes".into(),
            body: "Start".into(),
            folder: None,
            tags: vec![],
            visibility: None,
            pinned: false,
            task_id: None,
        };
        let report = state.world.create_report(new).await?;
        let store = &state.world;

        let append = |line: &'static str| async move { store.update_report(report.id, ReportActor::User(owner), &ReportPatch { append: Some(line.into()), ..Default::default() }).await };
        let (a, b) = tokio::join!(append("Agent line A"), append("Agent line B"));
        a?;
        b?;
        let body = store.get_report(report.id).await?.unwrap().body;
        assert!(body.starts_with("Start\n\n"), "{body}");
        assert!(body.contains("Agent line A") && body.contains("Agent line B"), "two unguarded appends both land: {body}");

        store
            .update_report(report.id, ReportActor::User(owner), &ReportPatch { title: Some("Renamed on the web".into()), pinned: Some(true), visibility: Some("public".into()), ..Default::default() })
            .await?;
        let agent = store.update_report(report.id, ReportActor::User(owner), &ReportPatch { body: Some("Agent rewrite".into()), ..Default::default() }).await?.unwrap();
        assert_eq!(agent.body, "Agent rewrite");
        assert_eq!((agent.title.as_str(), agent.pinned, agent.visibility.as_str()), ("Renamed on the web", true, "public"), "fields the patch leaves out come from the current row");

        let guarded = |body: &'static str| async move {
            store.update_report(report.id, ReportActor::User(owner), &ReportPatch { body: Some(body.into()), base_updated_at: Some(agent.updated_at), ..Default::default() }).await
        };
        let (saved, appended) = tokio::join!(guarded("Web save"), append("Agent tail"));
        appended?;
        let last = store.get_report(report.id).await?.unwrap();
        match saved {
            Ok(_) => assert!(last.body.starts_with("Web save") && last.body.contains("Agent tail"), "{}", last.body),
            Err(e) => {
                assert!(e.downcast_ref::<StaleReport>().is_some(), "{e}");
                assert!(last.body.starts_with("Agent rewrite") && last.body.contains("Agent tail"), "{}", last.body);
            }
        }

        sqlx::query("DELETE FROM world_events WHERE payload->>'owner_user_id' = $1").bind(owner.to_string()).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM org_members WHERE user_id = $1").bind(owner).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(state.db.pool()).await?;
        Ok(())
    }
}

#[derive(Serialize)]
pub struct Practiced {
    challenge_id: Uuid,
    report_id: Option<Uuid>,
    created: bool,
}

pub async fn practice_this(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<Json<Practiced>, (StatusCode, String)> {
    let report = readable(&state, user.0, role, id).await.map_err(status)?;
    if report.owner_user_id != user.0 {
        return Err((StatusCode::FORBIDDEN, loka_shared::academy::NOT_YOUR_REPORT.to_string()));
    }
    let org = org_of(&state, user.0).await.map_err(status)?;
    let (lesson, created) = state.world.practice_this(user.0, org, id).await.map_err(invalid)?;
    Ok(Json(Practiced {
        challenge_id: lesson.id,
        report_id: loka_shared::academy::uuid_in(&lesson.data, "report_id"),
        created,
    }))
}

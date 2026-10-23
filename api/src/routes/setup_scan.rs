use axum::{extract::State, http::StatusCode, Extension, Json};
use loka_shared::access::Role;
use loka_shared::setup_scan::{self as scan, CardKind};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::UserId;
use crate::routes::world::{internal, load};
use crate::AppState;

#[derive(Serialize)]
pub struct Offered {
    agent_id: Uuid,
    posted: bool,
    reason: Option<String>,
}

pub async fn offer(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
) -> Result<Json<Offered>, StatusCode> {
    if role < Role::Write {
        return Err(StatusCode::FORBIDDEN);
    }
    let main = state.world.main_agent_for(user.0).await.map_err(internal)?.ok_or(StatusCode::CONFLICT)?;
    match state.world.post_setup_request(user.0, &main, false).await.map_err(internal)? {
        Ok(_) => Ok(Json(Offered { agent_id: main.id, posted: true, reason: None })),
        Err(why) => {
            state.world.post_setup_note(&main, &why).await.map_err(internal)?;
            Ok(Json(Offered { agent_id: main.id, posted: false, reason: Some(why) }))
        }
    }
}

pub async fn card_kind(state: &AppState, agent_id: Uuid, card: Option<Uuid>) -> Result<Option<CardKind>, StatusCode> {
    match card {
        Some(card) => state.world.setup_card_kind(agent_id, card).await.map_err(internal),
        None => Ok(None),
    }
}

pub async fn answer(state: &AppState, user: &UserId, agent_id: Uuid, card: Uuid, kind: CardKind, text: &str) -> Result<(), StatusCode> {
    let agent = load(state, agent_id).await?;
    if agent.owner_user_id != Some(user.0) {
        return Err(StatusCode::FORBIDDEN);
    }
    match kind {
        CardKind::Request => {
            let Some(sources) = state.world.take_setup_request(agent_id, card).await.map_err(internal)? else {
                return Ok(());
            };
            let onboarding = state.world.onboarding_for(user.0, agent_id).await.map_err(internal)?.is_some();
            let note = match scan::parse_request(text, &sources) {
                None if onboarding => None,
                None => Some("No problem. Ask me any time to look at how you work and suggest a setup.".to_string()),
                Some(request) => Some(match state.world.start_setup_scan(user.0, &agent, &request).await.map_err(internal)? {
                    Ok(_) => format!(
                        "Reading the last {} days of {} on your Mac now. My suggestions will show up here as a card in a few minutes.",
                        request.days,
                        request.names()
                    ),
                    Err(why) => why,
                }),
            };
            if let Some(note) = note {
                state.world.post_setup_note(&agent, &note).await.map_err(internal)?;
            }
            if onboarding {
                crate::routes::onboarding::after_scan(state, user, agent_id).await?;
            }
        }
        CardKind::Suggestions => {
            let Some(suggestions) = state.world.take_setup_card(agent_id, card).await.map_err(internal)? else {
                return Ok(());
            };
            if scan::is_decline(text) {
                state.world.post_setup_note(&agent, "Okay, I created nothing. Ask me to look again any time.").await.map_err(internal)?;
                return Ok(());
            }
            let picks = scan::picked_ids(text);
            let applied = state.world.apply_setup(user.0, &agent, &suggestions, &picks).await.map_err(internal)?;
            state.world.post_setup_applied(&agent, &applied).await.map_err(internal)?;
        }
    }
    Ok(())
}

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use loka_shared::{
    access::Viewer,
    orgs::OrgRole,
    portable::{Bundle, MovingKey},
};
use serde::Serialize;

use crate::{auth::UserId, AppState};

#[derive(Serialize)]
pub struct MoveOut {
    bundle: Bundle,
    keys: Vec<MovingKey>,
}

pub fn from_the_app(headers: &HeaderMap, secret: &str) -> bool {
    if headers.contains_key(header::ORIGIN) || headers.contains_key(header::REFERER) {
        return false;
    }
    headers
        .get(crate::auth::APP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|given| crate::auth::app_session_matches(given, secret))
}

pub async fn move_out(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    if !from_the_app(&headers, crate::auth::app_session_secret()) {
        return Err(StatusCode::FORBIDDEN);
    }
    let org = viewer.org.ok_or(StatusCode::FORBIDDEN)?;
    if viewer.org_role != Some(OrgRole::Owner) {
        return Err(StatusCode::FORBIDDEN);
    }
    let bundle = state
        .world
        .export_bundle(user.0, org, Some(loka_shared::app_links::app_url()))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Could not pack the Loka for the move");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let keys = state.world.moving_keys(user.0).await.map_err(|e| {
        tracing::error!(error = %e, "Could not read the keys for the move");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    tracing::info!(agents = bundle.agents.len(), keys = keys.len(), "Packed the Loka for the move to Pro");
    let mut response = Json(MoveOut { bundle, keys }).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn only_the_app_itself_may_move_out() {
        assert!(from_the_app(&headers(&[(crate::auth::APP_SESSION_HEADER, SECRET)]), SECRET));
        assert!(!from_the_app(&headers(&[]), SECRET), "no session header");
        assert!(!from_the_app(&headers(&[(crate::auth::APP_SESSION_HEADER, "wrong")]), SECRET));
        assert!(!from_the_app(&headers(&[(crate::auth::APP_SESSION_HEADER, SECRET), ("origin", "http://127.0.0.1:3000")]), SECRET), "a page in the browser");
        assert!(!from_the_app(&headers(&[(crate::auth::APP_SESSION_HEADER, SECRET), ("referer", "http://127.0.0.1:3000/world")]), SECRET), "a page in the browser");
        assert!(!from_the_app(&headers(&[(crate::auth::APP_SESSION_HEADER, SECRET), ("origin", "null")]), SECRET), "a sandboxed frame");
        assert!(!from_the_app(&headers(&[("cookie", &format!("{}={SECRET}", crate::auth::APP_SESSION_COOKIE))]), SECRET), "the window cookie alone");
        assert!(!from_the_app(&headers(&[(crate::auth::APP_SESSION_HEADER, "")]), ""), "an empty secret");
    }
}

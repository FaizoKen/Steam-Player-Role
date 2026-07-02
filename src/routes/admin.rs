//! Admin routes for the iframe role-config page and per-guild settings.
//!
//! Dual-mode access: every protected handler accepts EITHER a `Bearer ifs:…`
//! iframe-session token (RoleLogic dashboard embed) OR an `rl_session`
//! cookie + Manage-Server check (direct nav).

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::models::condition::Condition;
use crate::schema;
use crate::services::auth::{extract_bearer, require_guild_admin, require_manager};
use crate::services::sync::{preview_matching_count, ConfigSyncEvent};
use crate::services::{auth_gateway, csrf, rl_token};
use crate::AppState;

const ROLE_CONFIG_TEMPLATE: &str = include_str!("../../templates/role_config.html");

/// CSP for the pages this module serves — they must be embeddable by the
/// RoleLogic dashboard (and only by it, when configured).
fn admin_iframe_csp(dashboard_origin: Option<&str>) -> String {
    let ancestor = dashboard_origin.unwrap_or("*");
    format!("frame-ancestors {ancestor}")
}

// ---------------------------------------------------------------------
// Iframe role-config page (dual-mode: rl_token JWT entry OR cookie+manager)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RoleConfigPageQuery {
    #[serde(default)]
    rl_token: Option<String>,
}

pub async fn role_config_page(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
    Query(query): Query<RoleConfigPageQuery>,
) -> Response {
    let has_rl_token = query
        .rl_token
        .as_deref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);

    // `read_only` is true when a developer is impersonating the user.
    let (iframe_session, read_only) = match query.rl_token.as_deref() {
        Some(token) if !token.is_empty() => {
            match verify_iframe_entry(&state, &guild_id, &role_id, token).await {
                Ok((t, ro)) => (Some(t), ro),
                Err(resp) => return resp,
            }
        }
        _ => (None, false),
    };

    // Direct-nav path: cookie + Manage Server. A cross-site iframe will NOT
    // carry our first-party `rl_session` cookie, so landing here with no
    // rl_token while the request smells like a frame load almost always
    // means RoleLogic never appended `?rl_token=`. Surface that precisely
    // instead of a dead-end "sign in" the user can't action.
    if iframe_session.is_none() {
        if let Err(e) = require_manager(&state, &jar, &guild_id).await {
            if !has_rl_token && looks_embedded(&headers) {
                tracing::warn!(
                    guild_id,
                    role_id,
                    base_url = %state.config.base_url,
                    "role_config_page reached inside an iframe with no rl_token — \
                     RoleLogic did not pass an auth token. Verify BASE_URL exactly \
                     matches the plugin URL registered in RoleLogic."
                );
                return render_iframe_no_token(&state);
            }
            return render_signin_page(&state, &e.to_string());
        }
    }

    let body = ROLE_CONFIG_TEMPLATE
        .replace("__BASE_URL__", &state.config.base_url)
        .replace("__GUILD_ID__", &guild_id)
        .replace("__ROLE_ID__", &role_id)
        .replace("__IFRAME_TOKEN__", iframe_session.as_deref().unwrap_or(""))
        .replace("__READ_ONLY__", if read_only { "1" } else { "0" });

    let csp = admin_iframe_csp(state.config.rl_dashboard_origin.as_deref());
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
            (
                header::CACHE_CONTROL,
                "private, max-age=300, must-revalidate".to_string(),
            ),
        ],
        body,
    )
        .into_response()
}

/// Verify `?rl_token=…` and return a freshly minted iframe-session token.
/// On failure returns a rendered error page so the iframe shows something
/// useful instead of an empty body.
async fn verify_iframe_entry(
    state: &AppState,
    guild_id: &str,
    role_id: &str,
    rl_token_str: &str,
) -> Result<(String, bool), Response> {
    let api_token: Option<String> =
        sqlx::query_scalar("SELECT api_token FROM role_links WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id)
            .bind(role_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| render_inline_error(state, &format!("Database error: {e}")))?;

    let Some(api_token) = api_token else {
        return Err(render_inline_error(
            state,
            "This role link isn't registered with this plugin yet.",
        ));
    };

    let verified =
        rl_token::verify(rl_token_str, &api_token, &state.config.base_url).map_err(|e| {
            let msg = match e {
                rl_token::RlTokenError::Expired => {
                    "Your session expired. Reopen the plugin in the RoleLogic dashboard."
                }
                rl_token::RlTokenError::BadSignature | rl_token::RlTokenError::Malformed => {
                    "Invalid auth token."
                }
                rl_token::RlTokenError::WrongAudience => "Token is for a different plugin.",
                rl_token::RlTokenError::WrongIssuer => "Token was not issued by RoleLogic.",
            };
            render_inline_error(state, msg)
        })?;

    if verified.guild_id != guild_id || verified.role_id != role_id {
        return Err(render_inline_error(
            state,
            "Token does not match this role link.",
        ));
    }

    if verified.read_only {
        tracing::info!(
            guild_id,
            role_id,
            target = %verified.discord_id,
            actor = verified.actor_id.as_deref().unwrap_or("?"),
            "Role config opened read-only (developer impersonation)"
        );
    }

    // Carry the read-only flag into the minted iframe-session so every XHR is
    // gated; return it too so the page renders in read-only mode.
    let token = rl_token::mint_iframe_session(
        &verified.discord_id,
        guild_id,
        role_id,
        verified.read_only,
        &state.config.session_secret,
    );
    Ok((token, verified.read_only))
}

fn render_inline_error(state: &AppState, message: &str) -> Response {
    let base_url = &state.config.base_url;
    let msg = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        r##"<!DOCTYPE html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Cannot load configuration</title>
<link rel="icon" href="{base_url}/favicon.ico">
<style>body{{font-family:system-ui,sans-serif;background:#0f1115;color:#e8eaed;padding:32px 24px;line-height:1.5}}
h1{{color:#fca5a5;font-size:18px;margin-bottom:10px}}p{{color:#9aa3b2}}</style>
</head><body><h1>Cannot load configuration</h1><p>{msg}</p>
<p style="margin-top:14px;color:#7a8497">If you opened this from the RoleLogic dashboard, close and reopen the role's plugin tab.</p>
</body></html>"##
    );
    let csp = admin_iframe_csp(state.config.rl_dashboard_origin.as_deref());
    (
        StatusCode::FORBIDDEN,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        body,
    )
        .into_response()
}

fn looks_embedded(headers: &HeaderMap) -> bool {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    let dest = h("sec-fetch-dest");
    dest == "iframe" || dest == "frame" || h("sec-fetch-site") == "cross-site"
}

fn render_iframe_no_token(state: &AppState) -> Response {
    let base_url = &state.config.base_url;
    let body = format!(
        r##"<!DOCTYPE html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Configuration unavailable</title>
<link rel="icon" href="{base_url}/favicon.ico">
<style>body{{font-family:system-ui,sans-serif;background:#0f1115;color:#e8eaed;padding:32px 24px;line-height:1.55;max-width:560px}}
h1{{color:#fbbf24;font-size:18px;margin:0 0 10px}}p{{color:#9aa3b2;margin:8px 0}}
code{{background:#0b0d12;padding:2px 6px;border-radius:4px;font-size:12px}}</style>
</head><body>
<h1>RoleLogic didn't pass an authentication token</h1>
<p>This plugin page must be opened from inside the RoleLogic dashboard, which
attaches a one-time token. None arrived with this request.</p>
<p><strong>If you're the server admin:</strong> close this tab and reopen the
role's plugin tab from RoleLogic. If it keeps happening, the plugin is
mis-registered — its <code>BASE_URL</code> must exactly match the URL
configured for this plugin in RoleLogic: HTTPS, no trailing slash, and
including the <code>/steam-player-role</code> path prefix.</p>
<p style="color:#7a8497;font-size:12px;margin-top:16px">Configured BASE_URL:
<code>{base_url}</code></p>
</body></html>"##
    );
    let csp = admin_iframe_csp(state.config.rl_dashboard_origin.as_deref());
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        body,
    )
        .into_response()
}

fn render_signin_page(state: &AppState, reason: &str) -> Response {
    let base_url = &state.config.base_url;
    let reason = reason
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        r##"<!DOCTYPE html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in — Steam Player Roles</title>
<link rel="icon" href="{base_url}/favicon.ico">
<style>body{{font-family:system-ui,sans-serif;background:#0f1115;color:#e8eaed;padding:48px 24px;max-width:520px;margin:0 auto;line-height:1.55}}
h1{{font-size:22px;margin:0 0 12px}}p{{color:#9aa3b2}}
a.btn{{display:inline-block;margin-top:18px;background:#5865f2;color:#fff;padding:12px 22px;border-radius:8px;text-decoration:none;font-weight:600}}
.actions{{display:flex;gap:10px;align-items:center;flex-wrap:wrap;margin-top:18px}}
.actions a.btn{{margin-top:0}}
form.logout-form{{margin:0}}
button.logout{{background:none;color:#8a93a4;border:1px solid #2a2f3a;padding:10px 16px;border-radius:8px;font-size:13px;font-weight:600;cursor:pointer;font-family:inherit}}
button.logout:hover{{color:#fca5a5;border-color:#5c2630}}</style>
</head><body>
<h1>Sign in to continue</h1>
<p>You need <strong>Manage Server</strong> on this guild to edit its
Steam Player Roles configuration.</p>
<p style="color:#7a8497;font-size:12px">{reason}</p>
<div class="actions">
  <a class="btn" id="login">Sign in with Discord</a>
  <form class="logout-form" method="POST" action="/auth/logout">
    <button type="submit" class="logout">Sign out &amp; try another account</button>
  </form>
</div>
<script>
const ORIGIN=new URL("{base_url}").origin;
const RET=encodeURIComponent(location.pathname);
document.getElementById('login').href=ORIGIN+'/auth/login?return_to='+RET;
document.querySelectorAll('form.logout-form').forEach(f=>{{
  f.action=ORIGIN+'/auth/logout?return_to='+RET;
}});
</script>
</body></html>"##
    );
    let csp = admin_iframe_csp(state.config.rl_dashboard_origin.as_deref());
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        body,
    )
        .into_response()
}

// ---------------------------------------------------------------------
// Dual gate for role-link-scoped admin XHRs
// ---------------------------------------------------------------------

/// Outcome of an access check for the role-config endpoints: who is calling
/// and whether the session is read-only (a developer impersonating the user).
struct RoleConfigAccess {
    #[allow(dead_code)]
    discord_id: String,
    read_only: bool,
}

async fn require_role_config_access(
    state: &Arc<AppState>,
    jar: &CookieJar,
    headers: &HeaderMap,
    guild_id: &str,
    role_id: &str,
) -> Result<RoleConfigAccess, AppError> {
    if let Some(bearer) = extract_bearer(headers) {
        let s = rl_token::verify_iframe_session(&bearer, &state.config.session_secret).ok_or_else(
            || {
                AppError::UnauthorizedWith(
                    "Your session expired. Reopen the plugin in the RoleLogic dashboard.".into(),
                )
            },
        )?;
        if s.guild_id != guild_id || s.role_id != role_id {
            return Err(AppError::Forbidden(
                "Token does not grant access to this role link.".into(),
            ));
        }
        return Ok(RoleConfigAccess {
            discord_id: s.discord_id,
            read_only: s.read_only,
        });
    }
    let discord_id = require_manager(state, jar, guild_id).await?;
    Ok(RoleConfigAccess {
        discord_id,
        read_only: false,
    })
}

/// Serialize the (single) saved condition into the shape the config page
/// consumes: `{category, field, operator, value, value_end, app_id,
/// has_publisher_key}`. `field` is null when the role link is unconfigured.
fn config_json(conditions: &[Condition], has_publisher_key: bool) -> Value {
    match conditions.first() {
        Some(c) => json!({
            "category": if c.field.requires_app_id() { "game" } else { "account" },
            "field": c.field.json_key(),
            "operator": c.operator.key(),
            "value": c.value,
            "value_end": c.value_end,
            "app_id": c.app_id,
            "has_publisher_key": has_publisher_key,
        }),
        None => json!({
            "category": Value::Null,
            "field": Value::Null,
            "operator": Value::Null,
            "value": Value::Null,
            "value_end": Value::Null,
            "app_id": Value::Null,
            "has_publisher_key": has_publisher_key,
        }),
    }
}

// ---------------------------------------------------------------------
// GET /admin/{guild_id}/role/{role_id}/data
// ---------------------------------------------------------------------

pub async fn role_config_data(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    require_role_config_access(&state, &jar, &headers, &guild_id, &role_id).await?;

    let link = sqlx::query_as::<_, (sqlx::types::Json<Vec<Condition>>, Option<String>, i32)>(
        "SELECT conditions, publisher_key, config_version \
         FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(&guild_id)
    .bind(&role_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        AppError::NotFound("This role link doesn't exist. Has it been added in RoleLogic?".into())
    })?;
    let (conditions, publisher_key, config_version) = link;

    let view_permission: String =
        sqlx::query_scalar("SELECT view_permission FROM guild_settings WHERE guild_id = $1")
            .bind(&guild_id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| "members".to_string());

    Ok(Json(json!({
        "guild_id": guild_id,
        "role_id": role_id,
        "config": config_json(&conditions, publisher_key.is_some()),
        "config_version": config_version,
        // Per-guild verify URL. The `?guild=<id>` query param lets the
        // verify page show server context and deep-link correctly.
        "verify_url": format!("{}/verify?guild={}", state.config.base_url, guild_id),
        "players": {
            "url": format!("{}/players/{}", state.config.base_url, guild_id),
            "view_permission": view_permission,
        },
    })))
}

// ---------------------------------------------------------------------
// POST /admin/{guild_id}/role/{role_id}/save  (optimistic-locked)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RoleConfigSaveBody {
    pub config_version: i32,
    /// Flat key-value map in the exact shape `schema::parse_config` /
    /// `schema::parse_publisher_key` validate (`condition_category`,
    /// `app_id`, `condition_field_game`, `value_num_game`, …).
    pub config: HashMap<String, Value>,
}

pub async fn role_config_save(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
    Json(body): Json<RoleConfigSaveBody>,
) -> Result<Json<Value>, AppError> {
    if extract_bearer(&headers).is_none() {
        csrf::verify_origin(&headers, &state.allowed_origins)?;
    }
    let access = require_role_config_access(&state, &jar, &headers, &guild_id, &role_id).await?;
    // Read-only sessions (a developer impersonating the user) may view but
    // not write — the server-side half of the read-only contract.
    if access.read_only {
        return Err(AppError::Forbidden(
            "This configuration is read-only while impersonating a user.".into(),
        ));
    }

    let conditions = schema::parse_config(&body.config)?;
    let publisher_key_action = schema::parse_publisher_key(&body.config)?;

    let mut tx = state.pool.begin().await?;

    let result = sqlx::query(
        "UPDATE role_links \
         SET conditions = $1, config_version = config_version + 1, updated_at = now() \
         WHERE guild_id = $2 AND role_id = $3 AND config_version = $4",
    )
    .bind(sqlx::types::Json(&conditions))
    .bind(&guild_id)
    .bind(&role_id)
    .bind(body.config_version)
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        let exists: Option<i32> = sqlx::query_scalar(
            "SELECT config_version FROM role_links WHERE guild_id = $1 AND role_id = $2",
        )
        .bind(&guild_id)
        .bind(&role_id)
        .fetch_optional(&mut *tx)
        .await?;
        return match exists {
            None => Err(AppError::NotFound(
                "This role link doesn't exist. Has it been added in RoleLogic?".into(),
            )),
            Some(_) => Err(AppError::StaleVersion),
        };
    }

    match &publisher_key_action {
        schema::PublisherKeyAction::Keep => {}
        schema::PublisherKeyAction::Clear => {
            sqlx::query(
                "UPDATE role_links SET publisher_key = NULL \
                 WHERE guild_id = $1 AND role_id = $2",
            )
            .bind(&guild_id)
            .bind(&role_id)
            .execute(&mut *tx)
            .await?;
        }
        schema::PublisherKeyAction::Set(key) => {
            sqlx::query(
                "UPDATE role_links SET publisher_key = $1 \
                 WHERE guild_id = $2 AND role_id = $3",
            )
            .bind(key)
            .bind(&guild_id)
            .bind(&role_id)
            .execute(&mut *tx)
            .await?;
        }
    }

    let new_version: i32 = sqlx::query_scalar(
        "SELECT config_version FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(&guild_id)
    .bind(&role_id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    tracing::info!(guild_id, role_id, "Config updated via role-config page");

    let _ = state
        .config_sync_tx
        .send(ConfigSyncEvent {
            guild_id: guild_id.clone(),
            role_id: role_id.clone(),
        })
        .await;

    // Echo the saved config so the page can reset its baseline without a
    // second round-trip.
    let has_key = match &publisher_key_action {
        schema::PublisherKeyAction::Set(_) => true,
        schema::PublisherKeyAction::Clear => false,
        schema::PublisherKeyAction::Keep => {
            sqlx::query_scalar::<_, bool>(
                "SELECT publisher_key IS NOT NULL FROM role_links \
                 WHERE guild_id = $1 AND role_id = $2",
            )
            .bind(&guild_id)
            .bind(&role_id)
            .fetch_one(&state.pool)
            .await?
        }
    };

    Ok(Json(json!({
        "success": true,
        "config_version": new_version,
        "config": config_json(&conditions, has_key),
    })))
}

// ---------------------------------------------------------------------
// GET /admin/{guild_id}/role/{role_id}/preview  — count for the saved rule
// POST same path with a proposed config body — preview an unsaved edit
// ---------------------------------------------------------------------

pub async fn role_config_preview(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    require_role_config_access(&state, &jar, &headers, &guild_id, &role_id).await?;

    let link = sqlx::query_as::<_, (sqlx::types::Json<Vec<Condition>>, Option<String>)>(
        "SELECT conditions, publisher_key FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(&guild_id)
    .bind(&role_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Role link not found.".into()))?;

    preview_count_for(&state, &guild_id, &link.0, link.1.is_some()).await
}

#[derive(Deserialize)]
pub struct RoleConfigPreviewBody {
    pub config: HashMap<String, Value>,
}

pub async fn role_config_preview_edit(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
    Json(body): Json<RoleConfigPreviewBody>,
) -> Result<Json<Value>, AppError> {
    if extract_bearer(&headers).is_none() {
        csrf::verify_origin(&headers, &state.allowed_origins)?;
    }
    require_role_config_access(&state, &jar, &headers, &guild_id, &role_id).await?;

    let conditions = schema::parse_config(&body.config)?;
    // A freshly typed publisher key has no ownership cache yet, so the
    // preview falls back to the public-library check either way; only a
    // stored key makes the partner-API data available.
    let has_key = match schema::parse_publisher_key(&body.config)? {
        schema::PublisherKeyAction::Clear => false,
        _ => sqlx::query_scalar::<_, bool>(
            "SELECT publisher_key IS NOT NULL FROM role_links \
             WHERE guild_id = $1 AND role_id = $2",
        )
        .bind(&guild_id)
        .bind(&role_id)
        .fetch_optional(&state.pool)
        .await?
        .unwrap_or(false),
    };

    preview_count_for(&state, &guild_id, &conditions, has_key).await
}

async fn preview_count_for(
    state: &Arc<AppState>,
    guild_id: &str,
    conditions: &[Condition],
    has_publisher_key: bool,
) -> Result<Json<Value>, AppError> {
    let member_ids = match auth_gateway::fetch_guild_member_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        guild_id,
    )
    .await
    {
        Ok(v) => v,
        Err(_) => {
            return Ok(Json(json!({
                "available": false,
                "reason": "Member list temporarily unavailable; preview will work once the Auth Gateway responds."
            })))
        }
    };

    let (matching, linked) =
        preview_matching_count(conditions, has_publisher_key, &member_ids, &state.pool).await?;

    Ok(Json(json!({
        "available": true,
        "matching": matching,
        "linked": linked,
    })))
}

// ---------------------------------------------------------------------
// POST /admin/{guild_id}/view-permission  (per-guild players-list setting)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ViewPermBody {
    pub view_permission: String,
}

pub async fn set_view_permission(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(body): Json<ViewPermBody>,
) -> Result<Json<Value>, AppError> {
    if extract_bearer(&headers).is_none() {
        csrf::verify_origin(&headers, &state.allowed_origins)?;
    }
    let access = require_guild_admin(&state, &jar, &headers, &guild_id).await?;
    if access.read_only {
        return Err(AppError::Forbidden(
            "This configuration is read-only while impersonating a user.".into(),
        ));
    }

    let vp = match body.view_permission.as_str() {
        "managers" | "members" => body.view_permission.as_str(),
        other => {
            return Err(AppError::BadRequest(format!(
                "Unknown view_permission '{other}' (expected managers|members)."
            )))
        }
    };

    sqlx::query(
        "INSERT INTO guild_settings (guild_id, view_permission, updated_at) \
         VALUES ($1, $2, now()) \
         ON CONFLICT (guild_id) DO UPDATE SET view_permission = EXCLUDED.view_permission, \
                                              updated_at = now()",
    )
    .bind(&guild_id)
    .bind(vp)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({ "success": true, "view_permission": vp })))
}

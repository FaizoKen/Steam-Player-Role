use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::session;
use crate::services::sync::PlayerSyncEvent;
use crate::AppState;

const SESSION_COOKIE: &str = "rl_session";

fn get_session(jar: &CookieJar, secret: &str) -> Result<(String, String), AppError> {
    let cookie = jar.get(SESSION_COOKIE).ok_or(AppError::Unauthorized)?;
    session::verify_session(cookie.value(), secret).ok_or(AppError::Unauthorized)
}

fn derive_origin(base_url: &str) -> String {
    if let Some(scheme_end) = base_url.find("://") {
        let after_scheme = scheme_end + 3;
        if let Some(path_slash) = base_url[after_scheme..].find('/') {
            return base_url[..after_scheme + path_slash].to_string();
        }
    }
    base_url.to_string()
}

pub fn render_verify_page(base_url: &str) -> String {
    let login_url = format!("{base_url}/verify/login");

    format!(
        r##"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Steam Player Roles - Link Account</title>
    <link rel="icon" href="{base_url}/favicon.ico" type="image/x-icon">
    <meta name="description" content="Link your Discord account with your Steam profile to automatically receive server roles based on your Steam data.">
    <meta property="og:type" content="website">
    <meta property="og:title" content="Steam Player Roles - Link Account">
    <meta property="og:description" content="Link your Discord account with your Steam profile to automatically receive server roles based on your Steam data.">
    <meta property="og:url" content="{base_url}/verify">
    <meta name="theme-color" content="#1b2838">
    <style>
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{ font-family: system-ui, -apple-system, sans-serif; max-width: 580px; margin: 0 auto; padding: 32px 20px; background: #1b2838; color: #c7d5e0; min-height: 100vh; }}
        h1 {{ color: #66c0f4; font-size: 24px; margin-bottom: 4px; }}
        h2 {{ color: #fff; font-size: 17px; margin-bottom: 14px; }}
        p {{ line-height: 1.6; margin: 6px 0; font-size: 14px; }}
        a {{ color: #66c0f4; }}
        .subtitle {{ color: #7a8a99; font-size: 14px; margin-bottom: 20px; }}
        .card {{ background: #2a475e; padding: 22px; border-radius: 10px; margin: 14px 0; border: 1px solid #3a6186; }}
        .btn {{ display: inline-flex; align-items: center; gap: 8px; padding: 10px 22px; color: #fff; text-decoration: none; border-radius: 6px; font-size: 14px; font-weight: 500; border: none; cursor: pointer; font-family: inherit; transition: background .15s; }}
        .btn-discord {{ background: #5865f2; }}
        .btn-discord:hover {{ background: #4752c4; }}
        .btn-steam {{ background: #588a1b; }}
        .btn-steam:hover {{ background: #4a7516; }}
        .btn-danger {{ background: transparent; color: #f87171; border: 1px solid #7f1d1d; font-size: 13px; padding: 8px 16px; }}
        .btn-danger:hover {{ background: #7f1d1d33; }}
        .badge {{ display: inline-block; padding: 3px 10px; border-radius: 20px; font-size: 12px; font-weight: 500; }}
        .badge-ok {{ background: #052e16; color: #4ade80; border: 1px solid #14532d; }}
        .msg {{ padding: 10px 14px; border-radius: 6px; margin: 12px 0; font-size: 13px; line-height: 1.5; }}
        .msg-error {{ background: #1c0a0a; color: #fca5a5; border: 1px solid #7f1d1d; }}
        .msg-success {{ background: #052e16; color: #86efac; border: 1px solid #14532d; }}
        .info-row {{ display: flex; align-items: center; gap: 8px; margin: 6px 0; font-size: 14px; }}
        .info-row .label {{ color: #7a8a99; min-width: 80px; }}
        .info-row .val {{ color: #66c0f4; font-weight: 600; }}
        .actions {{ display: flex; gap: 8px; margin-top: 16px; flex-wrap: wrap; }}
        .hidden {{ display: none !important; }}
        .divider {{ border: none; border-top: 1px solid #3a6186; margin: 16px 0; }}
        .trust-note {{ font-size: 13px; color: #8f9bab; background: #1b2838; border-left: 3px solid #66c0f4; padding: 10px 14px; border-radius: 0 6px 6px 0; margin: 10px 0; line-height: 1.6; }}
        .trust-note strong {{ color: #c7d5e0; }}
        .btn-logout {{ background: transparent; color: #8f9bab; border: 1px solid #3a6186; padding: 5px 12px; border-radius: 6px; font-size: 12px; cursor: pointer; font-family: inherit; transition: all .15s; }}
        .btn-logout:hover {{ color: #f87171; border-color: #7f1d1d; background: #7f1d1d22; }}
    </style>
</head>
<body>
    <div style="display:flex; align-items:center; justify-content:space-between; margin-bottom:4px;">
        <div style="display:flex; align-items:center; gap:10px;">
            <h1 style="margin:0;">Steam Player Roles</h1>
            <span style="font-size:11px; color:#7a8a99; background:#1b2838; padding:2px 8px; border-radius:4px;">Powered by <a href="https://rolelogic.faizo.net" target="_blank" rel="noopener" style="color:#66c0f4; text-decoration:none;">RoleLogic</a></span>
        </div>
        <button id="logout-btn" class="btn-logout hidden" onclick="doLogout()">Logout</button>
    </div>
    <p class="subtitle">Link your Discord account with your Steam profile to automatically receive server roles.</p>

    <div id="loading-section" class="card"><p style="color: #7a8a99;">Loading...</p></div>

    <div id="login-section" class="card hidden">
        <h2>Step 1: Sign in with Discord</h2>
        <p>Sign in so we know which Discord account to assign roles to.</p>
        <p class="trust-note">We request the <strong>identify</strong> and <strong>guilds</strong> scopes — we cannot read your messages, join servers, or access anything else on your account.</p>
        <div class="actions">
            <a href="{login_url}" class="btn btn-discord">
                <svg width="20" height="15" viewBox="0 0 71 55" fill="white"><path d="M60.1 4.9A58.5 58.5 0 0045.4.2a.2.2 0 00-.2.1 40.8 40.8 0 00-1.8 3.7 54 54 0 00-16.2 0A37.3 37.3 0 0025.4.3a.2.2 0 00-.2-.1A58.4 58.4 0 0010.6 4.9a.2.2 0 00-.1.1C1.5 18 -.9 30.6.3 43a.2.2 0 00.1.2 58.7 58.7 0 0017.7 9 .2.2 0 00.3-.1 42 42 0 003.6-5.9.2.2 0 00-.1-.3 38.6 38.6 0 01-5.5-2.6.2.2 0 01 0-.4l1.1-.9a.2.2 0 01.2 0 41.9 41.9 0 0035.6 0 .2.2 0 01.2 0l1.1.9a.2.2 0 010 .3 36.3 36.3 0 01-5.5 2.7.2.2 0 00-.1.3 47.2 47.2 0 003.6 5.9.2.2 0 00.3.1A58.5 58.5 0 0070.3 43a.2.2 0 00.1-.2c1.4-14.7-2.4-27.5-10.2-38.8a.2.2 0 00-.1 0zM23.7 35.3c-3.4 0-6.1-3.1-6.1-6.8s2.7-6.9 6.1-6.9 6.2 3.1 6.1 6.9c0 3.7-2.7 6.8-6.1 6.8zm22.6 0c-3.4 0-6.1-3.1-6.1-6.8s2.7-6.9 6.1-6.9 6.2 3.1 6.1 6.9c0 3.7-2.7 6.8-6.1 6.8z"/></svg>
                Login with Discord
            </a>
        </div>
    </div>

    <div id="linked-section" class="card hidden">
        <div style="display:flex; align-items:center; gap:10px; margin-bottom:14px;">
            <h2 style="margin:0;">Account Linked</h2>
            <span class="badge badge-ok">Verified</span>
        </div>
        <div class="info-row"><span class="label">Steam</span> <span class="val" id="linked-steam"></span></div>
        <div class="info-row"><span class="label">Discord</span> <span class="val" id="linked-discord" style="color:#8f9bab;font-weight:400;font-size:13px;"></span></div>
        <p style="color:#4ade80; margin-top:12px; font-size:13px;">Your roles are assigned automatically based on your Steam data.</p>
        <hr class="divider">
        <div class="actions">
            <button class="btn btn-danger" onclick="doUnlink()">Unlink Account</button>
        </div>
    </div>

    <div id="steam-section" class="card hidden">
        <h2>Step 2: Link Your Steam Account</h2>
        <p>Signed in as <span id="steam-discord" style="color:#66c0f4;"></span></p>
        <p style="margin-bottom:12px;">Click below to sign in via Steam. You'll be redirected to Steam's login page and then back here.</p>
        <p class="trust-note">Steam uses OpenID authentication — we never see your Steam password. We only receive your public Steam ID to link with your Discord account.</p>
        <div class="actions">
            <a href="{base_url}/verify/steam" class="btn btn-steam">
                <svg width="20" height="20" viewBox="0 0 256 259" fill="white"><path d="M128.2 0C58.5 0 1.6 53.9.1 122.5l68.8 28.4a36.2 36.2 0 0 1 20.5-6.3h1.9l30.7-44.4v-.7c0-27 22-49 49-49s49 22 49 49-22 49-49 49h-1.1l-43.7 31.2v1.4c0 20.3-16.5 36.8-36.8 36.8a37 37 0 0 1-36.5-31.5L3.5 162.3C18.9 217 67.8 258.6 128.2 258.6c71.5 0 127.8-57.5 127.8-129.3C256 57.5 199.7 0 128.2 0z"/></svg>
                Link Steam Account
            </a>
        </div>
    </div>

    <div id="msg" class="hidden"></div>

    <noscript><p style="color:#f87171; margin-top:20px;">JavaScript is required.</p></noscript>

    <script>
    const API = '{base_url}';

    async function api(method, path, body) {{
        const opts = {{ method, headers: {{}}, credentials: 'include' }};
        if (body) {{
            opts.headers['Content-Type'] = 'application/json';
            opts.body = JSON.stringify(body);
        }}
        const res = await fetch(API + path, opts);
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || 'Request failed');
        return data;
    }}

    function showSection(id) {{
        ['loading-section','login-section','linked-section','steam-section'].forEach(s =>
            document.getElementById(s).classList.add('hidden')
        );
        document.getElementById(id).classList.remove('hidden');
    }}

    function showMsg(text, type) {{
        const el = document.getElementById('msg');
        el.className = 'msg msg-' + type;
        el.textContent = text;
        el.classList.remove('hidden');
        if (type === 'success') setTimeout(() => el.classList.add('hidden'), 6000);
    }}

    function clearMsg() {{ document.getElementById('msg').classList.add('hidden'); }}

    let currentName = '';

    async function init() {{
        try {{
            const s = await api('GET', '/verify/status');
            currentName = s.display_name || '';
            document.getElementById('logout-btn').classList.remove('hidden');
            if (s.linked) {{
                document.getElementById('linked-steam').textContent = s.steam_name || s.linked;
                document.getElementById('linked-discord').textContent = s.display_name;
                showSection('linked-section');
            }} else {{
                document.getElementById('steam-discord').textContent = s.display_name;
                showSection('steam-section');
            }}
        }} catch (e) {{
            showSection('login-section');
        }}
    }}

    async function doLogout() {{
        clearMsg();
        try {{
            await api('POST', '/verify/logout');
            document.getElementById('logout-btn').classList.add('hidden');
            showSection('login-section');
            showMsg('Logged out.', 'success');
        }} catch (e) {{ showMsg(e.message, 'error'); }}
    }}

    async function doUnlink() {{
        clearMsg();
        if (!confirm('Unlink your Steam account? You will lose all assigned roles.')) return;
        try {{
            await api('POST', '/verify/unlink');
            document.getElementById('steam-discord').textContent = currentName;
            showSection('steam-section');
            showMsg('Account unlinked.', 'success');
        }} catch (e) {{ showMsg(e.message, 'error'); }}
    }}

    // Check for callback result
    const params = new URLSearchParams(window.location.search);
    if (params.get('linked') === 'true') {{
        window.history.replaceState({{}}, '', window.location.pathname);
    }} else if (params.get('error')) {{
        const err = params.get('error');
        window.history.replaceState({{}}, '', window.location.pathname);
        setTimeout(() => showMsg(decodeURIComponent(err), 'error'), 100);
    }}

    init();
    </script>
</body>
</html>"##
    )
}

pub async fn verify_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        state.verify_html.clone(),
    )
}

pub async fn login(State(_state): State<Arc<AppState>>) -> Response {
    let return_to = "/steam-player-role/verify";
    let url = format!("/auth/login?return_to={}", urlencoding::encode(return_to));
    Redirect::temporary(&url).into_response()
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<Json<Value>, AppError> {
    let (discord_id, display_name) = get_session(&jar, &state.config.session_secret)?;

    let account = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT steam_id, steam_name FROM linked_accounts WHERE discord_id = $1",
    )
    .bind(&discord_id)
    .fetch_optional(&state.pool)
    .await?;

    Ok(Json(json!({
        "discord_id": discord_id,
        "display_name": display_name,
        "linked": account.as_ref().map(|a| &a.0),
        "steam_name": account.as_ref().and_then(|a| a.1.as_ref()),
    })))
}

/// Redirect to Steam OpenID login
pub async fn steam_login(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let (discord_id, _) = get_session(&jar, &state.config.session_secret)?;

    // Check if already linked
    let existing = sqlx::query_scalar::<_, String>(
        "SELECT steam_id FROM linked_accounts WHERE discord_id = $1",
    )
    .bind(&discord_id)
    .fetch_optional(&state.pool)
    .await?;

    if existing.is_some() {
        return Err(AppError::BadRequest(
            "You already have a linked Steam account. Unlink it first.".into(),
        ));
    }

    // Generate nonce for replay protection
    let nonce: String = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..32)
            .map(|_| format!("{:02x}", rng.gen::<u8>()))
            .collect()
    };

    let expires = chrono::Utc::now() + chrono::Duration::minutes(15);

    // Clean up old sessions and create new one
    sqlx::query("DELETE FROM verification_sessions WHERE discord_id = $1")
        .bind(&discord_id)
        .execute(&state.pool)
        .await?;

    sqlx::query(
        "INSERT INTO verification_sessions (discord_id, nonce, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(&discord_id)
    .bind(&nonce)
    .bind(expires)
    .execute(&state.pool)
    .await?;

    let return_to = format!("{}/verify/callback", state.config.base_url);
    let realm = derive_origin(&state.config.base_url);

    let steam_url = format!(
        "https://steamcommunity.com/openid/login?\
         openid.ns=http://specs.openid.net/auth/2.0\
         &openid.mode=checkid_setup\
         &openid.return_to={}\
         &openid.realm={}\
         &openid.identity=http://specs.openid.net/auth/2.0/identifier_select\
         &openid.claimed_id=http://specs.openid.net/auth/2.0/identifier_select",
        urlencoding::encode(&return_to),
        urlencoding::encode(&realm),
    );

    Ok(Redirect::temporary(&steam_url).into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    #[serde(rename = "openid.claimed_id")]
    openid_claimed_id: Option<String>,
    #[serde(rename = "openid.sig")]
    openid_sig: Option<String>,
    #[serde(rename = "openid.signed")]
    openid_signed: Option<String>,
    #[serde(rename = "openid.assoc_handle")]
    openid_assoc_handle: Option<String>,
    #[serde(rename = "openid.ns")]
    openid_ns: Option<String>,
    #[serde(rename = "openid.op_endpoint")]
    openid_op_endpoint: Option<String>,
    #[serde(rename = "openid.response_nonce")]
    openid_response_nonce: Option<String>,
    #[serde(rename = "openid.return_to")]
    openid_return_to: Option<String>,
    #[serde(rename = "openid.identity")]
    openid_identity: Option<String>,
}

/// Steam OpenID callback — validates the response and links the account
pub async fn callback(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let (discord_id, _) = get_session(&jar, &state.config.session_secret)?;

    // Extract SteamID64 from claimed_id
    let claimed_id = query
        .openid_claimed_id
        .as_deref()
        .ok_or(AppError::VerificationFailed("Missing claimed_id".into()))?;

    let steam_id = claimed_id
        .strip_prefix("https://steamcommunity.com/openid/id/")
        .ok_or(AppError::VerificationFailed("Invalid claimed_id format".into()))?;

    // Validate: must be numeric SteamID64
    if steam_id.len() != 17 || !steam_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::VerificationFailed("Invalid SteamID format".into()));
    }

    // Verify the response with Steam's check_authentication endpoint
    let verify_params = [
        ("openid.ns", query.openid_ns.as_deref().unwrap_or("")),
        ("openid.mode", "check_authentication"),
        ("openid.sig", query.openid_sig.as_deref().unwrap_or("")),
        ("openid.signed", query.openid_signed.as_deref().unwrap_or("")),
        ("openid.assoc_handle", query.openid_assoc_handle.as_deref().unwrap_or("")),
        ("openid.op_endpoint", query.openid_op_endpoint.as_deref().unwrap_or("")),
        ("openid.claimed_id", claimed_id),
        ("openid.identity", query.openid_identity.as_deref().unwrap_or("")),
        ("openid.response_nonce", query.openid_response_nonce.as_deref().unwrap_or("")),
        ("openid.return_to", query.openid_return_to.as_deref().unwrap_or("")),
    ];

    let resp = state
        .http
        .post("https://steamcommunity.com/openid/login")
        .form(&verify_params)
        .send()
        .await
        .map_err(|e| AppError::VerificationFailed(format!("Steam verification request failed: {e}")))?;

    let body = resp
        .text()
        .await
        .map_err(|e| AppError::VerificationFailed(format!("Failed to read Steam response: {e}")))?;

    if !body.contains("is_valid:true") {
        tracing::warn!(discord_id, steam_id, "Steam OpenID validation failed");
        let redirect_url = format!(
            "{}/verify?error={}",
            state.config.base_url,
            urlencoding::encode("Steam authentication failed. Please try again.")
        );
        return Ok(Redirect::temporary(&redirect_url).into_response());
    }

    // Verify we have a pending verification session for this user
    let session_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM verification_sessions WHERE discord_id = $1 AND expires_at > now())",
    )
    .bind(&discord_id)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(false);

    if !session_exists {
        let redirect_url = format!(
            "{}/verify?error={}",
            state.config.base_url,
            urlencoding::encode("Verification session expired. Please try again.")
        );
        return Ok(Redirect::temporary(&redirect_url).into_response());
    }

    // Check if this Steam ID is already linked to another Discord user
    let steam_taken = sqlx::query_scalar::<_, String>(
        "SELECT discord_id FROM linked_accounts WHERE steam_id = $1",
    )
    .bind(steam_id)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(other_discord) = steam_taken {
        if other_discord != discord_id {
            let redirect_url = format!(
                "{}/verify?error={}",
                state.config.base_url,
                urlencoding::encode("This Steam account is already linked to another Discord user.")
            );
            return Ok(Redirect::temporary(&redirect_url).into_response());
        }
    }

    // Fetch Steam profile name
    let ids: Vec<&str> = vec![steam_id];
    let steam_name = match state.steam_client.get_player_summaries(&ids).await {
        Ok(profiles) => profiles.first().and_then(|p| p.personaname.clone()),
        Err(_) => None,
    };

    // Link the account
    sqlx::query(
        "INSERT INTO linked_accounts (discord_id, steam_id, steam_name) VALUES ($1, $2, $3) \
         ON CONFLICT (discord_id) DO UPDATE SET steam_id = $2, steam_name = $3, linked_at = now()",
    )
    .bind(&discord_id)
    .bind(steam_id)
    .bind(&steam_name)
    .execute(&state.pool)
    .await?;

    // Create initial user_cache entry
    sqlx::query(
        "INSERT INTO user_cache (steam_id) VALUES ($1) ON CONFLICT (steam_id) DO NOTHING",
    )
    .bind(steam_id)
    .execute(&state.pool)
    .await?;

    // Clean up verification session
    sqlx::query("DELETE FROM verification_sessions WHERE discord_id = $1")
        .bind(&discord_id)
        .execute(&state.pool)
        .await?;

    // Trigger initial data fetch + sync
    let _ = state
        .player_sync_tx
        .send(PlayerSyncEvent::AccountLinked {
            discord_id: discord_id.clone(),
        })
        .await;

    tracing::info!(discord_id, steam_id, "Steam account linked");

    let redirect_url = format!("{}/verify?linked=true", state.config.base_url);
    Ok(Redirect::temporary(&redirect_url).into_response())
}

pub async fn logout(jar: CookieJar) -> (CookieJar, Json<Value>) {
    let cookie = Cookie::build(SESSION_COOKIE).path("/");
    let jar = jar.remove(cookie);
    (jar, Json(json!({"success": true})))
}

pub async fn unlink(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<Json<Value>, AppError> {
    let (discord_id, _) = get_session(&jar, &state.config.session_secret)?;

    let account = sqlx::query_as::<_, (String,)>(
        "SELECT steam_id FROM linked_accounts WHERE discord_id = $1",
    )
    .bind(&discord_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound("No linked account found".into()))?;

    sqlx::query("DELETE FROM linked_accounts WHERE discord_id = $1")
        .bind(&discord_id)
        .execute(&state.pool)
        .await?;

    let _ = state
        .player_sync_tx
        .send(PlayerSyncEvent::AccountUnlinked {
            discord_id: discord_id.clone(),
        })
        .await;

    tracing::info!(discord_id, steam_id = account.0, "Steam account unlinked");

    Ok(Json(json!({"success": true})))
}

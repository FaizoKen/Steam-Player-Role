use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::session::verify_session;
use crate::AppState;

#[derive(Deserialize)]
pub struct PlayersQuery {
    page: Option<i64>,
    per_page: Option<i64>,
    sort: Option<String>,
    order: Option<String>,
    search: Option<String>,
}

fn sort_column(key: &str) -> Option<&'static str> {
    match key {
        "steam_level" => Some("uc.steam_level"),
        "total_games_owned" => Some("uc.total_games_owned"),
        "country_code" => Some("uc.country_code"),
        "fetched_at" => Some("uc.fetched_at"),
        "steam_id" => Some("la.steam_id"),
        "steam_name" => Some("la.steam_name"),
        "linked_at" => Some("la.linked_at"),
        _ => None,
    }
}

pub fn render_players_page(base_url: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Steam Player Roles - Player List</title>
    <link rel="icon" href="{base_url}/favicon.ico" type="image/x-icon">
    <meta name="description" content="View verified Steam players in this Discord server.">
    <meta name="theme-color" content="#1b2838">
    <style>
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{ font-family: system-ui, -apple-system, sans-serif; max-width: 1060px; margin: 0 auto; padding: 32px 20px; background: #1b2838; color: #c7d5e0; min-height: 100vh; }}
        .header {{ margin-bottom: 24px; }}
        .header-top {{ display: flex; align-items: center; gap: 10px; margin-bottom: 6px; justify-content: space-between; }}
        .header-title {{ display: flex; align-items: center; gap: 10px; }}
        .header-top h1 {{ color: #66c0f4; font-size: 24px; }}
        .powered {{ font-size: 11px; color: #7a8a99; background: #2a475e; padding: 2px 8px; border-radius: 4px; }}
        .powered a {{ color: #66c0f4; text-decoration: none; }}
        .guild-name {{ color: #c7d5e0; font-size: 18px; font-weight: 600; }}
        .guild-label {{ color: #7a8a99; font-size: 13px; margin-top: 2px; }}

        .card {{ background: #2a475e; padding: 22px; border-radius: 10px; border: 1px solid #3a6186; }}
        .msg {{ padding: 10px 14px; border-radius: 6px; margin: 12px 0; font-size: 13px; line-height: 1.5; }}
        .msg-error {{ background: #1c0a0a; color: #fca5a5; border: 1px solid #7f1d1d; }}
        .hidden {{ display: none !important; }}

        .toolbar {{ display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 10px; margin-bottom: 16px; }}
        .search-wrap {{ position: relative; flex: 1; max-width: 340px; }}
        .search-wrap svg {{ position: absolute; left: 10px; top: 50%; transform: translateY(-50%); color: #7a8a99; pointer-events: none; }}
        .search-wrap input {{ width: 100%; padding: 8px 12px 8px 34px; font-size: 13px; border-radius: 6px; border: 1px solid #3a6186; background: #1b2838; color: #c7d5e0; font-family: inherit; transition: border-color .15s; }}
        .search-wrap input:focus {{ outline: none; border-color: #66c0f4; }}
        .search-hint {{ color: #7a8a99; font-size: 11px; margin-top: 4px; }}
        .badge {{ display: inline-flex; align-items: center; gap: 5px; padding: 4px 12px; border-radius: 20px; font-size: 12px; font-weight: 500; background: #1b2838; color: #8f9bab; border: 1px solid #3a6186; white-space: nowrap; }}

        .table-wrap {{ overflow-x: auto; }}
        table {{ width: 100%; border-collapse: collapse; font-size: 13px; }}
        th, td {{ padding: 9px 12px; text-align: left; white-space: nowrap; }}
        th {{ color: #7a8a99; font-weight: 600; font-size: 11px; text-transform: uppercase; letter-spacing: 0.5px; border-bottom: 2px solid #3a6186; cursor: pointer; user-select: none; transition: color .15s; }}
        th:hover {{ color: #c7d5e0; }}
        th.sorted-asc::after {{ content: ' \25B2'; font-size: 9px; }}
        th.sorted-desc::after {{ content: ' \25BC'; font-size: 9px; }}
        td {{ border-bottom: 1px solid #1b2838; }}
        tr:hover td {{ background: #1b283880; }}
        .col-name {{ color: #c7d5e0; font-weight: 500; }}
        .col-steamid {{ font-family: 'Courier New', monospace; }}
        .col-steamid a {{ color: #66c0f4; text-decoration: none; }}
        .col-steamid a:hover {{ text-decoration: underline; }}
        .col-num {{ color: #66c0f4; text-align: right; }}
        th.col-num {{ text-align: right; }}
        .col-date {{ color: #7a8a99; font-size: 12px; }}

        .empty-state {{ text-align: center; padding: 40px 20px; color: #7a8a99; }}
        .empty-state p {{ font-size: 14px; margin-bottom: 4px; }}
        .empty-state .hint {{ font-size: 12px; }}

        .pagination {{ display: flex; align-items: center; justify-content: center; gap: 8px; margin-top: 16px; font-size: 13px; }}
        .pagination button {{ padding: 6px 14px; border-radius: 6px; border: 1px solid #3a6186; background: #1b2838; color: #c7d5e0; cursor: pointer; font-family: inherit; font-size: 13px; transition: all .15s; }}
        .pagination button:hover:not(:disabled) {{ background: #2a475e; border-color: #66c0f4; }}
        .pagination button:disabled {{ opacity: 0.3; cursor: not-allowed; }}
        .pagination .page-info {{ color: #7a8a99; }}

        .logout-btn {{ padding: 6px 14px; border-radius: 6px; border: 1px solid #3a6186; background: #1b2838; color: #c7d5e0; cursor: pointer; font-family: inherit; font-size: 12px; transition: all .15s; }}
        .logout-btn:hover {{ background: #2a475e; border-color: #ef4444; color: #fca5a5; }}

        .login-btn {{ display: inline-block; padding: 10px 22px; border-radius: 6px; background: #5865f2; color: #fff; text-decoration: none; font-weight: 600; font-size: 14px; font-family: inherit; transition: background .15s; }}
        .login-btn:hover {{ background: #4752c4; }}
    </style>
</head>
<body>
    <div class="header">
        <div class="header-top">
            <div class="header-title">
                <h1>Steam Player Roles</h1>
                <span class="powered">Powered by <a href="https://rolelogic.faizo.net" target="_blank" rel="noopener">RoleLogic</a></span>
            </div>
            <form id="logout-form" class="logout-form" method="POST" action="/auth/logout">
                <button type="submit" class="logout-btn">Logout</button>
            </form>
        </div>
        <p class="guild-name" id="guild-name">Verified Players</p>
        <p class="guild-label" id="guild-label">Loading guild info...</p>
    </div>

    <div id="loading" class="card"><p style="color:#7a8a99;">Loading player data...</p></div>
    <div id="error-msg" class="hidden"></div>

    <div id="login-prompt" class="card hidden" style="text-align:center;">
        <p style="color:#c7d5e0; font-size:15px; margin-bottom:6px;">You are not signed in.</p>
        <p style="color:#7a8a99; font-size:13px; margin-bottom:18px;">Sign in with Discord to view this server's verified Steam players.</p>
        <a id="login-link" class="login-btn" href="#">Login with Discord</a>
    </div>

    <div id="content" class="hidden">
        <div class="card">
            <div class="toolbar">
                <div>
                    <div class="search-wrap">
                        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>
                        <input type="text" id="search" placeholder="Search players..." />
                    </div>
                    <p class="search-hint">Search by Steam name, Steam ID, or Discord ID</p>
                </div>
                <span class="badge" id="player-count"></span>
            </div>
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th data-key="steam_name">Steam Name</th>
                            <th data-key="steam_id">Steam ID</th>
                            <th data-key="steam_level" class="col-num">Level</th>
                            <th data-key="total_games_owned" class="col-num">Games</th>
                            <th data-key="country_code">Country</th>
                            <th data-key="fetched_at">Last Updated</th>
                        </tr>
                    </thead>
                    <tbody id="tbody"></tbody>
                </table>
            </div>
            <div id="empty-state" class="empty-state hidden">
                <p>No players found</p>
                <p class="hint" id="empty-hint">Try a different search term</p>
            </div>
            <div class="pagination" id="pagination">
                <button id="btn-prev" onclick="goPage(state.page-1)">Prev</button>
                <span class="page-info" id="page-info"></span>
                <button id="btn-next" onclick="goPage(state.page+1)">Next</button>
            </div>
        </div>
    </div>

    <script>
    const parts = window.location.pathname.split('/').filter(Boolean);
    const guildId = parts[parts.indexOf('players') + 1] || '';
    const PER_PAGE = 20;
    const NUM_COLS = ['steam_level','total_games_owned'];

    (function setupAuthLinks() {{
        const returnTo = window.location.pathname + window.location.search;
        const form = document.getElementById('logout-form');
        if (form) form.action = '/auth/logout?return_to=' + encodeURIComponent(returnTo);
        const loginLink = document.getElementById('login-link');
        if (loginLink) loginLink.href = '/auth/login?return_to=' + encodeURIComponent(returnTo);
    }})();

    const state = {{ page: 1, sort: 'steam_level', order: 'desc', search: '', total: 0 }};
    let debounceTimer = null;

    function timeAgo(iso) {{
        if (!iso) return '-';
        const diff = Date.now() - new Date(iso).getTime();
        const mins = Math.floor(diff / 60000);
        if (mins < 1) return 'just now';
        if (mins < 60) return mins + 'm ago';
        const hrs = Math.floor(mins / 60);
        if (hrs < 24) return hrs + 'h ago';
        const days = Math.floor(hrs / 24);
        return days + 'd ago';
    }}

    function esc(s) {{
        const d = document.createElement('div');
        d.textContent = s;
        return d.innerHTML;
    }}

    function render(players) {{
        const tbody = document.getElementById('tbody');
        const emptyEl = document.getElementById('empty-state');
        tbody.innerHTML = '';
        if (players.length === 0) {{
            emptyEl.classList.remove('hidden');
            document.getElementById('empty-hint').textContent = state.search
                ? 'No results for "' + state.search + '"'
                : 'No verified players in this guild yet';
        }} else {{
            emptyEl.classList.add('hidden');
        }}
        players.forEach(p => {{
            const tr = document.createElement('tr');
            tr.innerHTML =
                '<td class="col-name">' + esc(p.steam_name || '-') + '</td>' +
                '<td class="col-steamid"><a href="https://steamcommunity.com/profiles/' + esc(p.steam_id) + '" target="_blank" rel="noopener">' + esc(p.steam_id) + '</a></td>' +
                '<td class="col-num">' + (p.steam_level || 0) + '</td>' +
                '<td class="col-num">' + (p.total_games_owned || 0) + '</td>' +
                '<td>' + esc(p.country_code || '-') + '</td>' +
                '<td class="col-date">' + timeAgo(p.fetched_at) + '</td>';
            tbody.appendChild(tr);
        }});
    }}

    function updatePagination() {{
        const totalPages = Math.max(1, Math.ceil(state.total / PER_PAGE));
        document.getElementById('player-count').textContent = state.total + ' player' + (state.total !== 1 ? 's' : '');
        document.getElementById('page-info').textContent = 'Page ' + state.page + ' of ' + totalPages;
        document.getElementById('btn-prev').disabled = state.page <= 1;
        document.getElementById('btn-next').disabled = state.page >= totalPages;
        document.getElementById('pagination').classList.toggle('hidden', state.total <= PER_PAGE);
    }}

    function updateSortUI() {{
        document.querySelectorAll('th[data-key]').forEach(h => {{
            h.classList.remove('sorted-asc', 'sorted-desc');
            if (h.dataset.key === state.sort) h.classList.add('sorted-' + state.order);
        }});
    }}

    async function fetchData() {{
        const params = new URLSearchParams({{
            page: state.page, per_page: PER_PAGE,
            sort: state.sort, order: state.order
        }});
        if (state.search) params.set('search', state.search);
        const res = await fetch('{base_url}/players/' + encodeURIComponent(guildId) + '/data?' + params, {{ credentials: 'same-origin' }});
        if (res.status === 401) {{
            const data = await res.json().catch(() => ({{}}));
            const err = new Error(data.error || 'You are not signed in.');
            err.authRequired = true;
            throw err;
        }}
        if (!res.ok) {{
            const data = await res.json().catch(() => ({{}}));
            throw new Error(data.error || 'Failed to load player data');
        }}
        return res.json();
    }}

    async function load() {{
        try {{
            const data = await fetchData();
            state.total = data.total;
            if (data.guild_name) {{
                document.getElementById('guild-name').textContent = data.guild_name;
                document.getElementById('guild-label').textContent = 'Verified Steam players';
                document.title = data.guild_name + ' - Steam Player Roles';
            }} else {{
                document.getElementById('guild-name').textContent = 'Verified Players';
                document.getElementById('guild-label').textContent = 'Steam player list';
            }}
            render(data.players);
            updatePagination();
            updateSortUI();
            document.getElementById('loading').classList.add('hidden');
            document.getElementById('content').classList.remove('hidden');
            document.getElementById('error-msg').classList.add('hidden');
        }} catch (e) {{
            document.getElementById('loading').classList.add('hidden');
            if (e && e.authRequired) {{
                document.getElementById('login-prompt').classList.remove('hidden');
                document.getElementById('error-msg').classList.add('hidden');
                document.getElementById('content').classList.add('hidden');
                const form = document.getElementById('logout-form');
                if (form) form.classList.add('hidden');
                document.getElementById('guild-name').textContent = 'Verified Players';
                document.getElementById('guild-label').textContent = 'Sign in to view this server\'s player list';
            }} else {{
                document.getElementById('guild-name').textContent = 'Verified Players';
                document.getElementById('guild-label').textContent = '';
                const el = document.getElementById('error-msg');
                el.className = 'msg msg-error';
                el.textContent = e.message;
                el.classList.remove('hidden');
            }}
        }}
    }}

    function goPage(p) {{
        const totalPages = Math.max(1, Math.ceil(state.total / PER_PAGE));
        state.page = Math.max(1, Math.min(p, totalPages));
        load();
    }}

    document.querySelectorAll('th[data-key]').forEach(th => {{
        th.addEventListener('click', () => {{
            const key = th.dataset.key;
            if (state.sort === key) {{
                state.order = state.order === 'asc' ? 'desc' : 'asc';
            }} else {{
                state.sort = key;
                state.order = NUM_COLS.includes(key) ? 'desc' : 'asc';
            }}
            state.page = 1;
            load();
        }});
    }});

    document.getElementById('search').addEventListener('input', e => {{
        clearTimeout(debounceTimer);
        debounceTimer = setTimeout(() => {{
            state.search = e.target.value.trim();
            state.page = 1;
            load();
        }}, 300);
    }});

    load();
    </script>
</body>
</html>"##
    )
}

pub async fn players_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        state.players_html.clone(),
    )
}

async fn auth_gateway_get(
    state: &Arc<AppState>,
    path_and_query: &str,
    session_cookie_value: &str,
) -> Result<Value, AppError> {
    let url = format!("{}{path_and_query}", state.config.auth_gateway_url);

    let outgoing = axum_extra::extract::cookie::Cookie::build((
        "rl_session",
        session_cookie_value.to_string(),
    ))
    .build();
    let cookie_header = outgoing.encoded().to_string();

    let resp = state
        .http
        .get(&url)
        .header(axum::http::header::COOKIE, cookie_header)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, url = %url, "Auth Gateway request failed");
            AppError::Internal(format!("Auth Gateway unreachable: {e}"))
        })?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AppError::UnauthorizedWith(
            "Session rejected by Auth Gateway. Please re-login.".into(),
        ));
    }
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "Auth Gateway returned {status}: {body_text}"
        )));
    }

    resp.json::<Value>().await.map_err(|e| {
        AppError::Internal(format!("Auth Gateway parse error: {e}"))
    })
}

async fn fetch_guild_permission(
    state: &Arc<AppState>,
    guild_id: &str,
    session_cookie_value: &str,
) -> Result<(bool, bool), AppError> {
    let path = format!(
        "/auth/guild_permission?guild_id={}",
        urlencoding::encode(guild_id)
    );
    let body = auth_gateway_get(state, &path, session_cookie_value).await?;

    let is_member = body.get("is_member").and_then(|v| v.as_bool()).unwrap_or(false);
    let is_manager = body.get("is_manager").and_then(|v| v.as_bool()).unwrap_or(false);

    Ok((is_member, is_manager))
}

async fn fetch_guild_members(
    state: &Arc<AppState>,
    guild_id: &str,
    session_cookie_value: &str,
) -> Result<(Vec<String>, Option<String>), AppError> {
    let path = format!(
        "/auth/guild_members?guild_id={}",
        urlencoding::encode(guild_id)
    );
    let body = auth_gateway_get(state, &path, session_cookie_value).await?;

    let discord_ids: Vec<String> = body
        .get("discord_ids")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let guild_name = body.get("guild_name").and_then(|v| v.as_str()).map(String::from);

    Ok((discord_ids, guild_name))
}

pub async fn players_data(
    State(state): State<Arc<AppState>>,
    Path(guild_id): Path<String>,
    jar: CookieJar,
    Query(query): Query<PlayersQuery>,
) -> Result<Json<Value>, AppError> {
    let session_cookie = jar.get("rl_session").ok_or_else(|| {
        AppError::UnauthorizedWith("No session cookie found. Please log in.".into())
    })?;

    let cookie_value = session_cookie.value();

    let (_viewer_discord_id, _) =
        verify_session(cookie_value, &state.config.session_secret).ok_or_else(|| {
            AppError::UnauthorizedWith("Session verification failed. Please re-login.".into())
        })?;

    // Check guild settings
    let guild_row: Option<(bool, String)> = sqlx::query_as(
        "SELECT \
           EXISTS(SELECT 1 FROM role_links WHERE guild_id = $1) AS has_link, \
           COALESCE( \
             (SELECT view_permission FROM guild_settings WHERE guild_id = $1), \
             'members' \
           ) AS view_permission",
    )
    .bind(&guild_id)
    .fetch_optional(&state.pool)
    .await?;

    let (has_link, view_permission) = guild_row.unwrap_or((false, "members".to_string()));
    if !has_link {
        return Err(AppError::NotFound(
            "No player list is configured for this server.".into(),
        ));
    }
    let members_allowed = view_permission == "members";

    let (_, is_manager) =
        fetch_guild_permission(&state, &guild_id, session_cookie.value()).await?;

    let (member_ids, ag_guild_name) =
        fetch_guild_members(&state, &guild_id, session_cookie.value()).await?;

    if member_ids.is_empty() {
        return Err(AppError::Forbidden(
            "You must be a member of this server to view its player list.".into(),
        ));
    }

    if !members_allowed && !is_manager {
        return Err(AppError::Forbidden(
            "Only server managers can view this player list.".into(),
        ));
    }

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * per_page;

    let order_col = query
        .sort
        .as_deref()
        .and_then(sort_column)
        .unwrap_or("uc.steam_level");
    let order_dir = match query.order.as_deref() {
        Some("asc") => "ASC",
        _ => "DESC",
    };

    let search = query.search.as_deref().unwrap_or("").trim();
    let has_search = !search.is_empty();
    let search_pattern = format!("%{search}%");

    let sql = format!(
        "SELECT la.steam_id, \
                la.steam_name, \
                la.discord_id, \
                uc.steam_level, \
                uc.total_games_owned, \
                uc.country_code, \
                uc.fetched_at, \
                COUNT(*) OVER() AS total_count \
         FROM linked_accounts la \
         JOIN user_cache uc ON uc.steam_id = la.steam_id \
         WHERE la.discord_id = ANY($1) {search_clause} \
         ORDER BY {order_col} {order_dir} NULLS LAST \
         LIMIT $2 OFFSET $3",
        search_clause = if has_search {
            "AND (la.steam_name ILIKE $4 \
             OR la.steam_id ILIKE $4 \
             OR la.discord_id ILIKE $4)"
        } else {
            ""
        },
        order_col = order_col,
        order_dir = order_dir,
    );

    use sqlx::Row;
    let rows = if has_search {
        sqlx::query(&sql)
            .bind(&member_ids)
            .bind(per_page)
            .bind(offset)
            .bind(&search_pattern)
            .fetch_all(&state.pool)
            .await?
    } else {
        sqlx::query(&sql)
            .bind(&member_ids)
            .bind(per_page)
            .bind(offset)
            .fetch_all(&state.pool)
            .await?
    };

    let total: i64 = rows.first().map(|r| r.get("total_count")).unwrap_or(0);

    let players: Vec<Value> = rows
        .iter()
        .map(|r| {
            let fetched_at: chrono::DateTime<chrono::Utc> = r.get("fetched_at");
            json!({
                "steam_id": r.get::<String, _>("steam_id"),
                "steam_name": r.get::<Option<String>, _>("steam_name"),
                "discord_id": r.get::<String, _>("discord_id"),
                "steam_level": r.get::<i32, _>("steam_level"),
                "total_games_owned": r.get::<i32, _>("total_games_owned"),
                "country_code": r.get::<String, _>("country_code"),
                "fetched_at": fetched_at,
            })
        })
        .collect();

    Ok(Json(json!({
        "players": players,
        "total": total,
        "page": page,
        "per_page": per_page,
        "guild_name": ag_guild_name,
    })))
}

//! Background refresh worker.
//!
//! Re-fetches each linked user's Steam data to keep roles in sync, within the
//! Web API key's daily call allowance. Every Steam call passes through the
//! [crate::services::quota::QuotaGovernor] (inside the API client), which
//! paces background spend smoothly across the UTC quota-day and reserves
//! headroom for interactive link-time calls. Design highlights:
//!
//! - **Batching.** Users are claimed in batches of up to 100 so
//!   GetPlayerSummaries and GetPlayerBans cost 1 call per 100 users instead
//!   of 2 per user.
//! - **Condition-driven fetching.** GetOwnedGames / GetSteamLevel /
//!   GetUserGroupList are only called when some role link's conditions
//!   actually reference that data; achievements and publisher-ownership
//!   checks are further scoped to the guilds the user is actually in. Most
//!   deployments drop from 5+ calls per user to ~1.
//! - **Adaptive cadence.** Steam data is stable for most users, so a user
//!   whose eval-relevant snapshot hasn't changed earns an exponentially
//!   longer interval (bounded by MAX_STABLE_REFRESH_SECS), and one that just
//!   changed is re-checked on the base cadence. This concentrates scarce
//!   quota on churn — the single biggest multiplier on effective capacity.
//! - **No thundering herd.** When the daily budget is spent the worker
//!   pauses and serves from cache; rows blocked mid-flight are requeued past
//!   the retry point with per-row jitter, never mass-stamped to one instant.
//! - **Horizontally scalable.** Rows are claimed with `FOR UPDATE SKIP
//!   LOCKED`, leased, and partitioned by `hashtext(steam_id) % N`, so N
//!   workers never double-process.
//! - **Accuracy first.** Cache columns are only written from definite API
//!   answers (unfetched classes are preserved via COALESCE); quota/network
//!   failures back the row off and never wipe data, so a role is never
//!   stripped because we couldn't check.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::models::condition::{Condition, ConditionField};
use crate::services::auth_gateway;
use crate::services::quota::Class;
use crate::services::steam_api::{
    OwnedGame, OwnedGamesResult, PlayerBan, PlayerSummary, SteamGroup, SUMMARY_BATCH_MAX,
};
use crate::services::sync::PlayerSyncEvent;
use crate::AppState;

const MIN_REFRESH_SECS: i64 = 1800; // 30 min floor
const MAX_REFRESH_SECS: i64 = 86400; // 24 hour cap for churny data
const INTERVAL_CACHE_SECS: u64 = 300;
const NEEDS_CACHE_SECS: u64 = 60;

/// Inactive users (no role_assignments) are refreshed this many times slower.
const INACTIVE_MULTIPLIER: i64 = 6;

/// Users claimed per cycle; also the GetPlayerSummaries/Bans batch size.
const CLAIM_BATCH: i64 = SUMMARY_BATCH_MAX as i64;

/// Lease applied to claimed rows so a crash mid-batch re-surfaces them after
/// this long instead of stranding them, and concurrent workers skip them
/// meanwhile. Sized for a worst-case fully-paced 100-user batch.
const LEASE_SECS: f64 = 1800.0;

/// Idle nap when there's nothing due.
const IDLE_SLEEP_SECS: u64 = 5;

/// Longest the worker sleeps in one go when paused on quota, so it
/// re-evaluates periodically (e.g. picks up a raised quota).
const MAX_PAUSE_SECS: u64 = 900;

/// A user linked within this window gets Interactive-class calls for their
/// first refresh — they're actively waiting on the verify page for roles.
const FRESH_LINK_WINDOW_SECS: i64 = 600;

/// Stability multiplier on the base interval: 1,1,2,2,4,4,8,8,16… capped at
/// 16×. A long unchanged streak means the data is stable and rarely needs
/// re-confirming, so we stretch its interval and reclaim that quota for churn.
fn stability_factor(streak: i32) -> i64 {
    1i64 << (streak / 2).clamp(0, 4)
}

/// Next refresh interval for a user. Churny rows (low streak) stay within
/// MAX_REFRESH_SECS; long-stable rows may stretch up to `max_stable`.
fn compute_interval(base: i64, is_active: bool, streak: i32, max_stable: i64) -> i64 {
    let activity = if is_active { 1 } else { INACTIVE_MULTIPLIER };
    let raw = (base * activity).clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS);
    (raw * stability_factor(streak)).min(max_stable.max(MAX_REFRESH_SECS))
}

// ── Global fetch needs (derived from role-link conditions) ─────────────────

/// Which Steam data classes any role link's conditions actually reference.
/// Data nobody conditions on is never fetched — the biggest per-user saving.
#[derive(Default)]
struct RefreshNeeds {
    need_games: bool,
    need_level: bool,
    need_groups: bool,
    /// app_id → guilds whose links condition on that app's achievements.
    achievement_apps: HashMap<String, HashSet<String>>,
    /// (app_id, publisher_key) → guilds whose links own that pair.
    ownership_pairs: HashMap<(String, String), HashSet<String>>,
}

impl RefreshNeeds {
    fn from_rows(rows: &[(String, Vec<Condition>, Option<String>)]) -> Self {
        let mut needs = Self::default();
        for (guild_id, conditions, publisher_key) in rows {
            for c in conditions {
                match c.field {
                    ConditionField::OwnsGame => {
                        needs.need_games = true;
                        if let (Some(app_id), Some(key)) = (&c.app_id, publisher_key) {
                            needs
                                .ownership_pairs
                                .entry((app_id.clone(), key.clone()))
                                .or_default()
                                .insert(guild_id.clone());
                        }
                    }
                    ConditionField::GamePlaytime
                    | ConditionField::RecentPlaytime
                    | ConditionField::TotalGamesOwned => needs.need_games = true,
                    ConditionField::SteamLevel => needs.need_level = true,
                    ConditionField::InGroup => needs.need_groups = true,
                    ConditionField::AchievementCount
                    | ConditionField::AchievementPercent
                    | ConditionField::HasAchievement => {
                        if let Some(app_id) = &c.app_id {
                            needs
                                .achievement_apps
                                .entry(app_id.clone())
                                .or_default()
                                .insert(guild_id.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        needs
    }

    fn needs_guild_scope(&self) -> bool {
        !self.achievement_apps.is_empty() || !self.ownership_pairs.is_empty()
    }

    /// Rough Steam calls per user refresh, used only to size the target
    /// cadence — the governor is the hard spend ceiling. The +1 covers the
    /// amortized batched summary/ban calls plus achievement spread.
    fn estimated_cost_per_user(&self) -> i64 {
        (self.need_games as i64 + self.need_level as i64 + self.need_groups as i64 + 1).max(1)
    }
}

/// 60s-TTL cache of RefreshNeeds. On query failure the previous value is
/// kept, so a transient DB error can't flip fetch classes (and reset every
/// stability streak via the hash shape).
struct NeedsCache {
    inner: Mutex<(Instant, Arc<RefreshNeeds>)>,
}

impl NeedsCache {
    fn new() -> Self {
        Self {
            inner: Mutex::new((
                Instant::now() - StdDuration::from_secs(NEEDS_CACHE_SECS + 1),
                Arc::new(RefreshNeeds::default()),
            )),
        }
    }

    async fn get(&self, pool: &sqlx::PgPool) -> Arc<RefreshNeeds> {
        let mut guard = self.inner.lock().await;
        if guard.0.elapsed() >= StdDuration::from_secs(NEEDS_CACHE_SECS) {
            match sqlx::query_as::<_, (String, sqlx::types::Json<Vec<Condition>>, Option<String>)>(
                "SELECT guild_id, conditions, publisher_key FROM role_links",
            )
            .fetch_all(pool)
            .await
            {
                Ok(rows) => {
                    let rows: Vec<(String, Vec<Condition>, Option<String>)> = rows
                        .into_iter()
                        .map(|(g, c, k)| (g, c.0, k))
                        .collect();
                    *guard = (Instant::now(), Arc::new(RefreshNeeds::from_rows(&rows)));
                }
                Err(e) => {
                    tracing::error!("Failed to load role-link needs: {e}");
                    guard.0 = Instant::now();
                }
            }
        }
        Arc::clone(&guard.1)
    }
}

// ── Base interval (target freshness) ───────────────────────────────────────

/// Caches the base refresh interval so we don't size it every cycle. Uses
/// Postgres' `reltuples` estimate instead of a full `COUNT(*)` so it stays
/// cheap at millions of rows — exactness doesn't matter because the quota
/// governor, not this number, is the hard spend ceiling; this only sets the
/// target freshness.
struct CachedInterval {
    value: AtomicI64,
    daily_quota: i64,
    last_computed: Mutex<Instant>,
}

impl CachedInterval {
    fn new(daily_quota: i64) -> Self {
        Self {
            value: AtomicI64::new(MIN_REFRESH_SECS),
            daily_quota: daily_quota.max(1),
            last_computed: Mutex::new(
                Instant::now() - StdDuration::from_secs(INTERVAL_CACHE_SECS + 1),
            ),
        }
    }

    async fn get(&self, pool: &sqlx::PgPool, cost_per_user: i64) -> i64 {
        let mut last = self.last_computed.lock().await;
        if last.elapsed() >= StdDuration::from_secs(INTERVAL_CACHE_SECS) {
            let est: i64 = sqlx::query_scalar(
                "SELECT GREATEST(reltuples, 0)::bigint FROM pg_class WHERE relname = 'user_cache'",
            )
            .fetch_one(pool)
            .await
            .unwrap_or(0);

            let interval = if est == 0 {
                MIN_REFRESH_SECS
            } else {
                ((est * 86400 * cost_per_user.max(1)) / self.daily_quota)
                    .clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS)
            };
            self.value.store(interval, Ordering::Relaxed);
            *last = Instant::now();
        }
        self.value.load(Ordering::Relaxed)
    }
}

// ── Stability hash ─────────────────────────────────────────────────────────

/// Deterministic digest of the eval-relevant snapshot. Only *fetched* classes
/// are included (a class that isn't fetched can't change in the DB either);
/// `None` means "not fetched this round" and hashes as a distinct marker so a
/// needs change resets streaks once instead of comparing unlike shapes.
/// Playtimes are bucketed to hours so minute-level drift doesn't count as
/// churn, while real play sessions do.
fn compute_data_hash(
    games: Option<&[OwnedGame]>,
    game_count: Option<i64>,
    library_visible: Option<bool>,
    level: Option<i32>,
    groups: Option<&[SteamGroup]>,
    vac_banned: bool,
    game_banned: bool,
    country: &str,
) -> String {
    let mut input = String::from("v1\n");

    match games {
        Some(games) => {
            let mut parts: Vec<String> = games
                .iter()
                .map(|g| {
                    format!(
                        "{}:{}:{}",
                        g.appid,
                        g.playtime_forever.unwrap_or(0) / 60,
                        g.playtime_2weeks.unwrap_or(0) / 60
                    )
                })
                .collect();
            parts.sort();
            input.push_str("games=");
            input.push_str(&parts.join(","));
        }
        None => input.push_str("games=skip"),
    }
    input.push('\n');

    match game_count {
        Some(n) => input.push_str(&format!("count={n}")),
        None => input.push_str("count=skip"),
    }
    input.push('\n');

    match library_visible {
        Some(v) => input.push_str(&format!("libvis={v}")),
        None => input.push_str("libvis=skip"),
    }
    input.push('\n');

    match level {
        Some(l) => input.push_str(&format!("level={l}")),
        None => input.push_str("level=skip"),
    }
    input.push('\n');

    match groups {
        Some(groups) => {
            let mut gids: Vec<&str> = groups.iter().map(|g| g.gid.as_str()).collect();
            gids.sort();
            input.push_str("groups=");
            input.push_str(&gids.join(","));
        }
        None => input.push_str("groups=skip"),
    }
    input.push('\n');

    input.push_str(&format!("bans={vac_banned}:{game_banned}\ncountry={country}\n"));

    hex::encode(Sha256::digest(input.as_bytes()))
}

// ── Worker ─────────────────────────────────────────────────────────────────

struct BatchUser {
    steam_id: String,
    discord_id: String,
    linked_at: chrono::DateTime<chrono::Utc>,
    is_active: bool,
    prev_streak: i32,
    prev_hash: String,
}

pub async fn run(state: Arc<AppState>, worker_id: i64, total_workers: i64) {
    tracing::info!(
        daily_quota = state.config.steam_api_daily_quota,
        worker_id,
        total_workers,
        "Refresh worker started"
    );

    let cached_interval = CachedInterval::new(state.config.steam_api_daily_quota);
    let needs_cache = NeedsCache::new();

    loop {
        // Budget triage. Fully paused when throttled or everything is spent —
        // serve from cache until the UTC reset; this is the anti-herd
        // keystone: nothing is stamped, nothing floods at rollover. When only
        // the background pool is spent, keep serving *user-initiated* rows
        // (fresh links / Re-checks, marked with next_fetch_at = epoch) from
        // the interactive reserve.
        let snap = state.quota.snapshot().await;
        if snap.throttled || snap.used >= snap.total_budget {
            let nap = (snap.reset_in_secs as u64).clamp(IDLE_SLEEP_SECS, MAX_PAUSE_SECS);
            tracing::warn!(
                used = snap.used,
                total_budget = snap.total_budget,
                reset_in_secs = snap.reset_in_secs,
                throttled = snap.throttled,
                "Steam quota unavailable; pausing all refreshes (serving from cache)"
            );
            tokio::time::sleep(StdDuration::from_secs(nap)).await;
            continue;
        }
        let interactive_only = snap.used >= snap.background_budget;

        let needs = needs_cache.get(&state.pool).await;
        let worked = process_batch(
            &state,
            &needs,
            &cached_interval,
            worker_id,
            total_workers,
            interactive_only,
        )
        .await;
        if !worked {
            tokio::time::sleep(StdDuration::from_secs(IDLE_SLEEP_SECS)).await;
        }
    }
}

/// Claim and refresh one batch of due users. Returns true if any rows were
/// claimed (whether or not every user succeeded), so the caller keeps
/// cycling; false means nothing was due.
///
/// `interactive_only` restricts claims to user-initiated rows (fresh links /
/// Re-checks queued at epoch) and serves them from the interactive reserve —
/// used when the background budget is spent for the day.
async fn process_batch(
    state: &Arc<AppState>,
    needs: &RefreshNeeds,
    cached_interval: &CachedInterval,
    worker_id: i64,
    total_workers: i64,
    interactive_only: bool,
) -> bool {
    // Claim with a lease: the provisional next_fetch_at keeps other workers
    // off these rows and re-surfaces them if we crash mid-batch. Success or
    // backoff overwrites it. The JOIN skips orphaned cache rows.
    let claimed = sqlx::query_as::<_, (String, i32, String)>(
        "WITH claimed AS ( \
            SELECT uc.steam_id FROM user_cache uc \
            JOIN linked_accounts la ON la.steam_id = uc.steam_id \
            WHERE uc.next_fetch_at <= now() \
              AND ($5::bool = false OR uc.next_fetch_at <= to_timestamp(1)) \
              AND ($2 = 1 OR abs(hashtext(uc.steam_id)::bigint) % $2 = $3) \
            ORDER BY uc.next_fetch_at ASC \
            LIMIT $4 \
            FOR UPDATE OF uc SKIP LOCKED \
         ) \
         UPDATE user_cache c SET next_fetch_at = now() + make_interval(secs => $1) \
         FROM claimed WHERE c.steam_id = claimed.steam_id \
         RETURNING c.steam_id, c.stable_streak, c.data_hash",
    )
    .bind(LEASE_SECS)
    .bind(total_workers)
    .bind(worker_id)
    .bind(CLAIM_BATCH)
    .bind(interactive_only)
    .fetch_all(&state.pool)
    .await;

    let claimed = match claimed {
        Ok(rows) if rows.is_empty() => return false,
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("Refresh claim failed: {e}");
            tokio::time::sleep(StdDuration::from_secs(5)).await;
            return true;
        }
    };

    // Owner + activity for every claimed row in one query. Rows whose link
    // vanished between claim and here are skipped; their cache row is dead
    // weight until relink and the lease keeps them out of the scan meanwhile.
    let steam_ids: Vec<String> = claimed.iter().map(|(sid, _, _)| sid.clone()).collect();
    let info_rows = sqlx::query_as::<_, (String, String, chrono::DateTime<chrono::Utc>, bool)>(
        "SELECT la.steam_id, la.discord_id, la.linked_at, \
         EXISTS(SELECT 1 FROM role_assignments ra WHERE ra.discord_id = la.discord_id) AS is_active \
         FROM linked_accounts la WHERE la.steam_id = ANY($1)",
    )
    .bind(&steam_ids)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let info: HashMap<String, (String, chrono::DateTime<chrono::Utc>, bool)> = info_rows
        .into_iter()
        .map(|(sid, did, linked, active)| (sid, (did, linked, active)))
        .collect();

    let users: Vec<BatchUser> = claimed
        .into_iter()
        .filter_map(|(steam_id, prev_streak, prev_hash)| {
            info.get(&steam_id).map(|(did, linked, active)| BatchUser {
                steam_id,
                discord_id: did.clone(),
                linked_at: *linked,
                is_active: *active,
                prev_streak,
                prev_hash,
            })
        })
        .collect();

    if users.is_empty() {
        return true;
    }

    // Batched profile + ban fetch: 1 quota unit per 100 users each.
    let batch_class = if interactive_only {
        Class::Interactive
    } else {
        Class::Background
    };
    let id_refs: Vec<&str> = users.iter().map(|u| u.steam_id.as_str()).collect();
    let mut profiles: HashMap<String, PlayerSummary> = HashMap::new();
    let mut bans: HashMap<String, PlayerBan> = HashMap::new();
    for chunk in id_refs.chunks(SUMMARY_BATCH_MAX) {
        match state
            .steam_client
            .get_player_summaries(chunk, batch_class)
            .await
        {
            Ok(list) => profiles.extend(list.into_iter().map(|p| (p.steamid.clone(), p))),
            Err(AppError::QuotaExhausted { retry_after_secs }) => {
                requeue_with_jitter(&state.pool, &steam_ids, retry_after_secs).await;
                tokio::time::sleep(pause_dur(retry_after_secs)).await;
                return true;
            }
            Err(e) => {
                tracing::warn!(count = steam_ids.len(), "Batched summaries failed: {e}");
                backoff_users(&state.pool, &steam_ids).await;
                return true;
            }
        }
        match state.steam_client.get_player_bans(chunk, batch_class).await {
            Ok(list) => bans.extend(list.into_iter().map(|b| (b.steam_id.clone(), b))),
            Err(AppError::QuotaExhausted { retry_after_secs }) => {
                requeue_with_jitter(&state.pool, &steam_ids, retry_after_secs).await;
                tokio::time::sleep(pause_dur(retry_after_secs)).await;
                return true;
            }
            Err(e) => {
                tracing::warn!(count = steam_ids.len(), "Batched bans failed: {e}");
                backoff_users(&state.pool, &steam_ids).await;
                return true;
            }
        }
    }

    // Guild scoping for achievements/ownership: role_assignments as the
    // always-available floor, topped up per user from the gateway below.
    let mut assignment_guilds: HashMap<String, HashSet<String>> = HashMap::new();
    if needs.needs_guild_scope() {
        let discord_ids: Vec<String> = users.iter().map(|u| u.discord_id.clone()).collect();
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT DISTINCT discord_id, guild_id FROM role_assignments WHERE discord_id = ANY($1)",
        )
        .bind(&discord_ids)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
        for (did, gid) in rows {
            assignment_guilds.entry(did).or_default().insert(gid);
        }
    }

    let base = cached_interval
        .get(&state.pool, needs.estimated_cost_per_user())
        .await;

    for (idx, user) in users.iter().enumerate() {
        let profile = profiles.get(&user.steam_id);
        let ban = bans.get(&user.steam_id);

        match refresh_user(
            state,
            needs,
            user,
            profile,
            ban,
            &assignment_guilds,
            base,
            interactive_only,
        )
        .await
        {
            Ok(()) => {}
            Err(AppError::QuotaExhausted { retry_after_secs }) => {
                // Main-key budget ran out mid-batch: requeue this user and
                // everyone we haven't reached, then pause.
                let remaining: Vec<String> = users[idx..]
                    .iter()
                    .map(|u| u.steam_id.clone())
                    .collect();
                requeue_with_jitter(&state.pool, &remaining, retry_after_secs).await;
                tokio::time::sleep(pause_dur(retry_after_secs)).await;
                return true;
            }
            Err(e) => {
                tracing::warn!(steam_id = user.steam_id, "Steam refresh failed: {e}");
                backoff_users(&state.pool, std::slice::from_ref(&user.steam_id)).await;
            }
        }
    }

    true
}

/// Fetch the conditional data classes for one user and commit the refresh.
/// Steam API failures must propagate: writing defaults on error would wipe
/// the cached library/groups/level and strip roles — the caller's backoff
/// keeps the old cache instead.
#[allow(clippy::too_many_arguments)]
async fn refresh_user(
    state: &Arc<AppState>,
    needs: &RefreshNeeds,
    user: &BatchUser,
    profile: Option<&PlayerSummary>,
    ban: Option<&PlayerBan>,
    assignment_guilds: &HashMap<String, HashSet<String>>,
    base_interval: i64,
    interactive_only: bool,
) -> Result<(), AppError> {
    let client = &state.steam_client;

    // A user linked minutes ago is watching the verify page — serve their
    // first refresh from the interactive reserve, unpaced. In
    // interactive-only mode every claimed row is user-initiated.
    let fresh_link =
        (chrono::Utc::now() - user.linked_at).num_seconds() < FRESH_LINK_WINDOW_SECS;
    let class = if fresh_link || interactive_only {
        Class::Interactive
    } else {
        Class::Background
    };

    let owned: Option<OwnedGamesResult> = if needs.need_games {
        Some(client.get_owned_games(&user.steam_id, class).await?)
    } else {
        None
    };
    let level: Option<i32> = if needs.need_level {
        Some(client.get_steam_level(&user.steam_id, class).await?)
    } else {
        None
    };
    let groups: Option<Vec<SteamGroup>> = if needs.need_groups {
        Some(client.get_user_group_list(&user.steam_id, class).await?)
    } else {
        None
    };

    // Guilds this user belongs to, for achievement/ownership scoping. The
    // gateway is authoritative (covers grants the user doesn't hold yet);
    // on gateway trouble fall back to assignment guilds — old cache rows
    // keep serving, so nothing is lost but freshness.
    let user_guilds: HashSet<String> = if needs.needs_guild_scope() {
        let mut guilds = assignment_guilds
            .get(&user.discord_id)
            .cloned()
            .unwrap_or_default();
        match auth_gateway::fetch_user_guild_ids(
            &state.http,
            &state.config.auth_gateway_url,
            &state.config.internal_api_key,
            &user.discord_id,
        )
        .await
        {
            Ok(ids) => guilds.extend(ids),
            Err(e) => {
                tracing::debug!(
                    discord_id = user.discord_id,
                    "Gateway guild lookup failed, using assignment guilds: {e}"
                );
            }
        }
        guilds
    } else {
        HashSet::new()
    };

    // Achievements for apps conditioned on in the user's guilds.
    for (app_id, guilds) in &needs.achievement_apps {
        if !guilds.iter().any(|g| user_guilds.contains(g)) {
            continue;
        }
        match client
            .get_player_achievements(&user.steam_id, app_id, class)
            .await
        {
            Ok(achievements) => {
                let total = achievements.len() as i32;
                let unlocked = achievements.iter().filter(|a| a.achieved == 1).count() as i32;
                let achievements_json = serde_json::to_value(&achievements).unwrap_or_default();

                sqlx::query(
                    "INSERT INTO game_achievement_cache (steam_id, app_id, achievements, total_count, unlocked_count, fetched_at) \
                     VALUES ($1, $2, $3, $4, $5, now()) \
                     ON CONFLICT (steam_id, app_id) DO UPDATE SET \
                     achievements = $3, total_count = $4, unlocked_count = $5, fetched_at = now()",
                )
                .bind(&user.steam_id)
                .bind(app_id)
                .bind(&achievements_json)
                .bind(total)
                .bind(unlocked)
                .execute(&state.pool)
                .await?;
            }
            Err(e @ AppError::QuotaExhausted { .. }) => return Err(e),
            Err(e) => {
                tracing::warn!(
                    steam_id = user.steam_id,
                    app_id,
                    "Failed to fetch achievements: {e}"
                );
            }
        }
    }

    // Publisher-key ownership checks, scoped the same way. These draw from
    // the publisher key's own governor — its exhaustion only skips this
    // section (old cache keeps serving), never the user's whole refresh.
    for ((app_id, publisher_key), guilds) in &needs.ownership_pairs {
        if !guilds.iter().any(|g| user_guilds.contains(g)) {
            continue;
        }
        let pub_quota = state.publisher_quotas.for_key(publisher_key).await;
        match client
            .check_app_ownership(&user.steam_id, app_id, publisher_key, &pub_quota)
            .await
        {
            Ok(o) => {
                sqlx::query(
                    "INSERT INTO app_ownership_cache (steam_id, app_id, owns_app, permanent, owner_steam_id, fetched_at) \
                     VALUES ($1, $2, $3, $4, $5, now()) \
                     ON CONFLICT (steam_id, app_id) DO UPDATE SET \
                     owns_app = $3, permanent = $4, owner_steam_id = $5, fetched_at = now()",
                )
                .bind(&user.steam_id)
                .bind(app_id)
                .bind(o.owns_app)
                .bind(o.permanent)
                .bind(&o.owner_steam_id)
                .execute(&state.pool)
                .await?;
            }
            Err(e) => {
                tracing::warn!(
                    steam_id = user.steam_id,
                    app_id,
                    "CheckAppOwnership failed: {e}"
                );
            }
        }
    }

    // Profile-derived denormalized fields (profile + bans always fetched,
    // batched).
    let profile_data = serde_json::to_value(profile).unwrap_or_default();
    let ban_data = serde_json::to_value(ban).unwrap_or_default();
    let is_vac_banned = ban.map(|b| b.vac_banned).unwrap_or(false);
    let is_game_banned = ban.map(|b| b.number_of_game_bans > 0).unwrap_or(false);
    let country_code = profile
        .and_then(|p| p.loccountrycode.clone())
        .unwrap_or_default();
    let account_created = profile
        .and_then(|p| p.timecreated)
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));

    let new_hash = compute_data_hash(
        owned.as_ref().map(|o| o.games.as_slice()),
        owned.as_ref().map(|o| o.game_count),
        owned.as_ref().map(|o| o.library_visible),
        level,
        groups.as_deref(),
        is_vac_banned,
        is_game_banned,
        &country_code,
    );
    let streak = if new_hash == user.prev_hash {
        (user.prev_streak + 1).min(100)
    } else {
        0
    };
    let interval = compute_interval(
        base_interval,
        user.is_active,
        streak,
        state.config.max_stable_refresh_secs,
    );

    let owned_games_json = owned
        .as_ref()
        .map(|o| serde_json::to_value(&o.games).unwrap_or_default());
    let groups_json = groups
        .as_ref()
        .map(|g| serde_json::to_value(g).unwrap_or_default());

    // COALESCE preserves classes we deliberately didn't fetch this round.
    sqlx::query(
        "UPDATE user_cache SET \
         profile_data = $1, ban_data = $2, \
         owned_games = COALESCE($3, owned_games), \
         groups = COALESCE($4, groups), \
         steam_level = COALESCE($5, steam_level), \
         account_created = $6, \
         total_games_owned = COALESCE($7, total_games_owned), \
         is_vac_banned = $8, is_game_banned = $9, country_code = $10, \
         library_visible = COALESCE($11, library_visible), \
         stable_streak = $12, data_hash = $13, \
         fetched_at = now(), next_fetch_at = now() + make_interval(secs => $14), \
         fetch_failures = 0 \
         WHERE steam_id = $15",
    )
    .bind(&profile_data)
    .bind(&ban_data)
    .bind(&owned_games_json)
    .bind(&groups_json)
    .bind(level)
    .bind(account_created)
    .bind(owned.as_ref().map(|o| o.game_count as i32))
    .bind(is_vac_banned)
    .bind(is_game_banned)
    .bind(&country_code)
    .bind(owned.as_ref().map(|o| o.library_visible))
    .bind(streak)
    .bind(&new_hash)
    .bind(interval as f64)
    .bind(&user.steam_id)
    .execute(&state.pool)
    .await?;

    let _ = state
        .player_sync_tx
        .send(PlayerSyncEvent::PlayerUpdated {
            discord_id: user.discord_id.clone(),
        })
        .await;

    tracing::debug!(
        steam_id = user.steam_id,
        is_active = user.is_active,
        streak,
        interval,
        "Steam data refreshed"
    );

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// How long to nap after an `Exhausted`, bounded so we re-evaluate
/// periodically.
fn pause_dur(retry_after_secs: u64) -> StdDuration {
    StdDuration::from_secs(retry_after_secs.clamp(IDLE_SLEEP_SECS, MAX_PAUSE_SECS))
}

/// Requeue rows to land after `retry_after_secs` plus up to 30 min of
/// per-row jitter, so rows blocked by exhaustion spread out across the
/// retry point instead of stampeding the moment budget returns.
async fn requeue_with_jitter(pool: &sqlx::PgPool, steam_ids: &[String], retry_after_secs: u64) {
    if steam_ids.is_empty() {
        return;
    }
    if let Err(e) = sqlx::query(
        "UPDATE user_cache SET next_fetch_at = now() \
           + make_interval(secs => $1) + make_interval(secs => random() * 1800) \
         WHERE steam_id = ANY($2)",
    )
    .bind(retry_after_secs as f64)
    .bind(steam_ids)
    .execute(pool)
    .await
    {
        tracing::error!("Failed to requeue users: {e}");
    }
}

/// Exponential failure backoff (transient API/network errors).
async fn backoff_users(pool: &sqlx::PgPool, steam_ids: &[String]) {
    if steam_ids.is_empty() {
        return;
    }
    if let Err(e) = sqlx::query(
        "UPDATE user_cache SET fetch_failures = fetch_failures + 1, \
         next_fetch_at = now() + LEAST(INTERVAL '60 seconds' * POWER(2, fetch_failures), INTERVAL '1 hour') \
         WHERE steam_id = ANY($1)",
    )
    .bind(steam_ids)
    .execute(pool)
    .await
    {
        tracing::error!("Failed to back off users: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::condition::ConditionOperator;
    use serde_json::json;

    #[test]
    fn stability_curve_grows_and_caps() {
        // 1,1,2,2,4,4,8,8,16,16… — doubles every two stable checks, caps 16×.
        assert_eq!(stability_factor(0), 1);
        assert_eq!(stability_factor(1), 1);
        assert_eq!(stability_factor(2), 2);
        assert_eq!(stability_factor(3), 2);
        assert_eq!(stability_factor(4), 4);
        assert_eq!(stability_factor(6), 8);
        assert_eq!(stability_factor(8), 16);
        assert_eq!(stability_factor(100), 16); // capped
    }

    #[test]
    fn churny_user_interval_stays_within_a_day() {
        // Freshly changed data (streak 0): even inactive users cap at 24h.
        assert_eq!(
            compute_interval(MAX_REFRESH_SECS, false, 0, 604_800),
            MAX_REFRESH_SECS
        );
        assert_eq!(
            compute_interval(MIN_REFRESH_SECS, true, 0, 604_800),
            MIN_REFRESH_SECS
        );
    }

    #[test]
    fn stable_user_interval_stretches_to_the_stable_cap() {
        // Long-stable active user at a 24h base: 24h × 16 clamps to 7d.
        assert_eq!(
            compute_interval(MAX_REFRESH_SECS, true, 100, 604_800),
            604_800
        );
        // A small stable cap can never squeeze below the 24h churn cap.
        assert_eq!(
            compute_interval(MAX_REFRESH_SECS, true, 100, 1),
            MAX_REFRESH_SECS
        );
    }

    fn game(appid: i64, minutes: i64) -> OwnedGame {
        OwnedGame {
            appid,
            name: None,
            playtime_forever: Some(minutes),
            playtime_2weeks: None,
        }
    }

    #[test]
    fn data_hash_ignores_minute_level_playtime_drift() {
        let a = compute_data_hash(
            Some(&[game(730, 600)]),
            Some(1),
            Some(true),
            None,
            None,
            false,
            false,
            "US",
        );
        // 610 minutes is the same hour bucket as 600.
        let b = compute_data_hash(
            Some(&[game(730, 610)]),
            Some(1),
            Some(true),
            None,
            None,
            false,
            false,
            "US",
        );
        // 660 minutes crosses into the next hour.
        let c = compute_data_hash(
            Some(&[game(730, 660)]),
            Some(1),
            Some(true),
            None,
            None,
            false,
            false,
            "US",
        );
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn data_hash_reacts_to_library_and_shape_changes() {
        let base = compute_data_hash(
            Some(&[game(730, 600)]),
            Some(1),
            Some(true),
            None,
            None,
            false,
            false,
            "US",
        );
        // New game in the library.
        let new_game = compute_data_hash(
            Some(&[game(730, 600), game(570, 0)]),
            Some(2),
            Some(true),
            None,
            None,
            false,
            false,
            "US",
        );
        // Same data but games not fetched (needs changed shape).
        let skipped = compute_data_hash(None, None, None, None, None, false, false, "US");
        // Library flipped private.
        let private = compute_data_hash(
            Some(&[]),
            Some(0),
            Some(false),
            None,
            None,
            false,
            false,
            "US",
        );
        assert_ne!(base, new_game);
        assert_ne!(base, skipped);
        assert_ne!(base, private);
    }

    fn cond(field: ConditionField, app_id: Option<&str>) -> Condition {
        Condition {
            field,
            operator: ConditionOperator::Eq,
            value: json!(true),
            value_end: None,
            app_id: app_id.map(String::from),
        }
    }

    #[test]
    fn needs_derive_only_referenced_classes() {
        let rows = vec![(
            "g1".to_string(),
            vec![cond(ConditionField::SteamLevel, None)],
            None,
        )];
        let needs = RefreshNeeds::from_rows(&rows);
        assert!(needs.need_level);
        assert!(!needs.need_games);
        assert!(!needs.need_groups);
        assert!(!needs.needs_guild_scope());
        // level single + amortized batch call
        assert_eq!(needs.estimated_cost_per_user(), 2);

        // Account-only deployments still cost the batched baseline.
        let none = RefreshNeeds::from_rows(&[(
            "g1".to_string(),
            vec![cond(ConditionField::IsVACBanned, None)],
            None,
        )]);
        assert!(!none.need_games && !none.need_level && !none.need_groups);
        assert_eq!(none.estimated_cost_per_user(), 1);
    }

    #[test]
    fn needs_scope_achievements_and_ownership_by_guild() {
        let rows = vec![
            (
                "g1".to_string(),
                vec![cond(ConditionField::HasAchievement, Some("730"))],
                None,
            ),
            (
                "g2".to_string(),
                vec![cond(ConditionField::OwnsGame, Some("440"))],
                Some("PUBKEY".to_string()),
            ),
            (
                // OwnsGame without a publisher key: library only, no pair.
                "g3".to_string(),
                vec![cond(ConditionField::OwnsGame, Some("570"))],
                None,
            ),
        ];
        let needs = RefreshNeeds::from_rows(&rows);
        assert!(needs.need_games);
        assert!(needs.needs_guild_scope());
        assert_eq!(
            needs.achievement_apps.get("730").unwrap(),
            &HashSet::from(["g1".to_string()])
        );
        assert_eq!(
            needs
                .ownership_pairs
                .get(&("440".to_string(), "PUBKEY".to_string()))
                .unwrap(),
            &HashSet::from(["g2".to_string()])
        );
        assert!(!needs
            .ownership_pairs
            .contains_key(&("570".to_string(), "PUBKEY".to_string())));
    }
}

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;

use crate::services::sync::PlayerSyncEvent;
use crate::AppState;

const MIN_REFRESH_SECS: i64 = 1800; // 30 min floor
const MAX_REFRESH_SECS: i64 = 86400; // 24 hour cap
const INTERVAL_CACHE_SECS: u64 = 300;
const INACTIVE_MULTIPLIER: i64 = 6;

struct CachedInterval {
    value: AtomicI64,
    max_req_per_hour: i64,
    last_computed: Mutex<Instant>,
}

impl CachedInterval {
    fn new(max_req_per_hour: i64) -> Self {
        Self {
            value: AtomicI64::new(MIN_REFRESH_SECS),
            max_req_per_hour,
            last_computed: Mutex::new(
                Instant::now() - std::time::Duration::from_secs(INTERVAL_CACHE_SECS + 1),
            ),
        }
    }

    async fn get(&self, pool: &sqlx::PgPool) -> i64 {
        let mut last = self.last_computed.lock().await;
        if last.elapsed() >= std::time::Duration::from_secs(INTERVAL_CACHE_SECS) {
            let player_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM linked_accounts")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

            let interval = if player_count == 0 {
                MIN_REFRESH_SECS
            } else {
                ((player_count * 3600) / self.max_req_per_hour)
                    .clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS)
            };

            self.value.store(interval, Ordering::Relaxed);
            *last = Instant::now();
        }
        self.value.load(Ordering::Relaxed)
    }
}

pub async fn run(state: Arc<AppState>) {
    let max_req = state.config.steam_api_rate_limit as i64;
    tracing::info!(max_req, "Refresh worker started");

    let cached_interval = CachedInterval::new(max_req);

    loop {
        // Get next user due for refresh
        let next = sqlx::query_as::<_, (String, String, bool)>(
            "SELECT uc.steam_id, la.discord_id, \
             EXISTS(SELECT 1 FROM role_assignments ra WHERE ra.discord_id = la.discord_id) as is_active \
             FROM user_cache uc \
             JOIN linked_accounts la ON la.steam_id = uc.steam_id \
             WHERE uc.next_fetch_at <= now() \
             ORDER BY is_active DESC, uc.fetch_failures ASC, uc.next_fetch_at ASC \
             LIMIT 1",
        )
        .fetch_optional(&state.pool)
        .await;

        let (steam_id, discord_id, is_active) = match next {
            Ok(Some(row)) => row,
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                continue;
            }
            Err(e) => {
                tracing::error!("Refresh worker DB error: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        tracing::debug!(steam_id, is_active, "Refreshing Steam data");

        // Determine which app_ids need achievement refresh
        let needed_app_ids: HashSet<String> = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT c.value->>'app_id' \
             FROM role_links rl, jsonb_array_elements(rl.conditions) c \
             WHERE rl.guild_id IN (SELECT ra.guild_id FROM role_assignments ra WHERE ra.discord_id = $1) \
               AND c.value->>'app_id' IS NOT NULL",
        )
        .bind(&discord_id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();

        // Also get app_ids from all role links in guilds this user is in
        let all_app_ids: HashSet<String> = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT rl.conditions FROM role_links rl \
             WHERE rl.guild_id IN ( \
               SELECT DISTINCT ra.guild_id FROM role_assignments ra WHERE ra.discord_id = $1 \
               UNION \
               SELECT DISTINCT rl2.guild_id FROM role_links rl2 \
               JOIN linked_accounts la ON la.discord_id = $1 \
             )",
        )
        .bind(&discord_id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .flat_map(|conditions_json| {
            conditions_json
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|c| c.get("app_id").and_then(|v| v.as_str()).map(String::from))
                .collect::<Vec<_>>()
        })
        .collect();

        let app_ids_to_fetch: HashSet<String> =
            needed_app_ids.union(&all_app_ids).cloned().collect();

        match refresh_user(&state, &steam_id, &app_ids_to_fetch).await {
            Ok(()) => {
                let base_interval = cached_interval.get(&state.pool).await;
                let multiplier = if is_active { 1 } else { INACTIVE_MULTIPLIER };
                let interval = base_interval * multiplier;
                let next_fetch = chrono::Utc::now() + chrono::Duration::seconds(interval);

                if let Err(e) = sqlx::query(
                    "UPDATE user_cache SET next_fetch_at = $1, fetch_failures = 0 WHERE steam_id = $2",
                )
                .bind(next_fetch)
                .bind(&steam_id)
                .execute(&state.pool)
                .await
                {
                    tracing::error!(steam_id, "Failed to update next_fetch_at: {e}");
                }

                let _ = state
                    .player_sync_tx
                    .send(PlayerSyncEvent::PlayerUpdated { discord_id })
                    .await;

                tracing::debug!(steam_id, interval, is_active, "Steam data refreshed");
            }
            Err(e) => {
                // Exponential backoff
                if let Err(db_err) = sqlx::query(
                    "UPDATE user_cache SET fetch_failures = fetch_failures + 1, \
                     next_fetch_at = now() + LEAST(INTERVAL '60 seconds' * POWER(2, fetch_failures), INTERVAL '1 hour') \
                     WHERE steam_id = $1",
                )
                .bind(&steam_id)
                .execute(&state.pool)
                .await
                {
                    tracing::error!(steam_id, "Failed to update failure count: {db_err}");
                }
                tracing::warn!(steam_id, "Steam fetch failed: {e}");
            }
        }
    }
}

async fn refresh_user(
    state: &AppState,
    steam_id: &str,
    app_ids: &HashSet<String>,
) -> Result<(), crate::error::AppError> {
    let client = &state.steam_client;
    let ids: Vec<&str> = vec![steam_id];

    // Fetch profile + bans (batchable but doing single here since per-user refresh)
    let profiles = client.get_player_summaries(&ids).await?;
    let profile = profiles.into_iter().next();
    let bans = client.get_player_bans(&ids).await?;
    let ban = bans.into_iter().next();

    // Fetch level
    let level = client.get_steam_level(steam_id).await.unwrap_or(0);

    // Fetch owned games
    let (games, game_count) = client.get_owned_games(steam_id).await.unwrap_or_default();

    // Fetch groups
    let groups = client
        .get_user_group_list(steam_id)
        .await
        .unwrap_or_default();

    // Build JSON data
    let profile_data = serde_json::to_value(&profile).unwrap_or_default();
    let ban_data = serde_json::to_value(&ban).unwrap_or_default();
    let owned_games_json = serde_json::to_value(&games).unwrap_or_default();
    let groups_json = serde_json::to_value(&groups).unwrap_or_default();

    let is_vac_banned = ban.as_ref().map(|b| b.vac_banned).unwrap_or(false);
    let is_game_banned = ban
        .as_ref()
        .map(|b| b.number_of_game_bans > 0)
        .unwrap_or(false);
    let country_code = profile
        .as_ref()
        .and_then(|p| p.loccountrycode.clone())
        .unwrap_or_default();
    let account_created = profile
        .as_ref()
        .and_then(|p| p.timecreated)
        .map(|ts| chrono::DateTime::from_timestamp(ts, 0))
        .flatten();

    // Update user_cache
    sqlx::query(
        "UPDATE user_cache SET \
         profile_data = $1, ban_data = $2, owned_games = $3, groups = $4, \
         steam_level = $5, account_created = $6, total_games_owned = $7, \
         is_vac_banned = $8, is_game_banned = $9, country_code = $10, \
         fetched_at = now() \
         WHERE steam_id = $11",
    )
    .bind(&profile_data)
    .bind(&ban_data)
    .bind(&owned_games_json)
    .bind(&groups_json)
    .bind(level)
    .bind(account_created)
    .bind(game_count as i32)
    .bind(is_vac_banned)
    .bind(is_game_banned)
    .bind(&country_code)
    .bind(steam_id)
    .execute(&state.pool)
    .await?;

    // Fetch achievements for relevant games
    for app_id in app_ids {
        match client.get_player_achievements(steam_id, app_id).await {
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
                .bind(steam_id)
                .bind(app_id)
                .bind(&achievements_json)
                .bind(total)
                .bind(unlocked)
                .execute(&state.pool)
                .await?;
            }
            Err(e) => {
                tracing::warn!(steam_id, app_id, "Failed to fetch achievements: {e}");
            }
        }
    }

    Ok(())
}

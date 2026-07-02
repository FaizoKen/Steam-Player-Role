use std::collections::{HashMap, HashSet};

use futures_util::stream::{self, StreamExt};
use sqlx::PgPool;

use crate::error::AppError;
use crate::models::condition::{Condition, ConditionField, ConditionOperator};
use crate::services::auth_gateway;
use crate::services::condition_eval::{
    evaluate_conditions, AppOwnershipRow, GameAchievementRow, UserCacheRow,
};
use crate::AppState;

/// Events sent to the player sync worker (lightweight, per-user).
#[derive(Debug, Clone)]
pub enum PlayerSyncEvent {
    PlayerUpdated { discord_id: String },
    AccountLinked { discord_id: String },
    AccountUnlinked { discord_id: String },
}

/// Events sent to the config sync worker (heavy, per-role-link).
#[derive(Debug, Clone)]
pub struct ConfigSyncEvent {
    pub guild_id: String,
    pub role_id: String,
}

/// Sync roles for a single player across all guilds.
pub async fn sync_for_player(discord_id: &str, state: &AppState) -> Result<(), AppError> {
    let pool = &state.pool;
    let rl_client = &state.rl_client;

    // Get player's Steam ID and cached data
    let cache_row = sqlx::query_as::<
        _,
        (
            String,
            serde_json::Value,
            serde_json::Value,
            i32,
            Option<chrono::DateTime<chrono::Utc>>,
            i32,
            bool,
            bool,
            String,
        ),
    >(
        "SELECT uc.steam_id, uc.owned_games, uc.groups, uc.steam_level, uc.account_created, \
         uc.total_games_owned, uc.is_vac_banned, uc.is_game_banned, uc.country_code \
         FROM user_cache uc \
         JOIN linked_accounts la ON la.steam_id = uc.steam_id \
         WHERE la.discord_id = $1",
    )
    .bind(discord_id)
    .fetch_optional(pool)
    .await?;

    let Some((
        steam_id,
        owned_games,
        groups,
        steam_level,
        account_created,
        total_games_owned,
        is_vac_banned,
        is_game_banned,
        country_code,
    )) = cache_row
    else {
        return Ok(());
    };

    let user_cache = UserCacheRow {
        steam_id: steam_id.clone(),
        owned_games,
        groups,
        steam_level,
        account_created,
        total_games_owned,
        is_vac_banned,
        is_game_banned,
        country_code,
    };

    // Get guild IDs from Auth Gateway
    let guild_ids = auth_gateway::fetch_user_guild_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        discord_id,
    )
    .await?;

    if guild_ids.is_empty() {
        return Ok(());
    }

    // Get role links for guilds this user is in
    let role_links = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            sqlx::types::Json<Vec<Condition>>,
            Option<String>,
        ),
    >(
        "SELECT rl.guild_id, rl.role_id, rl.api_token, rl.conditions, rl.publisher_key \
         FROM role_links rl \
         WHERE rl.guild_id = ANY($1)",
    )
    .bind(&guild_ids[..])
    .fetch_all(pool)
    .await?;

    // Collect app_ids referenced by conditions for achievement lookup
    let needed_app_ids: HashSet<String> = role_links
        .iter()
        .flat_map(|(_, _, _, conditions, _)| conditions.iter().filter_map(|c| c.app_id.clone()))
        .collect();

    // Fetch achievement data for needed games
    let mut game_achievements: HashMap<String, GameAchievementRow> = HashMap::new();
    for app_id in &needed_app_ids {
        if let Ok(Some(row)) = sqlx::query_as::<_, (serde_json::Value, i32, i32)>(
            "SELECT achievements, total_count, unlocked_count \
             FROM game_achievement_cache WHERE steam_id = $1 AND app_id = $2",
        )
        .bind(&steam_id)
        .bind(app_id)
        .fetch_optional(pool)
        .await
        {
            game_achievements.insert(
                app_id.clone(),
                GameAchievementRow {
                    total_count: row.1,
                    unlocked_count: row.2,
                    achievements: row.0,
                },
            );
        }
    }

    // Fetch partner-API ownership results for apps referenced by
    // publisher-key role links (shared map; only applied to those links)
    let ownership_app_ids: HashSet<String> = role_links
        .iter()
        .filter(|(_, _, _, _, publisher_key)| publisher_key.is_some())
        .flat_map(|(_, _, _, conditions, _)| {
            conditions
                .iter()
                .filter(|c| c.field == ConditionField::OwnsGame)
                .filter_map(|c| c.app_id.clone())
        })
        .collect();

    let mut app_ownership: HashMap<String, AppOwnershipRow> = HashMap::new();
    for app_id in &ownership_app_ids {
        if let Ok(Some(row)) = sqlx::query_as::<_, (bool, bool, String)>(
            "SELECT owns_app, permanent, owner_steam_id \
             FROM app_ownership_cache WHERE steam_id = $1 AND app_id = $2",
        )
        .bind(&steam_id)
        .bind(app_id)
        .fetch_optional(pool)
        .await
        {
            app_ownership.insert(
                app_id.clone(),
                AppOwnershipRow {
                    owns_app: row.0,
                    permanent: row.1,
                    owner_steam_id: row.2,
                },
            );
        }
    }

    // Batch fetch existing assignments
    let existing: HashSet<(String, String)> = sqlx::query_as::<_, (String, String)>(
        "SELECT guild_id, role_id FROM role_assignments WHERE discord_id = $1",
    )
    .bind(discord_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    // Phase 1: evaluate conditions locally
    enum Action {
        Add {
            guild_id: String,
            role_id: String,
            api_token: String,
        },
        Remove {
            guild_id: String,
            role_id: String,
            api_token: String,
        },
    }

    let mut actions: Vec<Action> = Vec::new();
    for (guild_id, role_id, api_token, conditions, publisher_key) in &role_links {
        let ownership = publisher_key.as_ref().map(|_| &app_ownership);
        let qualifies = evaluate_conditions(conditions, &user_cache, &game_achievements, ownership);
        let currently_assigned = existing.contains(&(guild_id.clone(), role_id.clone()));
        match (qualifies, currently_assigned) {
            (true, false) => actions.push(Action::Add {
                guild_id: guild_id.clone(),
                role_id: role_id.clone(),
                api_token: api_token.clone(),
            }),
            (false, true) => actions.push(Action::Remove {
                guild_id: guild_id.clone(),
                role_id: role_id.clone(),
                api_token: api_token.clone(),
            }),
            _ => {}
        }
    }

    if actions.is_empty() {
        return Ok(());
    }

    // Phase 2: execute API calls concurrently
    let discord_id_owned = discord_id.to_string();
    stream::iter(actions)
        .for_each_concurrent(10, |action| {
            let pool = pool.clone();
            let rl_client = rl_client.clone();
            let discord_id = discord_id_owned.clone();
            async move {
                match action {
                    Action::Add { guild_id, role_id, api_token } => {
                        match rl_client.add_user(&guild_id, &role_id, &discord_id, &api_token).await {
                            Err(AppError::RoleLinkNotFound) => {
                                delete_orphan_role_link(&guild_id, &role_id, &pool).await;
                                return;
                            }
                            Err(AppError::UserLimitReached { limit }) => {
                                tracing::warn!(guild_id, role_id, discord_id, limit, "Cannot add user: limit reached");
                                return;
                            }
                            Err(e) => {
                                tracing::error!(guild_id, role_id, discord_id, "Failed to add user: {e}");
                                return;
                            }
                            Ok(_) => {}
                        }
                        if let Err(e) = sqlx::query(
                            "INSERT INTO role_assignments (guild_id, role_id, discord_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                        )
                        .bind(&guild_id).bind(&role_id).bind(&discord_id)
                        .execute(&pool).await {
                            tracing::error!(guild_id, role_id, discord_id, "Failed to insert assignment: {e}");
                        }
                    }
                    Action::Remove { guild_id, role_id, api_token } => {
                        match rl_client.remove_user(&guild_id, &role_id, &discord_id, &api_token).await {
                            Err(AppError::RoleLinkNotFound) => {
                                delete_orphan_role_link(&guild_id, &role_id, &pool).await;
                                return;
                            }
                            Err(e) => {
                                tracing::error!(guild_id, role_id, discord_id, "Failed to remove user: {e}");
                                return;
                            }
                            Ok(_) => {}
                        }
                        if let Err(e) = sqlx::query(
                            "DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2 AND discord_id = $3",
                        )
                        .bind(&guild_id).bind(&role_id).bind(&discord_id)
                        .execute(&pool).await {
                            tracing::error!(guild_id, role_id, discord_id, "Failed to delete assignment: {e}");
                        }
                    }
                }
            }
        })
        .await;

    Ok(())
}

/// Bind value types for dynamic condition queries.
enum ConditionBind {
    Int(i64),
    Text(String),
    Bool(bool),
}

/// Build a SQL WHERE clause from conditions for SQL-side filtering.
/// With `skip_owns_game`, OwnsGame produces no clause — the caller
/// evaluates it in-memory against app_ownership_cache instead of the
/// owned_games JSONB (which is empty for private libraries).
fn build_condition_where(
    conditions: &[Condition],
    skip_owns_game: bool,
) -> (String, Vec<ConditionBind>) {
    if conditions.is_empty() {
        return ("TRUE".to_string(), vec![]);
    }

    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<ConditionBind> = Vec::new();

    for condition in conditions {
        match &condition.field {
            ConditionField::SteamLevel | ConditionField::TotalGamesOwned => {
                let col = condition.field.sql_column().unwrap();
                let val = condition.value.as_i64().unwrap_or(0);
                if matches!(condition.operator, ConditionOperator::Between) {
                    let end = condition
                        .value_end
                        .as_ref()
                        .and_then(|v| v.as_i64())
                        .unwrap_or(val);
                    let idx_start = binds.len() + 1;
                    let idx_end = binds.len() + 2;
                    clauses.push(format!("{col} >= ${idx_start} AND {col} <= ${idx_end}"));
                    binds.push(ConditionBind::Int(val));
                    binds.push(ConditionBind::Int(end));
                } else {
                    let op = condition.operator.sql_operator();
                    let idx = binds.len() + 1;
                    clauses.push(format!("{col} {op} ${idx}"));
                    binds.push(ConditionBind::Int(val));
                }
            }
            ConditionField::IsVACBanned | ConditionField::IsGameBanned => {
                let col = condition.field.sql_column().unwrap();
                let val = condition.value.as_bool().unwrap_or(true);
                let idx = binds.len() + 1;
                clauses.push(format!("{col} = ${idx}"));
                binds.push(ConditionBind::Bool(val));
            }
            ConditionField::CountryCode => {
                let val = condition.value.as_str().unwrap_or("").to_string();
                let idx = binds.len() + 1;
                clauses.push(format!("LOWER(uc.country_code) = LOWER(${idx})"));
                binds.push(ConditionBind::Text(val));
            }
            ConditionField::AccountAgeDays => {
                let val = condition.value.as_i64().unwrap_or(0);
                if matches!(condition.operator, ConditionOperator::Between) {
                    let end = condition
                        .value_end
                        .as_ref()
                        .and_then(|v| v.as_i64())
                        .unwrap_or(val);
                    let idx_start = binds.len() + 1;
                    let idx_end = binds.len() + 2;
                    clauses.push(format!(
                        "EXTRACT(EPOCH FROM (now() - uc.account_created)) / 86400 >= ${idx_start} \
                         AND EXTRACT(EPOCH FROM (now() - uc.account_created)) / 86400 <= ${idx_end}"
                    ));
                    binds.push(ConditionBind::Int(val));
                    binds.push(ConditionBind::Int(end));
                } else {
                    let op = condition.operator.sql_operator();
                    let idx = binds.len() + 1;
                    clauses.push(format!(
                        "EXTRACT(EPOCH FROM (now() - uc.account_created)) / 86400 {op} ${idx}"
                    ));
                    binds.push(ConditionBind::Int(val));
                }
            }
            ConditionField::OwnsGame => {
                if skip_owns_game {
                    continue;
                }
                if let Some(app_id) = &condition.app_id {
                    let expected = condition.value.as_bool().unwrap_or(true);
                    let idx = binds.len() + 1;
                    let containment = format!(
                        "uc.owned_games @> concat('[{{\"appid\":', ${idx}::text, '}}]')::jsonb"
                    );
                    if expected {
                        clauses.push(containment);
                    } else {
                        clauses.push(format!("NOT ({containment})"));
                    }
                    binds.push(ConditionBind::Int(app_id.parse().unwrap_or(0)));
                }
            }
            ConditionField::InGroup => {
                let gid = condition.value.as_str().unwrap_or("").to_string();
                let idx = binds.len() + 1;
                clauses.push(format!(
                    "uc.groups @> concat('[{{\"gid\":\"', ${idx}::text, '\"}}]')::jsonb"
                ));
                binds.push(ConditionBind::Text(gid));
            }
            // Game-specific conditions that need JSONB or achievement table
            // are evaluated in-memory after SQL pre-filter
            _ => {}
        }
    }

    if clauses.is_empty() {
        return ("TRUE".to_string(), vec![]);
    }

    (clauses.join(" AND "), binds)
}

/// Re-evaluate all users for a specific role link (after config change).
pub async fn sync_for_role_link(
    guild_id: &str,
    role_id: &str,
    state: &AppState,
) -> Result<(), AppError> {
    let pool = &state.pool;
    let rl_client = &state.rl_client;

    let link = sqlx::query_as::<_, (String, sqlx::types::Json<Vec<Condition>>, Option<String>)>(
        "SELECT api_token, conditions, publisher_key FROM role_links \
         WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(guild_id)
    .bind(role_id)
    .fetch_optional(pool)
    .await?;

    let Some((api_token, conditions, publisher_key)) = link else {
        return Ok(());
    };

    // No conditions configured → role is unconfigured, assign to nobody.
    if conditions.is_empty() {
        match rl_client
            .upload_users(guild_id, role_id, &[], &api_token)
            .await
        {
            Ok(_) => {}
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        sqlx::query("DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id)
            .bind(role_id)
            .execute(pool)
            .await?;
        return Ok(());
    }

    let (_user_count, user_limit) =
        match rl_client.get_user_info(guild_id, role_id, &api_token).await {
            Ok(v) => v,
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
                return Ok(());
            }
            Err(AppError::RoleLinkDisabled) => return Ok(()),
            Err(e) => return Err(e),
        };

    let member_ids = auth_gateway::fetch_guild_member_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        guild_id,
    )
    .await?;

    if member_ids.is_empty() {
        match rl_client
            .upload_users(guild_id, role_id, &[], &api_token)
            .await
        {
            Ok(_) => {}
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id)
            .bind(role_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(());
    }

    // Check if any conditions need achievement data (can't be fully pushed to SQL)
    let needs_achievement_eval = conditions.iter().any(|c| {
        matches!(
            c.field,
            ConditionField::AchievementCount
                | ConditionField::AchievementPercent
                | ConditionField::HasAchievement
                | ConditionField::GamePlaytime
                | ConditionField::RecentPlaytime
        )
    });

    // Publisher-key OwnsGame checks must not touch the owned_games JSONB
    // (empty for private libraries) — keep them out of the SQL filter and
    // evaluate in-memory against app_ownership_cache.
    let use_publisher_ownership = publisher_key.is_some()
        && conditions
            .iter()
            .any(|c| c.field == ConditionField::OwnsGame);

    let (where_clause, binds) = build_condition_where(&conditions, use_publisher_ownership);

    if needs_achievement_eval || use_publisher_ownership {
        // For conditions that need JSONB or achievement data, fetch candidates
        // with SQL pre-filter, then evaluate in-memory
        let members_bind_idx = binds.len() + 1;
        let query_str = format!(
            "SELECT la.discord_id, uc.steam_id, uc.owned_games, uc.groups, uc.steam_level, \
             uc.account_created, uc.total_games_owned, uc.is_vac_banned, uc.is_game_banned, \
             uc.country_code \
             FROM linked_accounts la \
             JOIN user_cache uc ON uc.steam_id = la.steam_id \
             WHERE la.discord_id = ANY(${members_bind_idx}::text[]) \
               AND ({where_clause}) \
             ORDER BY la.linked_at ASC",
        );

        let mut q = sqlx::query_as::<
            _,
            (
                String,
                String,
                serde_json::Value,
                serde_json::Value,
                i32,
                Option<chrono::DateTime<chrono::Utc>>,
                i32,
                bool,
                bool,
                String,
            ),
        >(&query_str);
        for bind in &binds {
            q = match bind {
                ConditionBind::Int(v) => q.bind(*v),
                ConditionBind::Text(v) => q.bind(v),
                ConditionBind::Bool(v) => q.bind(*v),
            };
        }
        q = q.bind(&member_ids);

        let candidates = q.fetch_all(pool).await?;

        // Collect app_ids needed
        let needed_app_ids: HashSet<String> =
            conditions.iter().filter_map(|c| c.app_id.clone()).collect();

        let mut qualifying_ids: Vec<String> = Vec::new();
        for (
            discord_id,
            steam_id,
            owned_games,
            groups,
            steam_level,
            account_created,
            total_games_owned,
            is_vac_banned,
            is_game_banned,
            country_code,
        ) in candidates
        {
            let uc = UserCacheRow {
                steam_id: steam_id.clone(),
                owned_games,
                groups,
                steam_level,
                account_created,
                total_games_owned,
                is_vac_banned,
                is_game_banned,
                country_code,
            };

            let mut achievements: HashMap<String, GameAchievementRow> = HashMap::new();
            for app_id in &needed_app_ids {
                if let Ok(Some(row)) = sqlx::query_as::<_, (serde_json::Value, i32, i32)>(
                    "SELECT achievements, total_count, unlocked_count \
                     FROM game_achievement_cache WHERE steam_id = $1 AND app_id = $2",
                )
                .bind(&steam_id)
                .bind(app_id)
                .fetch_optional(pool)
                .await
                {
                    achievements.insert(
                        app_id.clone(),
                        GameAchievementRow {
                            total_count: row.1,
                            unlocked_count: row.2,
                            achievements: row.0,
                        },
                    );
                }
            }

            let mut app_ownership: HashMap<String, AppOwnershipRow> = HashMap::new();
            if use_publisher_ownership {
                for app_id in &needed_app_ids {
                    if let Ok(Some(row)) = sqlx::query_as::<_, (bool, bool, String)>(
                        "SELECT owns_app, permanent, owner_steam_id \
                         FROM app_ownership_cache WHERE steam_id = $1 AND app_id = $2",
                    )
                    .bind(&steam_id)
                    .bind(app_id)
                    .fetch_optional(pool)
                    .await
                    {
                        app_ownership.insert(
                            app_id.clone(),
                            AppOwnershipRow {
                                owns_app: row.0,
                                permanent: row.1,
                                owner_steam_id: row.2,
                            },
                        );
                    }
                }
            }

            let ownership = use_publisher_ownership.then_some(&app_ownership);
            if evaluate_conditions(&conditions, &uc, &achievements, ownership) {
                qualifying_ids.push(discord_id);
                if qualifying_ids.len() >= user_limit {
                    break;
                }
            }
        }

        // Atomic replace (uses chunked upload if > 100k)
        match rl_client
            .upload_users(guild_id, role_id, &qualifying_ids, &api_token)
            .await
        {
            Ok(_) => {}
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
                return Ok(());
            }
            Err(e) => return Err(e),
        }

        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id)
            .bind(role_id)
            .execute(&mut *tx)
            .await?;
        if !qualifying_ids.is_empty() {
            sqlx::query(
                "INSERT INTO role_assignments (guild_id, role_id, discord_id) \
                 SELECT $1, $2, UNNEST($3::text[])",
            )
            .bind(guild_id)
            .bind(role_id)
            .bind(&qualifying_ids)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    } else {
        // Pure SQL path - all conditions can be evaluated server-side
        let members_bind_idx = binds.len() + 1;
        let limit_bind_idx = binds.len() + 2;
        let query_str = format!(
            "SELECT la.discord_id \
             FROM linked_accounts la \
             JOIN user_cache uc ON uc.steam_id = la.steam_id \
             WHERE la.discord_id = ANY(${members_bind_idx}::text[]) \
               AND ({where_clause}) \
             ORDER BY la.linked_at ASC \
             LIMIT ${limit_bind_idx}",
        );

        let qualifying_ids =
            exec_condition_query(&query_str, &binds, &member_ids, user_limit, pool).await?;

        match rl_client
            .upload_users(guild_id, role_id, &qualifying_ids, &api_token)
            .await
        {
            Ok(_) => {}
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
                return Ok(());
            }
            Err(e) => return Err(e),
        }

        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id)
            .bind(role_id)
            .execute(&mut *tx)
            .await?;
        if !qualifying_ids.is_empty() {
            sqlx::query(
                "INSERT INTO role_assignments (guild_id, role_id, discord_id) \
                 SELECT $1, $2, UNNEST($3::text[])",
            )
            .bind(guild_id)
            .bind(role_id)
            .bind(&qualifying_ids)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }

    Ok(())
}

/// Count how many of `member_ids` are linked and how many satisfy
/// `conditions` — powers the role-config page's preview. Mirrors
/// `sync_for_role_link`'s evaluation (SQL pre-filter plus in-memory
/// achievement / publisher-ownership eval) but only counts; it never
/// touches RoleLogic or role_assignments.
///
/// Returns `(matching, linked)`.
pub async fn preview_matching_count(
    conditions: &[Condition],
    has_publisher_key: bool,
    member_ids: &[String],
    pool: &PgPool,
) -> Result<(i64, i64), AppError> {
    if member_ids.is_empty() {
        return Ok((0, 0));
    }

    let linked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM linked_accounts WHERE discord_id = ANY($1::text[])",
    )
    .bind(member_ids)
    .fetch_one(pool)
    .await?;

    if conditions.is_empty() || linked == 0 {
        return Ok((0, linked));
    }

    let needs_achievement_eval = conditions.iter().any(|c| {
        matches!(
            c.field,
            ConditionField::AchievementCount
                | ConditionField::AchievementPercent
                | ConditionField::HasAchievement
                | ConditionField::GamePlaytime
                | ConditionField::RecentPlaytime
        )
    });
    let use_publisher_ownership = has_publisher_key
        && conditions
            .iter()
            .any(|c| c.field == ConditionField::OwnsGame);

    let (where_clause, binds) = build_condition_where(conditions, use_publisher_ownership);

    if !(needs_achievement_eval || use_publisher_ownership) {
        // Pure SQL path — everything can be counted server-side.
        let members_bind_idx = binds.len() + 1;
        let query_str = format!(
            "SELECT count(*) \
             FROM linked_accounts la \
             JOIN user_cache uc ON uc.steam_id = la.steam_id \
             WHERE la.discord_id = ANY(${members_bind_idx}::text[]) \
               AND ({where_clause})",
        );
        let mut q = sqlx::query_scalar::<_, i64>(&query_str);
        for bind in &binds {
            q = match bind {
                ConditionBind::Int(v) => q.bind(*v),
                ConditionBind::Text(v) => q.bind(v),
                ConditionBind::Bool(v) => q.bind(*v),
            };
        }
        q = q.bind(member_ids);
        let matching = q.fetch_one(pool).await?;
        return Ok((matching, linked));
    }

    // Achievement / publisher-ownership conditions: SQL pre-filter, then
    // in-memory evaluation against the cached data (same as the sync path).
    let members_bind_idx = binds.len() + 1;
    let query_str = format!(
        "SELECT la.discord_id, uc.steam_id, uc.owned_games, uc.groups, uc.steam_level, \
         uc.account_created, uc.total_games_owned, uc.is_vac_banned, uc.is_game_banned, \
         uc.country_code \
         FROM linked_accounts la \
         JOIN user_cache uc ON uc.steam_id = la.steam_id \
         WHERE la.discord_id = ANY(${members_bind_idx}::text[]) \
           AND ({where_clause})",
    );
    let mut q = sqlx::query_as::<
        _,
        (
            String,
            String,
            serde_json::Value,
            serde_json::Value,
            i32,
            Option<chrono::DateTime<chrono::Utc>>,
            i32,
            bool,
            bool,
            String,
        ),
    >(&query_str);
    for bind in &binds {
        q = match bind {
            ConditionBind::Int(v) => q.bind(*v),
            ConditionBind::Text(v) => q.bind(v),
            ConditionBind::Bool(v) => q.bind(*v),
        };
    }
    q = q.bind(member_ids);
    let candidates = q.fetch_all(pool).await?;

    let needed_app_ids: HashSet<String> =
        conditions.iter().filter_map(|c| c.app_id.clone()).collect();

    let mut matching: i64 = 0;
    for (
        _discord_id,
        steam_id,
        owned_games,
        groups,
        steam_level,
        account_created,
        total_games_owned,
        is_vac_banned,
        is_game_banned,
        country_code,
    ) in candidates
    {
        let uc = UserCacheRow {
            steam_id: steam_id.clone(),
            owned_games,
            groups,
            steam_level,
            account_created,
            total_games_owned,
            is_vac_banned,
            is_game_banned,
            country_code,
        };

        let mut achievements: HashMap<String, GameAchievementRow> = HashMap::new();
        for app_id in &needed_app_ids {
            if let Ok(Some(row)) = sqlx::query_as::<_, (serde_json::Value, i32, i32)>(
                "SELECT achievements, total_count, unlocked_count \
                 FROM game_achievement_cache WHERE steam_id = $1 AND app_id = $2",
            )
            .bind(&steam_id)
            .bind(app_id)
            .fetch_optional(pool)
            .await
            {
                achievements.insert(
                    app_id.clone(),
                    GameAchievementRow {
                        total_count: row.1,
                        unlocked_count: row.2,
                        achievements: row.0,
                    },
                );
            }
        }

        let mut app_ownership: HashMap<String, AppOwnershipRow> = HashMap::new();
        if use_publisher_ownership {
            for app_id in &needed_app_ids {
                if let Ok(Some(row)) = sqlx::query_as::<_, (bool, bool, String)>(
                    "SELECT owns_app, permanent, owner_steam_id \
                     FROM app_ownership_cache WHERE steam_id = $1 AND app_id = $2",
                )
                .bind(&steam_id)
                .bind(app_id)
                .fetch_optional(pool)
                .await
                {
                    app_ownership.insert(
                        app_id.clone(),
                        AppOwnershipRow {
                            owns_app: row.0,
                            permanent: row.1,
                            owner_steam_id: row.2,
                        },
                    );
                }
            }
        }

        let ownership = use_publisher_ownership.then_some(&app_ownership);
        if evaluate_conditions(conditions, &uc, &achievements, ownership) {
            matching += 1;
        }
    }

    Ok((matching, linked))
}

async fn exec_condition_query(
    query: &str,
    binds: &[ConditionBind],
    member_ids: &[String],
    limit: usize,
    pool: &PgPool,
) -> Result<Vec<String>, AppError> {
    let mut q = sqlx::query_scalar::<_, String>(query);
    for bind in binds {
        q = match bind {
            ConditionBind::Int(v) => q.bind(*v),
            ConditionBind::Text(v) => q.bind(v),
            ConditionBind::Bool(v) => q.bind(*v),
        };
    }
    q = q.bind(member_ids);
    q = q.bind(limit as i64);
    Ok(q.fetch_all(pool).await?)
}

/// Remove a user from all role assignments (after account unlink).
pub async fn remove_all_assignments(discord_id: &str, state: &AppState) -> Result<(), AppError> {
    let pool = &state.pool;
    let rl_client = &state.rl_client;
    let assignments = sqlx::query_as::<_, (String, String, String)>(
        "SELECT ra.guild_id, ra.role_id, rl.api_token \
         FROM role_assignments ra \
         JOIN role_links rl ON rl.guild_id = ra.guild_id AND rl.role_id = ra.role_id \
         WHERE ra.discord_id = $1",
    )
    .bind(discord_id)
    .fetch_all(pool)
    .await?;

    for (guild_id, role_id, api_token) in &assignments {
        match rl_client
            .remove_user(guild_id, role_id, discord_id, api_token)
            .await
        {
            Ok(_) => {}
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
            }
            Err(e) => {
                tracing::error!(
                    guild_id,
                    role_id,
                    discord_id,
                    "Failed to remove user during unlink: {e}"
                );
            }
        }
    }

    sqlx::query("DELETE FROM role_assignments WHERE discord_id = $1")
        .bind(discord_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Delete a role_link the RoleLogic API reports as gone (403 Invalid or
/// revoked token). CASCADE clears role_assignments. Best-effort: logs DB
/// failures, never propagates them — sync workers must not stop syncing
/// other links over a cleanup hiccup.
async fn delete_orphan_role_link(guild_id: &str, role_id: &str, pool: &PgPool) {
    tracing::warn!(
        guild_id,
        role_id,
        "Role link not found on RoleLogic; removing orphaned local row"
    );
    if let Err(e) = sqlx::query("DELETE FROM role_links WHERE guild_id = $1 AND role_id = $2")
        .bind(guild_id)
        .bind(role_id)
        .execute(pool)
        .await
    {
        tracing::error!(guild_id, role_id, "Failed to delete orphan role_link: {e}");
    }
}

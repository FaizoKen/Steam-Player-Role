use std::collections::HashMap;

use crate::models::condition::{Condition, ConditionField, ConditionOperator};

/// Row from user_cache table
pub struct UserCacheRow {
    pub steam_id: String,
    pub owned_games: serde_json::Value,
    pub groups: serde_json::Value,
    pub steam_level: i32,
    pub account_created: Option<chrono::DateTime<chrono::Utc>>,
    pub total_games_owned: i32,
    pub is_vac_banned: bool,
    pub is_game_banned: bool,
    pub country_code: String,
}

/// Row from game_achievement_cache table
pub struct GameAchievementRow {
    pub total_count: i32,
    pub unlocked_count: i32,
    pub achievements: serde_json::Value,
}

/// Row from app_ownership_cache table (partner-API CheckAppOwnership)
pub struct AppOwnershipRow {
    pub owns_app: bool,
    pub permanent: bool,
    pub owner_steam_id: String,
}

/// Evaluate all conditions against cached data. All must pass (AND logic).
/// An empty condition list is treated as "unconfigured" and grants no role.
///
/// `publisher_ownership` must only be `Some` for role links that have a
/// publisher key configured — OwnsGame then prefers the partner-API result
/// (keyed by app_id) over the public library, falling back to the library
/// for apps not yet cached.
pub fn evaluate_conditions(
    conditions: &[Condition],
    user_cache: &UserCacheRow,
    game_achievements: &HashMap<String, GameAchievementRow>,
    publisher_ownership: Option<&HashMap<String, AppOwnershipRow>>,
) -> bool {
    if conditions.is_empty() {
        return false;
    }
    conditions
        .iter()
        .all(|c| evaluate_single(c, user_cache, game_achievements, publisher_ownership))
}

fn evaluate_single(
    condition: &Condition,
    uc: &UserCacheRow,
    achievements: &HashMap<String, GameAchievementRow>,
    publisher_ownership: Option<&HashMap<String, AppOwnershipRow>>,
) -> bool {
    match &condition.field {
        ConditionField::OwnsGame => {
            let app_id = match &condition.app_id {
                Some(id) => id,
                None => return false,
            };
            let expected = condition.value.as_bool().unwrap_or(true);
            let owns = match publisher_ownership.and_then(|m| m.get(app_id.as_str())) {
                // Partner-API result: works with private libraries. The
                // license must belong to the linked account (excludes
                // Family Sharing) and be permanent (excludes free
                // weekends / timed trials).
                Some(o) => o.owns_app && o.permanent && o.owner_steam_id == uc.steam_id,
                None => {
                    let app_id_num: i64 = app_id.parse().unwrap_or(0);
                    uc.owned_games.as_array().is_some_and(|games| {
                        games
                            .iter()
                            .any(|g| g["appid"].as_i64() == Some(app_id_num))
                    })
                }
            };
            owns == expected
        }
        ConditionField::GamePlaytime => {
            let app_id = match &condition.app_id {
                Some(id) => id,
                None => return false,
            };
            let app_id_num: i64 = app_id.parse().unwrap_or(0);
            let playtime_minutes = uc
                .owned_games
                .as_array()
                .and_then(|games| {
                    games
                        .iter()
                        .find(|g| g["appid"].as_i64() == Some(app_id_num))
                        .and_then(|g| g["playtime_forever"].as_i64())
                })
                .unwrap_or(0);
            let playtime_hours = playtime_minutes / 60;
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(
                playtime_hours,
                expected,
                &condition.operator,
                &condition.value_end,
            )
        }
        ConditionField::RecentPlaytime => {
            let app_id = match &condition.app_id {
                Some(id) => id,
                None => return false,
            };
            let app_id_num: i64 = app_id.parse().unwrap_or(0);
            let playtime_minutes = uc
                .owned_games
                .as_array()
                .and_then(|games| {
                    games
                        .iter()
                        .find(|g| g["appid"].as_i64() == Some(app_id_num))
                        .and_then(|g| g["playtime_2weeks"].as_i64())
                })
                .unwrap_or(0);
            let playtime_hours = playtime_minutes / 60;
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(
                playtime_hours,
                expected,
                &condition.operator,
                &condition.value_end,
            )
        }
        ConditionField::AchievementCount => {
            let app_id = match &condition.app_id {
                Some(id) => id.as_str(),
                None => return false,
            };
            let unlocked = achievements
                .get(app_id)
                .map(|a| a.unlocked_count as i64)
                .unwrap_or(0);
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(
                unlocked,
                expected,
                &condition.operator,
                &condition.value_end,
            )
        }
        ConditionField::AchievementPercent => {
            let app_id = match &condition.app_id {
                Some(id) => id.as_str(),
                None => return false,
            };
            let pct = achievements
                .get(app_id)
                .map(|a| {
                    if a.total_count == 0 {
                        0
                    } else {
                        (a.unlocked_count as i64 * 100) / a.total_count as i64
                    }
                })
                .unwrap_or(0);
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(pct, expected, &condition.operator, &condition.value_end)
        }
        ConditionField::HasAchievement => {
            let app_id = match &condition.app_id {
                Some(id) => id.as_str(),
                None => return false,
            };
            let target_name = condition.value.as_str().unwrap_or("");
            achievements.get(app_id).is_some_and(|a| {
                a.achievements.as_array().is_some_and(|list| {
                    list.iter().any(|ach| {
                        ach["apiname"].as_str() == Some(target_name)
                            && ach["achieved"].as_i64() == Some(1)
                    })
                })
            })
        }
        ConditionField::SteamLevel => {
            let actual = uc.steam_level as i64;
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(actual, expected, &condition.operator, &condition.value_end)
        }
        ConditionField::AccountAgeDays => {
            let actual = uc
                .account_created
                .map(|created| (chrono::Utc::now() - created).num_days())
                .unwrap_or(0);
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(actual, expected, &condition.operator, &condition.value_end)
        }
        ConditionField::TotalGamesOwned => {
            let actual = uc.total_games_owned as i64;
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(actual, expected, &condition.operator, &condition.value_end)
        }
        ConditionField::IsVACBanned => {
            let expected = condition.value.as_bool().unwrap_or(true);
            uc.is_vac_banned == expected
        }
        ConditionField::IsGameBanned => {
            let expected = condition.value.as_bool().unwrap_or(true);
            uc.is_game_banned == expected
        }
        ConditionField::InGroup => {
            let target_gid = condition.value.as_str().unwrap_or("");
            uc.groups
                .as_array()
                .is_some_and(|groups| groups.iter().any(|g| g["gid"].as_str() == Some(target_gid)))
        }
        ConditionField::CountryCode => {
            let expected = condition.value.as_str().unwrap_or("");
            uc.country_code.eq_ignore_ascii_case(expected)
        }
    }
}

fn compare(
    actual: i64,
    expected: i64,
    operator: &ConditionOperator,
    value_end: &Option<serde_json::Value>,
) -> bool {
    match operator {
        ConditionOperator::Eq => actual == expected,
        ConditionOperator::Gt => actual > expected,
        ConditionOperator::Gte => actual >= expected,
        ConditionOperator::Lt => actual < expected,
        ConditionOperator::Lte => actual <= expected,
        ConditionOperator::Between => {
            let end = value_end
                .as_ref()
                .and_then(|v| v.as_i64())
                .unwrap_or(expected);
            actual >= expected && actual <= end
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_user_cache() -> UserCacheRow {
        UserCacheRow {
            steam_id: "76561198012345678".to_string(),
            owned_games: json!([
                {"appid": 730, "name": "Counter-Strike 2", "playtime_forever": 12000, "playtime_2weeks": 120},
                {"appid": 570, "name": "Dota 2", "playtime_forever": 600}
            ]),
            groups: json!([{"gid": "103582791429521408"}, {"gid": "103582791432297141"}]),
            steam_level: 25,
            account_created: Some(chrono::Utc::now() - chrono::Duration::days(3650)),
            total_games_owned: 150,
            is_vac_banned: false,
            is_game_banned: false,
            country_code: "US".to_string(),
        }
    }

    fn sample_achievements() -> HashMap<String, GameAchievementRow> {
        let mut map = HashMap::new();
        map.insert(
            "730".to_string(),
            GameAchievementRow {
                total_count: 67,
                unlocked_count: 35,
                achievements: json!([
                    {"apiname": "WIN_BOMB_DEFUSE", "achieved": 1, "unlocktime": 1600000000},
                    {"apiname": "KILL_ENEMY_RELOADING", "achieved": 1, "unlocktime": 1600000001},
                    {"apiname": "UNSTOPPABLE_FORCE", "achieved": 0}
                ]),
            },
        );
        map
    }

    #[test]
    fn test_owns_game_true() {
        let conditions = vec![Condition {
            field: ConditionField::OwnsGame,
            operator: ConditionOperator::Eq,
            value: json!(true),
            value_end: None,
            app_id: Some("730".to_string()),
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_owns_game_false() {
        let conditions = vec![Condition {
            field: ConditionField::OwnsGame,
            operator: ConditionOperator::Eq,
            value: json!(true),
            value_end: None,
            app_id: Some("999999".to_string()),
        }];
        assert!(!evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_game_playtime() {
        // 12000 minutes = 200 hours
        let conditions = vec![Condition {
            field: ConditionField::GamePlaytime,
            operator: ConditionOperator::Gte,
            value: json!(100),
            value_end: None,
            app_id: Some("730".to_string()),
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_steam_level() {
        let conditions = vec![Condition {
            field: ConditionField::SteamLevel,
            operator: ConditionOperator::Gte,
            value: json!(10),
            value_end: None,
            app_id: None,
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_is_vac_banned_false() {
        let conditions = vec![Condition {
            field: ConditionField::IsVACBanned,
            operator: ConditionOperator::Eq,
            value: json!(false),
            value_end: None,
            app_id: None,
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_in_group() {
        let conditions = vec![Condition {
            field: ConditionField::InGroup,
            operator: ConditionOperator::Eq,
            value: json!("103582791429521408"),
            value_end: None,
            app_id: None,
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_country_code() {
        let conditions = vec![Condition {
            field: ConditionField::CountryCode,
            operator: ConditionOperator::Eq,
            value: json!("US"),
            value_end: None,
            app_id: None,
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_has_achievement() {
        let conditions = vec![Condition {
            field: ConditionField::HasAchievement,
            operator: ConditionOperator::Eq,
            value: json!("WIN_BOMB_DEFUSE"),
            value_end: None,
            app_id: Some("730".to_string()),
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_has_achievement_not_unlocked() {
        let conditions = vec![Condition {
            field: ConditionField::HasAchievement,
            operator: ConditionOperator::Eq,
            value: json!("UNSTOPPABLE_FORCE"),
            value_end: None,
            app_id: Some("730".to_string()),
        }];
        assert!(!evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_achievement_percent() {
        // 35/67 ≈ 52%
        let conditions = vec![Condition {
            field: ConditionField::AchievementPercent,
            operator: ConditionOperator::Gte,
            value: json!(50),
            value_end: None,
            app_id: Some("730".to_string()),
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_between() {
        let conditions = vec![Condition {
            field: ConditionField::SteamLevel,
            operator: ConditionOperator::Between,
            value: json!(20),
            value_end: Some(json!(30)),
            app_id: None,
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_account_age_days() {
        // ~3650 days old
        let conditions = vec![Condition {
            field: ConditionField::AccountAgeDays,
            operator: ConditionOperator::Gte,
            value: json!(365),
            value_end: None,
            app_id: None,
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_total_games_owned() {
        let conditions = vec![Condition {
            field: ConditionField::TotalGamesOwned,
            operator: ConditionOperator::Gte,
            value: json!(100),
            value_end: None,
            app_id: None,
        }];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    /// Cache row as it looks for a user whose game details are private:
    /// GetOwnedGames returned no games key, so the library is empty.
    fn private_library_user_cache() -> UserCacheRow {
        UserCacheRow {
            owned_games: json!([]),
            total_games_owned: 0,
            ..sample_user_cache()
        }
    }

    fn owns_game_condition(expected: bool) -> Vec<Condition> {
        vec![Condition {
            field: ConditionField::OwnsGame,
            operator: ConditionOperator::Eq,
            value: json!(expected),
            value_end: None,
            app_id: Some("730".to_string()),
        }]
    }

    fn publisher_ownership(
        owns_app: bool,
        permanent: bool,
        owner_steam_id: &str,
    ) -> HashMap<String, AppOwnershipRow> {
        let mut map = HashMap::new();
        map.insert(
            "730".to_string(),
            AppOwnershipRow {
                owns_app,
                permanent,
                owner_steam_id: owner_steam_id.to_string(),
            },
        );
        map
    }

    #[test]
    fn test_publisher_ownership_grants_despite_private_library() {
        let ownership = publisher_ownership(true, true, "76561198012345678");
        assert!(evaluate_conditions(
            &owns_game_condition(true),
            &private_library_user_cache(),
            &sample_achievements(),
            Some(&ownership)
        ));
    }

    #[test]
    fn test_publisher_ownership_overrides_stale_library() {
        // Library still lists the game (stale cache) but the publisher
        // check says the license is gone (refund/revocation).
        let ownership = publisher_ownership(false, true, "76561198012345678");
        assert!(!evaluate_conditions(
            &owns_game_condition(true),
            &sample_user_cache(),
            &sample_achievements(),
            Some(&ownership)
        ));
    }

    #[test]
    fn test_publisher_ownership_excludes_family_sharing() {
        // Owned, but the license belongs to another account.
        let ownership = publisher_ownership(true, true, "76561198099999999");
        assert!(!evaluate_conditions(
            &owns_game_condition(true),
            &private_library_user_cache(),
            &sample_achievements(),
            Some(&ownership)
        ));
        // The inverted condition ("does NOT own") therefore matches.
        assert!(evaluate_conditions(
            &owns_game_condition(false),
            &private_library_user_cache(),
            &sample_achievements(),
            Some(&ownership)
        ));
    }

    #[test]
    fn test_publisher_ownership_excludes_temporary_license() {
        // Free weekend: ownsapp=true but permanent=false.
        let ownership = publisher_ownership(true, false, "76561198012345678");
        assert!(!evaluate_conditions(
            &owns_game_condition(true),
            &private_library_user_cache(),
            &sample_achievements(),
            Some(&ownership)
        ));
    }

    #[test]
    fn test_publisher_ownership_missing_app_falls_back_to_library() {
        // Publisher key configured but no cached result for this app yet —
        // fall back to the public library, which has the game.
        let ownership: HashMap<String, AppOwnershipRow> = HashMap::new();
        assert!(evaluate_conditions(
            &owns_game_condition(true),
            &sample_user_cache(),
            &sample_achievements(),
            Some(&ownership)
        ));
    }

    #[test]
    fn test_no_publisher_key_ignores_ownership_data() {
        // Without a publisher key the caller passes None; a private
        // library means the game can't be seen, so no role.
        assert!(!evaluate_conditions(
            &owns_game_condition(true),
            &private_library_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_empty_conditions() {
        let conditions: Vec<Condition> = vec![];
        assert!(!evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }

    #[test]
    fn test_multiple_conditions_and() {
        let conditions = vec![
            Condition {
                field: ConditionField::OwnsGame,
                operator: ConditionOperator::Eq,
                value: json!(true),
                value_end: None,
                app_id: Some("730".to_string()),
            },
            Condition {
                field: ConditionField::SteamLevel,
                operator: ConditionOperator::Gte,
                value: json!(10),
                value_end: None,
                app_id: None,
            },
        ];
        assert!(evaluate_conditions(
            &conditions,
            &sample_user_cache(),
            &sample_achievements(),
            None
        ));
    }
}

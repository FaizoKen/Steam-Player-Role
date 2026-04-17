use serde_json::{json, Value};
use std::collections::HashMap;

use crate::error::AppError;
use crate::models::condition::{Condition, ConditionField, ConditionOperator};

const NUMERIC_GAME_FIELDS: &[&str] = &[
    "gamePlaytime",
    "recentPlaytime",
    "achievementCount",
    "achievementPercent",
];

const NUMERIC_ACCOUNT_FIELDS: &[&str] = &["steamLevel", "accountAgeDays", "totalGamesOwned"];

pub fn build_config_schema(
    conditions: &[Condition],
    verify_url: &str,
    players_url: &str,
    view_permission: &str,
) -> Value {
    let c = conditions.first();

    let mut values = HashMap::new();

    let category = c
        .map(|c| {
            if c.field.requires_app_id() {
                "game"
            } else {
                "account"
            }
        })
        .unwrap_or("");

    values.insert("condition_category".to_string(), json!(category));

    // Split field key into category-specific slot so switching category
    // does not fight a stale value.
    values.insert(
        "condition_field_game".to_string(),
        json!(if category == "game" {
            c.map(|c| c.field.json_key()).unwrap_or("")
        } else {
            ""
        }),
    );
    values.insert(
        "condition_field_account".to_string(),
        json!(if category == "account" {
            c.map(|c| c.field.json_key()).unwrap_or("")
        } else {
            ""
        }),
    );

    let operator_key = c.map(|c| c.operator.key()).unwrap_or("gte");
    values.insert(
        "operator_game".to_string(),
        json!(if category == "game" { operator_key } else { "gte" }),
    );
    values.insert(
        "operator_account".to_string(),
        json!(if category == "account" { operator_key } else { "gte" }),
    );
    values.insert("view_permission".to_string(), json!(view_permission));

    if let Some(c) = c {
        if let Some(app_id) = &c.app_id {
            values.insert("app_id".to_string(), json!(app_id));
        }

        match &c.field {
            ConditionField::OwnsGame => {
                let bool_val = c.value.as_bool().unwrap_or(true);
                values.insert(
                    "value_bool_game".to_string(),
                    json!(if bool_val { "true" } else { "false" }),
                );
            }
            ConditionField::IsVACBanned | ConditionField::IsGameBanned => {
                let bool_val = c.value.as_bool().unwrap_or(true);
                values.insert(
                    "value_bool_account".to_string(),
                    json!(if bool_val { "true" } else { "false" }),
                );
            }
            ConditionField::HasAchievement => {
                let str_val = c.value.as_str().unwrap_or("");
                values.insert("value_achievement".to_string(), json!(str_val));
            }
            ConditionField::InGroup => {
                let str_val = c.value.as_str().unwrap_or("");
                values.insert("value_group".to_string(), json!(str_val));
            }
            ConditionField::CountryCode => {
                let str_val = c.value.as_str().unwrap_or("");
                values.insert("value_country".to_string(), json!(str_val));
            }
            _ => {
                if let Some(n) = c.value.as_i64() {
                    let key = if c.field.requires_app_id() {
                        "value_num_game"
                    } else {
                        "value_num_account"
                    };
                    values.insert(key.to_string(), json!(n));
                }
            }
        }

        if c.operator == ConditionOperator::Between {
            if let Some(end) = &c.value_end {
                if let Some(n) = end.as_i64() {
                    let key = if c.field.requires_app_id() {
                        "value_end_game"
                    } else {
                        "value_end_account"
                    };
                    values.insert(key.to_string(), json!(n));
                }
            }
        }
    }

    json!({
        "version": 1,
        "name": "Steam Player Roles",
        "description": "Assign a Discord role based on a member's Steam account — games owned, playtime, achievements, level, account status, and more.",
        "sections": [
            {
                "title": "How it works",
                "fields": [
                    {
                        "type": "display",
                        "key": "info",
                        "label": "Quick overview",
                        "value": format!(
                            "Three steps:\n\
                             \n\
                             1. Members link their Steam account at:\n\
                             {verify_url}\n\
                             \n\
                             2. You pick one condition below (for example: owns a specific game, played 100+ hours, Steam level 10+).\n\
                             \n\
                             3. Any verified member who matches the condition gets this Discord role automatically. Steam data refreshes on a schedule so roles stay current.\n\
                             \n\
                             Verified members for this server:\n\
                             {players_url}"
                        )
                    }
                ]
            },
            {
                "title": "Condition",
                "description": "Choose what the plugin should check. Pick a category first — the fields below adjust to match.",
                "fields": [
                    {
                        "type": "radio",
                        "key": "condition_category",
                        "label": "Category",
                        "description": "Game-specific checks look at a single Steam game (ownership, playtime, achievements). Account-level checks look at the Steam profile itself (level, age, bans, country).",
                        "validation": { "required": true },
                        "options": [
                            {"label": "Game-specific (needs a Steam App ID)", "value": "game"},
                            {"label": "Account-level (no App ID needed)", "value": "account"}
                        ]
                    },

                    // ---------- Game branch ----------
                    {
                        "type": "text",
                        "key": "app_id",
                        "label": "Steam App ID",
                        "description": "The numeric App ID of your game — e.g. 730 for CS2, 570 for Dota 2. Find it in the Steam store URL or Steamworks dashboard.",
                        "validation": { "pattern": "^[0-9]+$", "pattern_message": "App ID must be numeric", "required": true },
                        "condition": { "field": "condition_category", "equals": "game" }
                    },
                    {
                        "type": "select",
                        "key": "condition_field_game",
                        "label": "What to check",
                        "description": "Pick which game-specific data the plugin should evaluate.",
                        "validation": { "required": true },
                        "condition": { "field": "condition_category", "equals": "game" },
                        "options": [
                            {"label": "Owns the game", "value": "ownsGame"},
                            {"label": "Total playtime (hours)", "value": "gamePlaytime"},
                            {"label": "Recent playtime — last 2 weeks (hours)", "value": "recentPlaytime"},
                            {"label": "Achievement count", "value": "achievementCount"},
                            {"label": "Achievement completion %", "value": "achievementPercent"},
                            {"label": "Has a specific achievement", "value": "hasAchievement"}
                        ]
                    },
                    {
                        "type": "select",
                        "key": "operator_game",
                        "label": "Comparison",
                        "description": "How to compare the player's value against yours.",
                        "default_value": "gte",
                        "conditions": [
                            { "field": "condition_category", "equals": "game" },
                            { "field": "condition_field_game", "equals_any": NUMERIC_GAME_FIELDS }
                        ],
                        "options": [
                            {"label": "= equals", "value": "eq"},
                            {"label": "> greater than", "value": "gt"},
                            {"label": ">= at least", "value": "gte"},
                            {"label": "< less than", "value": "lt"},
                            {"label": "<= at most", "value": "lte"},
                            {"label": "↔ between (range)", "value": "between"}
                        ]
                    },
                    {
                        "type": "radio",
                        "key": "value_bool_game",
                        "label": "Must own the game?",
                        "default_value": "true",
                        "conditions": [
                            { "field": "condition_category", "equals": "game" },
                            { "field": "condition_field_game", "equals": "ownsGame" }
                        ],
                        "options": [
                            {"label": "Yes — grant role if they own it", "value": "true"},
                            {"label": "No — grant role if they do NOT own it", "value": "false"}
                        ]
                    },
                    {
                        "type": "number",
                        "key": "value_num_game",
                        "label": "Value",
                        "description": "The number to compare against (e.g. 100 for 100 hours).",
                        "validation": { "min": 0, "required": true },
                        "conditions": [
                            { "field": "condition_category", "equals": "game" },
                            { "field": "condition_field_game", "equals_any": NUMERIC_GAME_FIELDS }
                        ]
                    },
                    {
                        "type": "number",
                        "key": "value_end_game",
                        "label": "End value",
                        "description": "Upper bound of the range (inclusive).",
                        "validation": { "min": 0, "required": true },
                        "pair_with": "value_num_game",
                        "conditions": [
                            { "field": "condition_category", "equals": "game" },
                            { "field": "condition_field_game", "equals_any": NUMERIC_GAME_FIELDS },
                            { "field": "operator_game", "equals": "between" }
                        ]
                    },
                    {
                        "type": "text",
                        "key": "value_achievement",
                        "label": "Achievement API name",
                        "description": "The internal API name of the achievement — e.g. WIN_BOMB_DEFUSE. Check SteamDB or the Steamworks dashboard to find it.",
                        "validation": { "required": true },
                        "conditions": [
                            { "field": "condition_category", "equals": "game" },
                            { "field": "condition_field_game", "equals": "hasAchievement" }
                        ]
                    },

                    // ---------- Account branch ----------
                    {
                        "type": "select",
                        "key": "condition_field_account",
                        "label": "What to check",
                        "description": "Pick which account-level data the plugin should evaluate.",
                        "validation": { "required": true },
                        "condition": { "field": "condition_category", "equals": "account" },
                        "options": [
                            {"label": "Steam level", "value": "steamLevel"},
                            {"label": "Account age (days since creation)", "value": "accountAgeDays"},
                            {"label": "Total games owned", "value": "totalGamesOwned"},
                            {"label": "VAC banned", "value": "isVACBanned"},
                            {"label": "Game banned", "value": "isGameBanned"},
                            {"label": "Member of Steam group", "value": "inGroup"},
                            {"label": "Country code", "value": "countryCode"}
                        ]
                    },
                    {
                        "type": "select",
                        "key": "operator_account",
                        "label": "Comparison",
                        "description": "How to compare the player's value against yours.",
                        "default_value": "gte",
                        "conditions": [
                            { "field": "condition_category", "equals": "account" },
                            { "field": "condition_field_account", "equals_any": NUMERIC_ACCOUNT_FIELDS }
                        ],
                        "options": [
                            {"label": "= equals", "value": "eq"},
                            {"label": "> greater than", "value": "gt"},
                            {"label": ">= at least", "value": "gte"},
                            {"label": "< less than", "value": "lt"},
                            {"label": "<= at most", "value": "lte"},
                            {"label": "↔ between (range)", "value": "between"}
                        ]
                    },
                    {
                        "type": "radio",
                        "key": "value_bool_account",
                        "label": "Must be banned?",
                        "default_value": "false",
                        "conditions": [
                            { "field": "condition_category", "equals": "account" },
                            { "field": "condition_field_account", "equals_any": ["isVACBanned", "isGameBanned"] }
                        ],
                        "options": [
                            {"label": "Yes — grant role if banned", "value": "true"},
                            {"label": "No — grant role if NOT banned", "value": "false"}
                        ]
                    },
                    {
                        "type": "number",
                        "key": "value_num_account",
                        "label": "Value",
                        "description": "The number to compare against (e.g. 10 for Steam level 10).",
                        "validation": { "min": 0, "required": true },
                        "conditions": [
                            { "field": "condition_category", "equals": "account" },
                            { "field": "condition_field_account", "equals_any": NUMERIC_ACCOUNT_FIELDS }
                        ]
                    },
                    {
                        "type": "number",
                        "key": "value_end_account",
                        "label": "End value",
                        "description": "Upper bound of the range (inclusive).",
                        "validation": { "min": 0, "required": true },
                        "pair_with": "value_num_account",
                        "conditions": [
                            { "field": "condition_category", "equals": "account" },
                            { "field": "condition_field_account", "equals_any": NUMERIC_ACCOUNT_FIELDS },
                            { "field": "operator_account", "equals": "between" }
                        ]
                    },
                    {
                        "type": "text",
                        "key": "value_group",
                        "label": "Steam Group ID",
                        "description": "The numeric ID of the Steam group. Find it on the group page or via a SteamID lookup tool.",
                        "validation": { "required": true },
                        "conditions": [
                            { "field": "condition_category", "equals": "account" },
                            { "field": "condition_field_account", "equals": "inGroup" }
                        ]
                    },
                    {
                        "type": "text",
                        "key": "value_country",
                        "label": "Country code",
                        "description": "Two-letter ISO country code — e.g. US, DE, JP.",
                        "validation": { "pattern": "^[A-Za-z]{2}$", "pattern_message": "Must be a 2-letter country code", "required": true },
                        "conditions": [
                            { "field": "condition_category", "equals": "account" },
                            { "field": "condition_field_account", "equals": "countryCode" }
                        ]
                    }
                ]
            },
            {
                "title": "Player list access",
                "description": "Choose who can view the verified-player list for this server. This setting is shared across every role link in the server.",
                "fields": [
                    {
                        "type": "radio",
                        "key": "view_permission",
                        "label": "Who can view the player list",
                        "default_value": "members",
                        "options": [
                            {"label": "Anyone in the server", "value": "members"},
                            {"label": "Server managers only (Manage Server permission)", "value": "managers"}
                        ]
                    }
                ]
            },
            {
                "title": "Examples",
                "collapsible": true,
                "default_collapsed": true,
                "fields": [
                    {
                        "type": "display",
                        "key": "examples",
                        "label": "Common setups",
                        "value": "Owns your game → Category=Game, App ID=YOUR_APPID, Check=Owns the game, Value=Yes\n\
                                  Veteran (100+ hrs) → Category=Game, App ID=YOUR_APPID, Check=Total playtime, >= 100\n\
                                  Achievement hunter → Category=Game, Check=Achievement completion %, >= 50\n\
                                  No VAC ban → Category=Account, Check=VAC banned, No\n\
                                  Steam level 10+ → Category=Account, Check=Steam level, >= 10\n\
                                  1+ year old account → Category=Account, Check=Account age, >= 365\n\
                                  In a Steam group → Category=Account, Check=Member of Steam group, Value=GROUP_ID"
                    }
                ]
            }
        ],
        "values": values
    })
}

pub fn parse_config(config: &HashMap<String, Value>) -> Result<Vec<Condition>, AppError> {
    let category = config
        .get("condition_category")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (field_key, operator_key, num_key, end_key, bool_key) = match category {
        "game" => (
            config.get("condition_field_game").and_then(|v| v.as_str()).unwrap_or(""),
            config.get("operator_game").and_then(|v| v.as_str()).unwrap_or(""),
            "value_num_game",
            "value_end_game",
            "value_bool_game",
        ),
        "account" => (
            config.get("condition_field_account").and_then(|v| v.as_str()).unwrap_or(""),
            config.get("operator_account").and_then(|v| v.as_str()).unwrap_or(""),
            "value_num_account",
            "value_end_account",
            "value_bool_account",
        ),
        _ => {
            return Err(AppError::BadRequest(
                "Pick a category (Game-specific or Account-level)".into(),
            ));
        }
    };

    if field_key.is_empty() {
        return Err(AppError::BadRequest("Pick a condition type".into()));
    }

    let field = ConditionField::from_key(field_key)
        .ok_or_else(|| AppError::BadRequest(format!("Invalid condition field '{field_key}'")))?;

    if category == "game" && !field.requires_app_id() {
        return Err(AppError::BadRequest(
            "That condition is account-level — switch the category to Account.".into(),
        ));
    }
    if category == "account" && field.requires_app_id() {
        return Err(AppError::BadRequest(
            "That condition is game-specific — switch the category to Game.".into(),
        ));
    }

    let app_id = config
        .get("app_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if field.requires_app_id() && app_id.is_none() {
        return Err(AppError::BadRequest(format!(
            "Steam App ID is required for '{field_key}'"
        )));
    }

    let operator = if field.is_boolean() || field.is_string_exact() {
        ConditionOperator::Eq
    } else {
        if operator_key.is_empty() {
            return Err(AppError::BadRequest(
                "Pick a comparison (>=, =, between, …)".into(),
            ));
        }
        ConditionOperator::from_key(operator_key)
            .ok_or_else(|| AppError::BadRequest(format!("Invalid operator '{operator_key}'")))?
    };

    let value = if field.is_boolean() {
        let bool_str = config
            .get(bool_key)
            .and_then(|v| v.as_str())
            .unwrap_or("true");
        Value::Bool(bool_str == "true")
    } else if field.is_string_exact() {
        let value_key = match field {
            ConditionField::HasAchievement => "value_achievement",
            ConditionField::InGroup => "value_group",
            ConditionField::CountryCode => "value_country",
            _ => unreachable!(),
        };
        let text = config
            .get(value_key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            return Err(AppError::BadRequest("Value is required".into()));
        }
        if field == ConditionField::CountryCode {
            Value::String(text.to_uppercase())
        } else {
            Value::String(text)
        }
    } else {
        let raw = config.get(num_key);
        let n = raw
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .ok_or_else(|| AppError::BadRequest("Numeric value is required".into()))?;
        Value::Number(n.into())
    };

    let value_end = if operator == ConditionOperator::Between {
        let raw = config.get(end_key);
        let n = raw
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .ok_or_else(|| {
                AppError::BadRequest("End value is required for the between operator".into())
            })?;

        if let Some(start) = value.as_i64() {
            if start > n {
                return Err(AppError::BadRequest(
                    "Start value must be less than or equal to end value".into(),
                ));
            }
        }

        Some(Value::Number(n.into()))
    } else {
        None
    };

    Ok(vec![Condition {
        field,
        operator,
        value,
        value_end,
        app_id,
    }])
}

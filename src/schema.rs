use serde_json::{json, Value};
use std::collections::HashMap;

use crate::error::AppError;
use crate::models::condition::{Condition, ConditionField, ConditionOperator};

/// Placeholder echoed for a stored publisher key instead of the key itself
/// — the real key never leaves the plugin. A save carrying this exact
/// value means "keep the stored key".
pub const SECRET_MASK: &str = "••••••••";

/// Build the iframe-mode response returned by GET /config. RoleLogic
/// appends `?rl_token=<jwt>` to `embed_url` before rendering the iframe;
/// the admin page verifies that token locally to authenticate the admin.
pub fn build_iframe_config(base_url: &str, guild_id: &str, role_id: &str) -> Value {
    let embed_url = format!("{base_url}/admin/{guild_id}/role/{role_id}");
    json!({
        "version": 1,
        "ui_mode": "iframe",
        "name": "Steam Player Roles",
        "description": "Assign a Discord role based on a member's Steam account — games owned, playtime, achievements, level, account status, and more.",
        "embed_url": embed_url,
        // We honor read_only impersonation tokens (writes are blocked
        // server-side), so RoleLogic may hand us a read-only token for viewing.
        "supports_impersonation_readonly": true,
    })
}

/// POST /config is unreachable in iframe mode — the RoleLogic backend
/// rejects it before forwarding — but the contract still expects 200 on the
/// off chance an older backend forwards a call. Token has already been
/// verified in the handler.
pub fn accept_empty_config() -> Value {
    json!({ "success": true })
}

pub fn parse_config(config: &HashMap<String, Value>) -> Result<Vec<Condition>, AppError> {
    let category = config
        .get("condition_category")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (field_key, operator_key, num_key, end_key, bool_key) = match category {
        "game" => (
            config
                .get("condition_field_game")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            config
                .get("operator_game")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "value_num_game",
            "value_end_game",
            "value_bool_game",
        ),
        "account" => (
            config
                .get("condition_field_account")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            config
                .get("operator_account")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
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
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .ok_or_else(|| AppError::BadRequest("Numeric value is required".into()))?;
        Value::Number(n.into())
    };

    let value_end = if operator == ConditionOperator::Between {
        let raw = config.get(end_key);
        let n = raw
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
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

/// What POST /config should do with the stored publisher key.
#[derive(Debug, PartialEq)]
pub enum PublisherKeyAction {
    Keep,
    Clear,
    Set(String),
}

pub fn parse_publisher_key(
    config: &HashMap<String, Value>,
) -> Result<PublisherKeyAction, AppError> {
    let raw = match config.get("publisher_key").and_then(|v| v.as_str()) {
        // Field absent (e.g. hidden for account-level conditions) —
        // leave any stored key alone.
        None => return Ok(PublisherKeyAction::Keep),
        Some(s) => s.trim(),
    };
    if raw == SECRET_MASK {
        return Ok(PublisherKeyAction::Keep);
    }
    if raw.is_empty() {
        return Ok(PublisherKeyAction::Clear);
    }
    if raw.len() != 32 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::BadRequest(
            "Publisher key must be the 32-character hexadecimal Web API key from the Steamworks partner site".into(),
        ));
    }
    Ok(PublisherKeyAction::Set(raw.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_key(key: &str) -> HashMap<String, Value> {
        let mut config = HashMap::new();
        config.insert("publisher_key".to_string(), json!(key));
        config
    }

    #[test]
    fn test_publisher_key_absent_keeps() {
        let config = HashMap::new();
        assert_eq!(
            parse_publisher_key(&config).unwrap(),
            PublisherKeyAction::Keep
        );
    }

    #[test]
    fn test_publisher_key_mask_keeps() {
        assert_eq!(
            parse_publisher_key(&config_with_key(SECRET_MASK)).unwrap(),
            PublisherKeyAction::Keep
        );
    }

    #[test]
    fn test_publisher_key_empty_clears() {
        assert_eq!(
            parse_publisher_key(&config_with_key("  ")).unwrap(),
            PublisherKeyAction::Clear
        );
    }

    #[test]
    fn test_publisher_key_valid_sets() {
        let key = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(
            parse_publisher_key(&config_with_key(key)).unwrap(),
            PublisherKeyAction::Set(key.to_string())
        );
    }

    #[test]
    fn test_publisher_key_invalid_rejected() {
        assert!(parse_publisher_key(&config_with_key("not-a-key")).is_err());
        assert!(parse_publisher_key(&config_with_key("0123456789ABCDEF")).is_err());
    }
}

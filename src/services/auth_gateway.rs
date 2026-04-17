//! Server-to-server client for the centralized Auth Gateway's
//! `/auth/internal/*` endpoints.

use serde::Deserialize;

use crate::error::AppError;

#[derive(Debug, Deserialize)]
struct UserGuildIdsResponse {
    guild_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GuildMemberIdsResponse {
    discord_ids: Vec<String>,
}

/// `GET /auth/internal/user_guild_ids?discord_id=...` → list of guild IDs
pub async fn fetch_user_guild_ids(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    discord_id: &str,
) -> Result<Vec<String>, AppError> {
    let url = format!("{base}/auth/internal/user_guild_ids");
    let resp = http
        .get(&url)
        .header("X-Internal-Key", key)
        .query(&[("discord_id", discord_id)])
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("auth_gateway request failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "auth_gateway user_guild_ids returned {status}: {body}"
        )));
    }

    let parsed: UserGuildIdsResponse = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("auth_gateway response not JSON: {e}")))?;
    Ok(parsed.guild_ids)
}

/// `GET /auth/internal/guild_member_ids?guild_id=...` → list of Discord IDs
pub async fn fetch_guild_member_ids(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    guild_id: &str,
) -> Result<Vec<String>, AppError> {
    let url = format!("{base}/auth/internal/guild_member_ids");
    let resp = http
        .get(&url)
        .header("X-Internal-Key", key)
        .query(&[("guild_id", guild_id)])
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("auth_gateway request failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "auth_gateway guild_member_ids returned {status}: {body}"
        )));
    }

    let parsed: GuildMemberIdsResponse = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("auth_gateway response not JSON: {e}")))?;
    Ok(parsed.discord_ids)
}

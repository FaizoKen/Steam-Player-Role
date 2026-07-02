//! Steam Web API client.
//!
//! Every request passes through the [`QuotaGovernor`](crate::services::quota)
//! *inside* this client — call sites can't bypass the daily budget. Callers
//! tag each call with a [`Class`]: `Interactive` for something a user is
//! actively waiting on (link-time lookups, a fresh link's first refresh),
//! `Background` for routine re-checks, which are paced smoothly across the
//! quota-day. On budget exhaustion or a Steam 429 the client returns
//! [`AppError::QuotaExhausted`] so workers can requeue rows instead of
//! erroring them into failure backoff.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::quota::{Class, Outcome, QuotaGovernor, DEFAULT_THROTTLE_SECS};

/// Max Steam IDs per GetPlayerSummaries / GetPlayerBans request.
pub const SUMMARY_BATCH_MAX: usize = 100;

pub struct SteamApiClient {
    http: reqwest::Client,
    api_key: String,
    /// Budget for the plugin's own Web API key. Publisher-key calls use the
    /// per-key governor passed to [`Self::check_app_ownership`] instead.
    quota: Arc<QuotaGovernor>,
}

// ── Response types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct PlayerSummary {
    pub steamid: String,
    pub personaname: Option<String>,
    pub profileurl: Option<String>,
    pub avatar: Option<String>,
    pub avatarmedium: Option<String>,
    pub timecreated: Option<i64>,
    pub lastlogoff: Option<i64>,
    pub communityvisibilitystate: Option<i32>,
    pub personastate: Option<i32>,
    pub loccountrycode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlayerSummariesResponse {
    response: PlayerSummariesInner,
}
#[derive(Debug, Deserialize)]
struct PlayerSummariesInner {
    players: Vec<PlayerSummary>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PlayerBan {
    #[serde(rename = "SteamId")]
    pub steam_id: String,
    #[serde(rename = "VACBanned")]
    pub vac_banned: bool,
    #[serde(rename = "NumberOfVACBans")]
    pub number_of_vac_bans: i32,
    #[serde(rename = "DaysSinceLastBan")]
    pub days_since_last_ban: i32,
    #[serde(rename = "NumberOfGameBans")]
    pub number_of_game_bans: i32,
    #[serde(rename = "CommunityBanned")]
    pub community_banned: bool,
    #[serde(rename = "EconomyBan")]
    pub economy_ban: String,
}

#[derive(Debug, Deserialize)]
struct PlayerBansResponse {
    players: Vec<PlayerBan>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OwnedGame {
    pub appid: i64,
    pub name: Option<String>,
    pub playtime_forever: Option<i64>,
    pub playtime_2weeks: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct OwnedGamesResponse {
    response: OwnedGamesInner,
}
#[derive(Debug, Deserialize)]
struct OwnedGamesInner {
    game_count: Option<i64>,
    games: Option<Vec<OwnedGame>>,
}

pub struct OwnedGamesResult {
    pub games: Vec<OwnedGame>,
    pub game_count: i64,
    /// False when Steam omitted the `games` key entirely — the user's
    /// "game details" privacy setting is hidden. A public-but-empty
    /// library still returns `games: []`.
    pub library_visible: bool,
}

#[derive(Debug, Deserialize)]
struct AppOwnershipResponse {
    appownership: AppOwnershipInner,
}
#[derive(Debug, Deserialize)]
struct AppOwnershipInner {
    ownsapp: bool,
    permanent: bool,
    ownersteamid: String,
}

pub struct AppOwnership {
    pub owns_app: bool,
    /// False for temporary licenses (free weekends, timed trials).
    pub permanent: bool,
    /// The account that holds the license — differs from the checked
    /// steam_id when the game is borrowed via Family Sharing.
    pub owner_steam_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Achievement {
    pub apiname: String,
    pub achieved: i32,
    pub unlocktime: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AchievementsResponse {
    playerstats: AchievementsInner,
}
#[derive(Debug, Deserialize)]
struct AchievementsInner {
    #[allow(dead_code)]
    #[serde(rename = "gameName")]
    game_name: Option<String>,
    achievements: Option<Vec<Achievement>>,
    success: bool,
}

#[derive(Debug, Deserialize)]
struct SteamLevelResponse {
    response: SteamLevelInner,
}
#[derive(Debug, Deserialize)]
struct SteamLevelInner {
    player_level: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SteamGroup {
    pub gid: String,
}

#[derive(Debug, Deserialize)]
struct GroupListResponse {
    response: GroupListInner,
}
#[derive(Debug, Deserialize)]
struct GroupListInner {
    success: bool,
    groups: Option<Vec<SteamGroup>>,
}

// ── Client ──────────────────────────────────────────────────────────────

/// Seconds from a 429's Retry-After header, if parseable.
fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

impl SteamApiClient {
    pub fn new(api_key: &str, quota: Arc<QuotaGovernor>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            api_key: api_key.to_string(),
            quota,
        }
    }

    /// Acquire budget, perform the GET, and normalize throttling. URLs carry
    /// API keys, so errors never include them.
    async fn checked_get(
        &self,
        quota: &QuotaGovernor,
        class: Class,
        url: &str,
    ) -> Result<reqwest::Response, AppError> {
        if let Outcome::Exhausted { retry_after } = quota.acquire(class).await {
            return Err(AppError::QuotaExhausted {
                retry_after_secs: retry_after.as_secs().max(1),
            });
        }

        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| AppError::SteamApi(e.without_url().to_string()))?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let secs = retry_after_secs(&resp).unwrap_or(DEFAULT_THROTTLE_SECS);
            quota.mark_throttled(secs).await;
            return Err(AppError::QuotaExhausted {
                retry_after_secs: secs,
            });
        }
        Ok(resp)
    }

    /// Batch up to [`SUMMARY_BATCH_MAX`] steam IDs — one quota unit per call.
    pub async fn get_player_summaries(
        &self,
        steam_ids: &[&str],
        class: Class,
    ) -> Result<Vec<PlayerSummary>, AppError> {
        if steam_ids.is_empty() {
            return Ok(vec![]);
        }
        let ids = steam_ids.join(",");
        let url = format!(
            "https://api.steampowered.com/ISteamUser/GetPlayerSummaries/v2/?key={}&steamids={}",
            self.api_key, ids
        );
        let resp: PlayerSummariesResponse = self
            .checked_get(&self.quota, class, &url)
            .await?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse player summaries: {e}")))?;
        Ok(resp.response.players)
    }

    /// Batch up to [`SUMMARY_BATCH_MAX`] steam IDs — one quota unit per call.
    pub async fn get_player_bans(
        &self,
        steam_ids: &[&str],
        class: Class,
    ) -> Result<Vec<PlayerBan>, AppError> {
        if steam_ids.is_empty() {
            return Ok(vec![]);
        }
        let ids = steam_ids.join(",");
        let url = format!(
            "https://api.steampowered.com/ISteamUser/GetPlayerBans/v1/?key={}&steamids={}",
            self.api_key, ids
        );
        let resp: PlayerBansResponse = self
            .checked_get(&self.quota, class, &url)
            .await?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse player bans: {e}")))?;
        Ok(resp.players)
    }

    pub async fn get_steam_level(&self, steam_id: &str, class: Class) -> Result<i32, AppError> {
        let url = format!(
            "https://api.steampowered.com/IPlayerService/GetSteamLevel/v1/?key={}&steamid={}",
            self.api_key, steam_id
        );
        let resp: SteamLevelResponse = self
            .checked_get(&self.quota, class, &url)
            .await?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse steam level: {e}")))?;
        Ok(resp.response.player_level.unwrap_or(0))
    }

    pub async fn get_owned_games(
        &self,
        steam_id: &str,
        class: Class,
    ) -> Result<OwnedGamesResult, AppError> {
        let url = format!(
            "https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={}&steamid={}&include_appinfo=1&include_played_free_games=1",
            self.api_key, steam_id
        );
        let resp: OwnedGamesResponse = self
            .checked_get(&self.quota, class, &url)
            .await?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse owned games: {e}")))?;
        let library_visible = resp.response.games.is_some();
        Ok(OwnedGamesResult {
            games: resp.response.games.unwrap_or_default(),
            game_count: resp.response.game_count.unwrap_or(0),
            library_visible,
        })
    }

    pub async fn get_player_achievements(
        &self,
        steam_id: &str,
        app_id: &str,
        class: Class,
    ) -> Result<Vec<Achievement>, AppError> {
        let url = format!(
            "https://api.steampowered.com/ISteamUserStats/GetPlayerAchievements/v1/?key={}&steamid={}&appid={}",
            self.api_key, steam_id, app_id
        );
        let resp = self.checked_get(&self.quota, class, &url).await?;

        // 400 means game has no achievements or is invalid
        if resp.status() == reqwest::StatusCode::BAD_REQUEST {
            return Ok(vec![]);
        }

        let parsed: AchievementsResponse = resp
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse achievements: {e}")))?;

        if !parsed.playerstats.success {
            return Ok(vec![]);
        }
        Ok(parsed.playerstats.achievements.unwrap_or_default())
    }

    pub async fn get_user_group_list(
        &self,
        steam_id: &str,
        class: Class,
    ) -> Result<Vec<SteamGroup>, AppError> {
        let url = format!(
            "https://api.steampowered.com/ISteamUser/GetUserGroupList/v1/?key={}&steamid={}",
            self.api_key, steam_id
        );
        let resp: GroupListResponse = self
            .checked_get(&self.quota, class, &url)
            .await?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse group list: {e}")))?;

        if !resp.response.success {
            return Ok(vec![]);
        }
        Ok(resp.response.groups.unwrap_or_default())
    }

    /// Steamworks partner-API ownership check. Requires a publisher key
    /// issued for the app; authoritative even when the user's library is
    /// private. Draws from `quota` — the per-publisher-key governor, since
    /// each publisher key has its own daily allowance independent of ours.
    pub async fn check_app_ownership(
        &self,
        steam_id: &str,
        app_id: &str,
        publisher_key: &str,
        quota: &QuotaGovernor,
    ) -> Result<AppOwnership, AppError> {
        let url = format!(
            "https://partner.steam-api.com/ISteamUser/CheckAppOwnership/v2/?key={}&steamid={}&appid={}",
            publisher_key, steam_id, app_id
        );
        let resp = self.checked_get(quota, Class::Background, &url).await?;

        if resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(AppError::SteamApi(
                "CheckAppOwnership rejected the publisher key (403) — the key must be a Steamworks publisher key with access to this app".into(),
            ));
        }
        if !resp.status().is_success() {
            return Err(AppError::SteamApi(format!(
                "CheckAppOwnership returned HTTP {}",
                resp.status()
            )));
        }

        let parsed: AppOwnershipResponse = resp
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse app ownership: {e}")))?;

        Ok(AppOwnership {
            owns_app: parsed.appownership.ownsapp,
            permanent: parsed.appownership.permanent,
            owner_steam_id: parsed.appownership.ownersteamid,
        })
    }
}

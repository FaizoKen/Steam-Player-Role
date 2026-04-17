use std::num::NonZeroU32;
use std::sync::Arc;

use governor::{Quota, RateLimiter};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

type GovernorLimiter = RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

pub struct SteamApiClient {
    http: reqwest::Client,
    api_key: String,
    rate_limiter: Arc<GovernorLimiter>,
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
    #[allow(dead_code)]
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

impl SteamApiClient {
    pub fn new(api_key: &str, max_requests_per_hour: u32) -> Self {
        let per_second = std::cmp::max(1, max_requests_per_hour / 3600);
        let quota = Quota::per_second(NonZeroU32::new(per_second).unwrap());
        let limiter = RateLimiter::direct(quota);

        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            api_key: api_key.to_string(),
            rate_limiter: Arc::new(limiter),
        }
    }

    async fn wait_rate_limit(&self) {
        self.rate_limiter.until_ready().await;
    }

    /// Batch up to 100 steam IDs
    pub async fn get_player_summaries(
        &self,
        steam_ids: &[&str],
    ) -> Result<Vec<PlayerSummary>, AppError> {
        if steam_ids.is_empty() {
            return Ok(vec![]);
        }
        self.wait_rate_limit().await;
        let ids = steam_ids.join(",");
        let url = format!(
            "https://api.steampowered.com/ISteamUser/GetPlayerSummaries/v2/?key={}&steamids={}",
            self.api_key, ids
        );
        let resp: PlayerSummariesResponse = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::SteamApi(e.to_string()))?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse player summaries: {e}")))?;
        Ok(resp.response.players)
    }

    /// Batch up to 100 steam IDs
    pub async fn get_player_bans(
        &self,
        steam_ids: &[&str],
    ) -> Result<Vec<PlayerBan>, AppError> {
        if steam_ids.is_empty() {
            return Ok(vec![]);
        }
        self.wait_rate_limit().await;
        let ids = steam_ids.join(",");
        let url = format!(
            "https://api.steampowered.com/ISteamUser/GetPlayerBans/v1/?key={}&steamids={}",
            self.api_key, ids
        );
        let resp: PlayerBansResponse = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::SteamApi(e.to_string()))?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse player bans: {e}")))?;
        Ok(resp.players)
    }

    pub async fn get_steam_level(&self, steam_id: &str) -> Result<i32, AppError> {
        self.wait_rate_limit().await;
        let url = format!(
            "https://api.steampowered.com/IPlayerService/GetSteamLevel/v1/?key={}&steamid={}",
            self.api_key, steam_id
        );
        let resp: SteamLevelResponse = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::SteamApi(e.to_string()))?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse steam level: {e}")))?;
        Ok(resp.response.player_level.unwrap_or(0))
    }

    pub async fn get_owned_games(
        &self,
        steam_id: &str,
    ) -> Result<(Vec<OwnedGame>, i64), AppError> {
        self.wait_rate_limit().await;
        let url = format!(
            "https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={}&steamid={}&include_appinfo=1&include_played_free_games=1",
            self.api_key, steam_id
        );
        let resp: OwnedGamesResponse = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::SteamApi(e.to_string()))?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse owned games: {e}")))?;
        let count = resp.response.game_count.unwrap_or(0);
        let games = resp.response.games.unwrap_or_default();
        Ok((games, count))
    }

    pub async fn get_player_achievements(
        &self,
        steam_id: &str,
        app_id: &str,
    ) -> Result<Vec<Achievement>, AppError> {
        self.wait_rate_limit().await;
        let url = format!(
            "https://api.steampowered.com/ISteamUserStats/GetPlayerAchievements/v1/?key={}&steamid={}&appid={}",
            self.api_key, steam_id, app_id
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::SteamApi(e.to_string()))?;

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
    ) -> Result<Vec<SteamGroup>, AppError> {
        self.wait_rate_limit().await;
        let url = format!(
            "https://api.steampowered.com/ISteamUser/GetUserGroupList/v1/?key={}&steamid={}",
            self.api_key, steam_id
        );
        let resp: GroupListResponse = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::SteamApi(e.to_string()))?
            .json()
            .await
            .map_err(|e| AppError::SteamApi(format!("Failed to parse group list: {e}")))?;

        if !resp.response.success {
            return Ok(vec![]);
        }
        Ok(resp.response.groups.unwrap_or_default())
    }
}

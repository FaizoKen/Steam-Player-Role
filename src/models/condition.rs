use serde::{Deserialize, Serialize};

/// A Steam group's 64-bit ID is this base plus the group's 32-bit account
/// ID. GetUserGroupList answers with the short form ("1523005"); group pages,
/// `memberslistxml` and lookup tools show the long one
/// ("103582791431044413"). Both name the same group.
pub const GROUP_ID64_BASE: u64 = 103_582_791_429_521_408;

/// The short (account-ID) spelling of a Steam group ID, from either form.
/// Anything that isn't a group ID comes back trimmed and otherwise as is, so
/// it simply matches nothing.
pub fn group_account_id(raw: &str) -> String {
    let s = raw.trim();
    match s.parse::<u64>() {
        Ok(v) if v >= GROUP_ID64_BASE && v - GROUP_ID64_BASE <= u32::MAX as u64 => {
            (v - GROUP_ID64_BASE).to_string()
        }
        Ok(v) => v.to_string(),
        Err(_) => s.to_string(),
    }
}

/// Both spellings of a Steam group ID — `(short, long)` — so a lookup
/// matches whichever form Steam stored.
pub fn group_id_forms(raw: &str) -> (String, String) {
    let short = group_account_id(raw);
    let long = short
        .parse::<u64>()
        .ok()
        .filter(|v| *v <= u32::MAX as u64)
        .map(|v| (v + GROUP_ID64_BASE).to_string())
        .unwrap_or_else(|| short.clone());
    (short, long)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ConditionField {
    // Game-specific (require app_id)
    OwnsGame,
    GamePlaytime,
    RecentPlaytime,
    AchievementCount,
    AchievementPercent,
    HasAchievement,
    // Account-level
    SteamLevel,
    AccountAgeDays,
    TotalGamesOwned,
    IsVACBanned,
    IsGameBanned,
    InGroup,
    CountryCode,
}

impl ConditionField {
    pub fn is_boolean(&self) -> bool {
        matches!(
            self,
            Self::OwnsGame | Self::IsVACBanned | Self::IsGameBanned
        )
    }

    pub fn is_string_exact(&self) -> bool {
        matches!(
            self,
            Self::HasAchievement | Self::InGroup | Self::CountryCode
        )
    }

    pub fn is_numeric(&self) -> bool {
        !self.is_boolean() && !self.is_string_exact()
    }

    pub fn requires_app_id(&self) -> bool {
        matches!(
            self,
            Self::OwnsGame
                | Self::GamePlaytime
                | Self::RecentPlaytime
                | Self::AchievementCount
                | Self::AchievementPercent
                | Self::HasAchievement
        )
    }

    pub fn json_key(&self) -> &'static str {
        match self {
            Self::OwnsGame => "ownsGame",
            Self::GamePlaytime => "gamePlaytime",
            Self::RecentPlaytime => "recentPlaytime",
            Self::AchievementCount => "achievementCount",
            Self::AchievementPercent => "achievementPercent",
            Self::HasAchievement => "hasAchievement",
            Self::SteamLevel => "steamLevel",
            Self::AccountAgeDays => "accountAgeDays",
            Self::TotalGamesOwned => "totalGamesOwned",
            Self::IsVACBanned => "isVACBanned",
            Self::IsGameBanned => "isGameBanned",
            Self::InGroup => "inGroup",
            Self::CountryCode => "countryCode",
        }
    }

    /// Returns the PostgreSQL column name for denormalized fields,
    /// or None for fields that require JSONB/achievement queries.
    pub fn sql_column(&self) -> Option<&'static str> {
        match self {
            Self::SteamLevel => Some("uc.steam_level"),
            Self::TotalGamesOwned => Some("uc.total_games_owned"),
            Self::IsVACBanned => Some("uc.is_vac_banned"),
            Self::IsGameBanned => Some("uc.is_game_banned"),
            Self::CountryCode => Some("uc.country_code"),
            // AccountAgeDays computed from account_created
            // Game-specific fields require JSONB or achievement table
            _ => None,
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "ownsGame" => Some(Self::OwnsGame),
            "gamePlaytime" => Some(Self::GamePlaytime),
            "recentPlaytime" => Some(Self::RecentPlaytime),
            "achievementCount" => Some(Self::AchievementCount),
            "achievementPercent" => Some(Self::AchievementPercent),
            "hasAchievement" => Some(Self::HasAchievement),
            "steamLevel" => Some(Self::SteamLevel),
            "accountAgeDays" => Some(Self::AccountAgeDays),
            "totalGamesOwned" => Some(Self::TotalGamesOwned),
            "isVACBanned" => Some(Self::IsVACBanned),
            "isGameBanned" => Some(Self::IsGameBanned),
            "inGroup" => Some(Self::InGroup),
            "countryCode" => Some(Self::CountryCode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConditionOperator {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
    Between,
}

impl ConditionOperator {
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "eq" => Some(Self::Eq),
            "gt" => Some(Self::Gt),
            "gte" => Some(Self::Gte),
            "lt" => Some(Self::Lt),
            "lte" => Some(Self::Lte),
            "between" => Some(Self::Between),
            _ => None,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Between => "between",
        }
    }

    pub fn sql_operator(&self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
            Self::Between => "BETWEEN",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    pub field: ConditionField,
    pub operator: ConditionOperator,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_end: Option<serde_json::Value>,
    /// Steam App ID for game-specific conditions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_ids_normalise_to_the_short_form() {
        assert_eq!(group_account_id("103582791431044413"), "1523005");
        assert_eq!(group_account_id(" 1523005 "), "1523005");
        assert_eq!(group_account_id("0001523005"), "1523005");
        assert_eq!(group_account_id("steamlug"), "steamlug");
        assert_eq!(
            group_id_forms("1523005"),
            ("1523005".to_string(), "103582791431044413".to_string())
        );
        assert_eq!(
            group_id_forms("103582791431044413"),
            ("1523005".to_string(), "103582791431044413".to_string())
        );
    }
}

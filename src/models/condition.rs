use serde::{Deserialize, Serialize};

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
        matches!(self, Self::OwnsGame | Self::IsVACBanned | Self::IsGameBanned)
    }

    pub fn is_string_exact(&self) -> bool {
        matches!(self, Self::HasAchievement | Self::InGroup | Self::CountryCode)
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

use std::env;

#[derive(Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub session_secret: String,
    pub base_url: String,
    pub listen_addr: String,
    pub auth_gateway_url: String,
    pub internal_api_key: String,
    /// Origin allowed to embed this plugin in an iframe. Used to build the
    /// `Content-Security-Policy: frame-ancestors …` header on the
    /// role-config page and added to the CSRF origin allowlist. Unset →
    /// permissive `*` (dev / self-hosted RoleLogic).
    pub rl_dashboard_origin: Option<String>,
    pub steam_api_key: String,
    /// Nominal daily Steam Web API call allowance for our key (Valve's ToU
    /// default is 100,000/day). The governor spends `× safety_fraction` of it.
    pub steam_api_daily_quota: i64,
    /// Fraction of the daily quota reserved for interactive (link-time)
    /// calls a user is actively waiting on. Background refreshes can't touch
    /// it, so a verify spike never starves real-time verification.
    pub quota_interactive_reserve: f64,
    /// Fraction of the nominal quota the governor will actually spend,
    /// leaving headroom for accounting skew / undocumented reset timing.
    pub quota_safety_fraction: f64,
    /// Number of background refresh workers. They share the governor's
    /// budget and partition rows by `hashtext(steam_id) % N`.
    pub refresh_workers: i64,
    /// Ceiling for the stability-stretched refresh interval. Churny users
    /// stay within 24h; long-stable users may stretch up to this.
    pub max_stable_refresh_secs: i64,
}

pub fn derive_origin(base_url: &str) -> String {
    if let Some(scheme_end) = base_url.find("://") {
        let after_scheme = scheme_end + 3;
        if let Some(path_slash) = base_url[after_scheme..].find('/') {
            return base_url[..after_scheme + path_slash].to_string();
        }
    }
    base_url.to_string()
}

impl AppConfig {
    pub fn from_env() -> Self {
        let base_url = env::var("BASE_URL").expect("BASE_URL must be set");
        let auth_gateway_url = env::var("AUTH_GATEWAY_URL")
            .ok()
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| derive_origin(&base_url));

        // STEAM_API_RATE_LIMIT (requests/hour) is the deprecated pre-governor
        // knob; honor it as a daily-quota fallback so existing deployments
        // keep their configured ceiling until they migrate.
        let legacy_daily = env::var("STEAM_API_RATE_LIMIT")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .map(|hourly| hourly.max(1) * 24);
        if legacy_daily.is_some() && env::var("STEAM_API_DAILY_QUOTA").is_err() {
            tracing::warn!(
                "STEAM_API_RATE_LIMIT is deprecated — set STEAM_API_DAILY_QUOTA instead"
            );
        }

        Self {
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            session_secret: env::var("SESSION_SECRET").expect("SESSION_SECRET must be set"),
            base_url,
            listen_addr: env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8088".to_string()),
            auth_gateway_url,
            internal_api_key: env::var("INTERNAL_API_KEY")
                .expect("INTERNAL_API_KEY must be set (must match the Auth Gateway's value)"),
            rl_dashboard_origin: env::var("RL_DASHBOARD_ORIGIN")
                .ok()
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty()),
            steam_api_key: env::var("STEAM_API_KEY").expect("STEAM_API_KEY must be set"),
            steam_api_daily_quota: env::var("STEAM_API_DAILY_QUOTA")
                .ok()
                .and_then(|v| v.parse().ok())
                .or(legacy_daily)
                .unwrap_or(100_000)
                .max(1),
            quota_interactive_reserve: env::var("QUOTA_INTERACTIVE_RESERVE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.20),
            quota_safety_fraction: env::var("QUOTA_SAFETY_FRACTION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.95),
            refresh_workers: env::var("REFRESH_WORKERS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .map(|n| n.clamp(1, 64))
                .unwrap_or(2),
            max_stable_refresh_secs: env::var("MAX_STABLE_REFRESH_SECS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(604_800)
                .max(86_400),
        }
    }
}

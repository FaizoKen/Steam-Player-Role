//! Central Steam Web API quota governor.
//!
//! Steam's Web API Terms of Use cap a key at **100,000 calls per day**, and
//! the expensive per-user endpoints (GetOwnedGames, GetSteamLevel,
//! GetUserGroupList, GetPlayerAchievements) have no batch form — so the daily
//! key budget is a hard ceiling shared by every user and every code path.
//!
//! Before this module the only guard was a naive per-second token bucket in
//! the API client: no daily ceiling (it couldn't even enforce rates below
//! 3600/hour), no persistence across restarts, and no separation between
//! background refreshes and link-time calls a user is actively waiting on.
//!
//! The governor is the *one* place every quota-costing call must pass
//! through (the API client acquires internally, so no call site can bypass
//! it). Ported from YouTube-Subscriber-Role's governor, with Steam-specific
//! changes:
//!
//!   - **UTC quota-day.** Valve doesn't document when the daily counter
//!     resets, so we bucket by UTC date and lean on the safety fraction for
//!     the skew.
//!   - **Scoped ledgers.** The plugin's own key ("main") and each Steamworks
//!     publisher key ("pub:<hash>") have independent daily allowances, so
//!     each gets its own governor and its own ledger rows in
//!     `api_quota_usage`.
//!   - **429 → short throttle, not day-long stop.** A Steam 429 usually means
//!     a burst/IP throttle rather than "daily budget gone", so
//!     `mark_throttled` pauses for the `Retry-After` (or a few minutes)
//!     instead of until reset. The self-imposed daily budget is what prevents
//!     genuine over-spend.
//!
//! Mechanics shared with the YouTube governor:
//!   - Tracks units spent in the current quota-day, persisted to
//!     `api_quota_usage` so a restart doesn't reset the counter and over-spend.
//!   - Splits the budget into an **interactive reserve** (link-time checks a
//!     user is actively waiting on) and a **background** pool (routine
//!     refreshes). Background can never eat the reserve; interactive may
//!     borrow idle background headroom.
//!   - **Paces** background calls smoothly across the whole day
//!     (`spacing = time_to_reset / remaining_background_budget`), so spend is
//!     a gentle trickle instead of bursts — no thundering herd, ever.
//!   - Stops *itself* before Steam has to (`Outcome::Exhausted`).
//!
//! Multi-instance note: `used` is reconciled against the DB on every flush via
//! an atomic delta-add, so N processes converge on the shared total within one
//! flush interval. Per-process pacing is independent, so for true horizontal
//! scale either run one refresh process or divide the configured quota across
//! instances. Single-process (the default) is exact.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::Mutex;

/// Which budget pool a call draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A user is actively waiting on this result (link-time check, fresh-link
    /// first fetch).
    Interactive,
    /// Routine background refresh.
    Background,
}

/// Result of requesting permission to spend one quota unit.
#[derive(Debug)]
pub enum Outcome {
    /// Go ahead — one unit has been reserved (and, for background, paced).
    Granted,
    /// No budget left in this pool; try again after `retry_after`.
    Exhausted { retry_after: StdDuration },
}

/// Longest a single background call will be paced to wait. Keeps the worker
/// responsive (and able to notice a quota bump) even when the trickle is slow.
const MAX_SPACING_MS: i64 = 60_000;

/// How often the in-memory counter is persisted to the durable ledger.
const FLUSH_INTERVAL: StdDuration = StdDuration::from_secs(10);

/// Short-term burst floor shared by all classes — politeness to the API and a
/// smoother retry profile. Far above the daily-budget trickle; it only clips
/// genuine bursts (a verify spike draining the interactive reserve).
const BURST_PER_SEC: u32 = 25;

/// Default pause after a Steam 429 without a Retry-After header.
pub const DEFAULT_THROTTLE_SECS: u64 = 300;

struct Inner {
    /// The UTC quota-day these counters belong to.
    date: NaiveDate,
    /// Units reserved so far today (interactive + background).
    used: i64,
    /// Units reserved but not yet written to the ledger.
    unflushed: i64,
    /// Earliest instant the next background call may proceed (pacing gate).
    bg_next_at: DateTime<Utc>,
    /// Hard pause set when Steam itself throttles us (HTTP 429).
    throttled_until: Option<DateTime<Utc>>,
}

pub struct QuotaGovernor {
    pool: PgPool,
    /// Ledger key: "main" for the plugin's Web API key, "pub:<hash>" per
    /// publisher key.
    scope: String,
    /// Usable daily budget = configured quota × safety fraction.
    total_budget: i64,
    /// Ceiling for background calls; the gap up to `total_budget` is the
    /// interactive-only reserve.
    background_budget: i64,
    inner: Mutex<Inner>,
    burst: governor::RateLimiter<
        governor::state::NotKeyed,
        governor::state::InMemoryState,
        governor::clock::DefaultClock,
    >,
}

/// A point-in-time view for health/observability.
#[derive(Debug, Clone)]
pub struct QuotaSnapshot {
    pub date: NaiveDate,
    pub used: i64,
    pub total_budget: i64,
    pub background_budget: i64,
    pub reset_in_secs: i64,
    pub throttled: bool,
}

impl QuotaSnapshot {
    pub fn remaining(&self) -> i64 {
        (self.total_budget - self.used).max(0)
    }
}

fn utc_date(now: DateTime<Utc>) -> NaiveDate {
    now.date_naive()
}

fn next_utc_reset(now: DateTime<Utc>) -> DateTime<Utc> {
    let tomorrow = now.date_naive() + Duration::days(1);
    tomorrow.and_hms_opt(0, 0, 0).unwrap().and_utc()
}

impl QuotaGovernor {
    /// Build a governor for one API key's budget, loading today's already-
    /// spent units from the durable ledger so a restart resumes accounting
    /// instead of resetting to zero.
    pub async fn new(
        pool: PgPool,
        scope: String,
        quota_per_day: i64,
        reserve_frac: f64,
        safety_frac: f64,
    ) -> Arc<Self> {
        let safety = safety_frac.clamp(0.5, 1.0);
        let reserve = reserve_frac.clamp(0.0, 0.9);
        let total_budget = ((quota_per_day as f64) * safety).floor().max(1.0) as i64;
        let background_budget = ((total_budget as f64) * (1.0 - reserve)).floor().max(1.0) as i64;

        let now = Utc::now();
        let date = utc_date(now);
        let used = sqlx::query_scalar::<_, i64>(
            "SELECT used_units FROM api_quota_usage WHERE quota_date = $1 AND scope = $2",
        )
        .bind(date)
        .bind(&scope)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0);

        // Burst cap scales with the budget: ~4× the average sustainable rate,
        // so it only clips genuine spikes and never becomes the throughput
        // ceiling when the quota is raised. Floored at BURST_PER_SEC.
        let burst_rate =
            (((total_budget * 4) / 86_400).max(BURST_PER_SEC as i64)).min(100_000) as u32;
        let burst_quota =
            governor::Quota::per_second(std::num::NonZeroU32::new(burst_rate.max(1)).unwrap());
        let burst = governor::RateLimiter::direct(burst_quota);

        tracing::info!(
            scope,
            total_budget,
            background_budget,
            interactive_reserve = total_budget - background_budget,
            used_today = used,
            "Quota governor initialized"
        );

        Arc::new(Self {
            pool,
            scope,
            total_budget,
            background_budget,
            inner: Mutex::new(Inner {
                date,
                used,
                unflushed: 0,
                bg_next_at: now,
                throttled_until: None,
            }),
            burst,
        })
    }

    /// Roll the in-memory counters over when the UTC day changes. The prior
    /// day's final tally is already in the ledger from the last flush.
    fn roll_over(&self, g: &mut Inner, now: DateTime<Utc>) {
        let today = utc_date(now);
        if g.date != today {
            g.date = today;
            g.used = 0;
            g.unflushed = 0;
            g.bg_next_at = now;
            g.throttled_until = None;
        }
    }

    /// Request permission to spend one quota unit. On `Granted`, exactly one
    /// unit has been reserved; background calls additionally sleep to honor
    /// the daily pacing. Reserve-on-grant (no refund) keeps concurrent
    /// interactive callers from over-committing — a rare failed call just
    /// costs one unit of conservatism, never an over-spend.
    pub async fn acquire(&self, class: Class) -> Outcome {
        // Politeness burst cap first, so a spike queues here, not at the API.
        self.burst.until_ready().await;

        let now = Utc::now();
        let wait_ms: i64;
        {
            let mut g = self.inner.lock().await;
            self.roll_over(&mut g, now);

            if let Some(until) = g.throttled_until {
                if now < until {
                    return Outcome::Exhausted {
                        retry_after: (until - now).to_std().unwrap_or(StdDuration::ZERO),
                    };
                }
                g.throttled_until = None;
            }

            let ceiling = match class {
                Class::Background => self.background_budget,
                Class::Interactive => self.total_budget,
            };
            if g.used >= ceiling {
                return Outcome::Exhausted {
                    retry_after: (next_utc_reset(now) - now)
                        .to_std()
                        .unwrap_or(StdDuration::ZERO),
                };
            }

            // Reserve the unit.
            g.used += 1;
            g.unflushed += 1;

            wait_ms = match class {
                Class::Interactive => 0,
                Class::Background => {
                    let remaining = (self.background_budget - g.used).max(1);
                    let secs_to_reset = (next_utc_reset(now) - now).num_seconds().max(1);
                    let spacing = ((secs_to_reset * 1000) / remaining).clamp(0, MAX_SPACING_MS);
                    let base = if g.bg_next_at > now {
                        g.bg_next_at
                    } else {
                        now
                    };
                    let w = (base - now).num_milliseconds().max(0);
                    g.bg_next_at = base + Duration::milliseconds(spacing);
                    w
                }
            };
        }

        if wait_ms > 0 {
            tokio::time::sleep(StdDuration::from_millis(wait_ms as u64)).await;
        }
        Outcome::Granted
    }

    /// Steam answered 429 despite our accounting: a burst/IP throttle, or
    /// external spend on the same key. Pause all calls on this key briefly
    /// (honoring Retry-After when the caller parsed one) — NOT until the
    /// daily reset, because a 429 doesn't mean the daily allowance is gone.
    pub async fn mark_throttled(&self, secs: u64) {
        let now = Utc::now();
        let until = now + Duration::seconds(secs.max(1) as i64);
        let mut g = self.inner.lock().await;
        // Keep the longest pause if we're already throttled.
        if g.throttled_until.map_or(true, |u| u < until) {
            g.throttled_until = Some(until);
        }
        tracing::warn!(
            scope = self.scope,
            pause_secs = secs,
            "Steam returned 429 — pausing API calls on this key"
        );
    }

    /// Persist the unflushed delta to the durable ledger and reconcile the
    /// in-memory total against the shared total (multi-instance safe).
    pub async fn flush(&self) {
        let (date, delta) = {
            let mut g = self.inner.lock().await;
            let d = g.unflushed;
            g.unflushed = 0;
            (g.date, d)
        };
        if delta <= 0 {
            return;
        }
        match sqlx::query_scalar::<_, i64>(
            "INSERT INTO api_quota_usage (quota_date, scope, used_units, updated_at) \
             VALUES ($1, $2, $3, now()) \
             ON CONFLICT (quota_date, scope) DO UPDATE SET \
               used_units = api_quota_usage.used_units + EXCLUDED.used_units, updated_at = now() \
             RETURNING used_units",
        )
        .bind(date)
        .bind(&self.scope)
        .bind(delta)
        .fetch_one(&self.pool)
        .await
        {
            Ok(db_used) => {
                let mut g = self.inner.lock().await;
                if g.date == date {
                    g.used = g.used.max(db_used);
                }
            }
            Err(e) => {
                tracing::error!(scope = self.scope, "Quota ledger flush failed: {e}");
                let mut g = self.inner.lock().await;
                g.unflushed += delta; // retry next interval
            }
        }
    }

    pub async fn snapshot(&self) -> QuotaSnapshot {
        let now = Utc::now();
        let mut g = self.inner.lock().await;
        // Roll the day over here too: the background worker polls snapshot
        // while paused on exhaustion and never calls acquire, so without this
        // it could stay paused after the UTC reset.
        self.roll_over(&mut g, now);
        QuotaSnapshot {
            date: g.date,
            used: g.used,
            total_budget: self.total_budget,
            background_budget: self.background_budget,
            reset_in_secs: (next_utc_reset(now) - now).num_seconds().max(0),
            throttled: g.throttled_until.is_some_and(|u| now < u),
        }
    }

    /// Background task: periodically persist the counter.
    pub async fn run_flusher(self: Arc<Self>) {
        tracing::info!(scope = self.scope, "Quota ledger flusher started");
        loop {
            tokio::time::sleep(FLUSH_INTERVAL).await;
            self.flush().await;
        }
    }
}

/// Lazily-built governors for Steamworks publisher keys. Each publisher key
/// has its own independent daily allowance on Valve's side, so ownership
/// checks must not drain (or be blocked by) the main key's budget. Keys are
/// identified in the ledger by a hash — the raw key never touches the DB.
pub struct PublisherQuotas {
    pool: PgPool,
    quota_per_day: i64,
    safety_frac: f64,
    map: Mutex<HashMap<String, Arc<QuotaGovernor>>>,
}

impl PublisherQuotas {
    pub fn new(pool: PgPool, quota_per_day: i64, safety_frac: f64) -> Self {
        Self {
            pool,
            quota_per_day,
            safety_frac,
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Governor for one publisher key, created (and its flusher spawned) on
    /// first use. Publisher calls are all background work, so no reserve.
    pub async fn for_key(&self, publisher_key: &str) -> Arc<QuotaGovernor> {
        let scope = format!(
            "pub:{}",
            &hex::encode(Sha256::digest(publisher_key.as_bytes()))[..12]
        );
        {
            let map = self.map.lock().await;
            if let Some(g) = map.get(&scope) {
                return Arc::clone(g);
            }
        }
        let governor = QuotaGovernor::new(
            self.pool.clone(),
            scope.clone(),
            self.quota_per_day,
            0.0,
            self.safety_frac,
        )
        .await;
        let mut map = self.map.lock().await;
        // Racing first-users may both build one; keep whichever landed first
        // so all callers share a single pacing gate.
        let entry = map
            .entry(scope)
            .or_insert_with(|| {
                tokio::spawn(Arc::clone(&governor).run_flusher());
                governor
            })
            .clone();
        entry
    }
}

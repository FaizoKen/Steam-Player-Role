# Steam Player Roles

A [RoleLogic](https://rolelogic.faizo.net) plugin that links Discord accounts with Steam profiles via [Steam OpenID](https://steamcommunity.com/dev). Once verified, roles are assigned automatically based on Steam data — games owned, playtime, achievements, Steam level, account age, VAC/game bans, group membership, country, and more.

> **Requires [Auth Gateway](https://github.com/FaizoKen/Auth-Gateway)** — Discord login is handled by the centralized Auth Gateway. This plugin reads the shared `rl_session` cookie set by the gateway.

## How it works

1. **Registers** guild/role pairs via the RoleLogic plugin API
2. **Authenticates** users through the centralized Auth Gateway (Discord OAuth)
3. **Verifies** Steam ownership via Steam OpenID (the user signs in to Steam, Steam redirects back with a signed `steam_id`)
4. **Fetches** player data from the [Steam Web API](https://steamcommunity.com/dev) — summaries, bans, owned games, achievements, level, groups
5. **Syncs** verified player data to RoleLogic for automatic role assignment based on configurable conditions

## Setup

```bash
cp .env.example .env
# Edit .env with your values
```

### Environment Variables

| Variable               | Required | Default                                | Description                                                                     |
| ---------------------- | -------- | -------------------------------------- | ------------------------------------------------------------------------------- |
| `DATABASE_URL`         | Yes      | --                                     | PostgreSQL connection string                                                    |
| `SESSION_SECRET`       | Yes      | --                                     | HMAC key for `rl_session` cookie (must match Auth Gateway)                      |
| `BASE_URL`             | Yes      | --                                     | Full URL with path prefix, e.g. `https://your-domain.com/steam-player-role`     |
| `LISTEN_ADDR`          | No       | `0.0.0.0:8088`                         | Server bind address                                                             |
| `AUTH_GATEWAY_URL`     | Yes      | --                                     | Internal URL for the Auth Gateway, e.g. `http://auth-gateway:8080`              |
| `INTERNAL_API_KEY`     | Yes      | --                                     | Shared secret for calling `/auth/internal/*` on the Auth Gateway                |
| `RL_DASHBOARD_ORIGIN`  | No       | `*`                                    | Origin allowed to embed the role-config page in an iframe (the RoleLogic dashboard) |
| `STEAM_API_KEY`        | Yes      | --                                     | Steam Web API key — [get one here](https://steamcommunity.com/dev/apikey)       |
| `STEAM_API_DAILY_QUOTA` | No      | `100000`                               | Daily Steam Web API call allowance for the key (Valve's ToU default)            |
| `QUOTA_INTERACTIVE_RESERVE` | No  | `0.20`                                 | Fraction of the daily quota reserved for link-time calls users actively wait on |
| `QUOTA_SAFETY_FRACTION` | No      | `0.95`                                 | Fraction of the nominal quota the governor will actually spend                  |
| `REFRESH_WORKERS`      | No       | `2`                                    | Background refresh workers (partitioned by steam_id hash, never double-process) |
| `MAX_STABLE_REFRESH_SECS` | No    | `604800`                               | Ceiling for long-stable users' stretched refresh interval                       |
| `STEAM_API_RATE_LIMIT` | No       | --                                     | **Deprecated** — honored as `× 24` daily quota when `STEAM_API_DAILY_QUOTA` is unset |
| `RUST_LOG`             | No       | `steam_player_role=info,tower_http=info` | Log level                                                                     |

## Run

### Docker (recommended)

```bash
docker compose up -d
```

### From source

```bash
cargo run              # development
cargo build --release  # production
```

## Endpoints

All routes are nested under `/steam-player-role`:

| Method   | Path                       | Description                                           |
| -------- | -------------------------- | ----------------------------------------------------- |
| `GET`    | `/health`                  | Health check                                          |
| `POST`   | `/register`                | Register a guild/role pair                            |
| `GET`    | `/config`                  | Iframe-mode config descriptor (`ui_mode: "iframe"`)   |
| `POST`   | `/config`                  | Contract stub (edits go through `/admin/*`)           |
| `DELETE` | `/config`                  | Delete a registration                                 |
| `GET`    | `/admin/{guild_id}/role/{role_id}`         | Role-config page (embedded by the RoleLogic dashboard, or direct nav for managers) |
| `GET`    | `/admin/{guild_id}/role/{role_id}/data`    | Config + verify/player URLs for the page (JSON)       |
| `POST`   | `/admin/{guild_id}/role/{role_id}/save`    | Save the condition (optimistic-locked)                |
| `GET/POST` | `/admin/{guild_id}/role/{role_id}/preview` | Count matching members for the saved / proposed rule |
| `POST`   | `/admin/{guild_id}/view-permission`        | Set who can view the public player list               |
| `GET`    | `/verify`                  | Verification page                                     |
| `GET`    | `/verify/login`            | Redirects to Auth Gateway for Discord login           |
| `GET`    | `/verify/status`           | Check linked account status                           |
| `GET`    | `/verify/steam`            | Redirect to Steam OpenID login                        |
| `GET`    | `/verify/callback`         | Steam OpenID return URL — validates and links account |
| `POST`   | `/verify/unlink`           | Unlink Discord account from Steam profile             |
| `POST`   | `/verify/recheck`          | Queue an immediate data refresh (once per 5 minutes)  |
| `POST`   | `/verify/logout`           | Clear the `rl_session` cookie                         |
| `GET`    | `/players/{guild_id}`      | Player list page                                      |
| `GET`    | `/players/{guild_id}/data` | Paginated player data (JSON)                          |

## Conditions

A role link evaluates one condition against the verified member's Steam data.

**Game-specific** (requires an App ID): owns the game, total playtime, recent (2-week) playtime, achievement count, achievement completion %, has a specific achievement.

**Account-level**: Steam level, account age in days, total games owned, VAC banned, game banned, member of a Steam group, country code.

Numeric fields support `=`, `>`, `>=`, `<`, `<=`, and `between` (inclusive range). Boolean and string-exact fields use equality.

For the "owns the game" condition, the game's developer/publisher can optionally add a Steamworks Web API publisher key in the role-link config — ownership is then verified through Steam's partner API (`CheckAppOwnership`) instead of the member's public library. See [Privacy & ownership](#privacy--ownership).

## Privacy & ownership

- **Game details must be public** for game-based conditions (ownership, playtime, achievements) — unless the role link has a publisher key (below). If a member's Steam **Game details** privacy setting is private, the plugin sees an empty library and game-based roles are removed. The verify page warns affected members and offers a **Re-check** button (rate-limited to once per 5 minutes) that queues an immediate refresh after they fix their settings.
- **Publisher key (optional)**: with a [Steamworks Web API publisher key](https://partner.steamgames.com/doc/webapi_overview/auth) configured, "owns the game" is checked against Steam's partner API, which answers even when game details are private. The key is stored server-side and never echoed back to the dashboard.
- **Family Sharing is excluded**: borrowed games never grant roles. The publisher check enforces this explicitly — the license must belong to the linked Steam account — and shared titles don't appear in the public library either.
- **Temporary licenses don't count**: with a publisher key, free-weekend and trial licenses (`permanent = false`) are treated as not owned.
- **Refunds & revocations**: Steam data refreshes on an adaptive schedule — every 30 minutes to 24 hours per user, depending on player count and activity; accounts whose data hasn't changed across many checks stretch further, up to `MAX_STABLE_REFRESH_SECS` (7 days by default). A refunded or revoked game drops the role within that window, and the change itself resets the account back to the fast cadence.

## Scaling & rate limits

Valve's Web API Terms of Use allow **100,000 calls per day per key**, and the per-user endpoints (GetOwnedGames, GetSteamLevel, GetUserGroupList, achievements) have no batch form — so the daily key budget is the hard ceiling on how many users the plugin can keep fresh. The plugin is built to spend that budget well:

- **Quota governor** — every Steam call passes through a central daily budget (`STEAM_API_DAILY_QUOTA × QUOTA_SAFETY_FRACTION`), persisted to the database so restarts can't over-spend. Background refreshes are paced smoothly across the UTC day (no bursts, no thundering herd) and stop by themselves when the budget is spent — the plugin then serves from cache until the next day. An **interactive reserve** (`QUOTA_INTERACTIVE_RESERVE`) is kept aside so link-time lookups, fresh links, and verify-page Re-checks keep working even after the background budget is gone. A Steam `429` pauses calls briefly (honoring `Retry-After`); current spend is visible under `quota` in `GET /health`.
- **Batching** — profiles and ban states are fetched 100 users per call.
- **Condition-driven fetching** — owned games / level / groups are only fetched if some role link's condition actually references them; achievements and publisher-ownership checks are further scoped to the guilds each user is in. Typical deployments cost ~1 call per user per refresh instead of 5+.
- **Adaptive cadence** — users whose data keeps coming back unchanged earn exponentially longer refresh intervals (up to 16×, capped by `MAX_STABLE_REFRESH_SECS`), concentrating the budget on accounts that are actually changing.
- **Publisher keys have their own budget** — `CheckAppOwnership` calls are accounted per publisher key (each has its own daily allowance on Valve's side) and never drain the main key's budget.
- **Horizontal workers** — refresh workers claim users with `FOR UPDATE SKIP LOCKED`, leased and partitioned by steam_id hash, so `REFRESH_WORKERS` (or multiple instances) never double-process. For multiple *instances*, divide `STEAM_API_DAILY_QUOTA` across them.

Rough capacity on one key at defaults: ~90k usable calls/day ÷ ~1 call per user ≈ **90k users at daily freshness**, stretching to several hundred thousand once most users settle onto the stability-extended cadence. Beyond that, raise `MAX_STABLE_REFRESH_SECS` (staler roles for stable users) or use additional keys/instances.

## Usage

1. Ensure the [Auth Gateway](https://github.com/FaizoKen/Auth-Gateway) is running on `your-domain.com/auth/*`
2. In the RoleLogic dashboard, create a Role Link and set the **Custom Plugin URL** to `https://your-domain.com/steam-player-role`
3. RoleLogic will automatically register the guild/role pair
4. Users visit the verification page, sign in with Discord (via Auth Gateway), then link their Steam account via Steam OpenID
5. Roles are assigned automatically based on the condition you configure; Steam data refreshes on a background schedule so roles stay current

## API Reference

- [RoleLogic Role Link API](https://docs-rolelogic.faizo.net/reference/role-link-api)
- [Steam Web API](https://steamcommunity.com/dev)
- [Steam OpenID](https://steamcommunity.com/dev)

## License

[MIT](LICENSE)

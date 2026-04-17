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
| `STEAM_API_KEY`        | Yes      | --                                     | Steam Web API key — [get one here](https://steamcommunity.com/dev/apikey)       |
| `STEAM_API_RATE_LIMIT` | No       | `3600`                                 | Max Steam Web API requests per hour (smoothed to a per-second token bucket)     |
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
| `GET`    | `/config`                  | Get plugin configuration schema                       |
| `POST`   | `/config`                  | Update role link conditions                           |
| `DELETE` | `/config`                  | Delete a registration                                 |
| `GET`    | `/verify`                  | Verification page                                     |
| `GET`    | `/verify/login`            | Redirects to Auth Gateway for Discord login           |
| `GET`    | `/verify/status`           | Check linked account status                           |
| `GET`    | `/verify/steam`            | Redirect to Steam OpenID login                        |
| `GET`    | `/verify/callback`         | Steam OpenID return URL — validates and links account |
| `POST`   | `/verify/unlink`           | Unlink Discord account from Steam profile             |
| `POST`   | `/verify/logout`           | Clear the `rl_session` cookie                         |
| `GET`    | `/players/{guild_id}`      | Player list page                                      |
| `GET`    | `/players/{guild_id}/data` | Paginated player data (JSON)                          |

## Conditions

A role link evaluates one condition against the verified member's Steam data.

**Game-specific** (requires an App ID): owns the game, total playtime, recent (2-week) playtime, achievement count, achievement completion %, has a specific achievement.

**Account-level**: Steam level, account age in days, total games owned, VAC banned, game banned, member of a Steam group, country code.

Numeric fields support `=`, `>`, `>=`, `<`, `<=`, and `between` (inclusive range). Boolean and string-exact fields use equality.

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

# Qivomedia

A digital signage platform written in Rust. One small binary includes the CMS, the API, real-time push to screens, and an offline-first web player.

## Run it

```bash
# Native (Rust 1.80+)
cargo run --release
# → http://localhost:8080          dashboard (the first visit creates your organization + owner account)
# → http://localhost:8080/player   open this on each screen and pair it with the 6-character code

# Docker
docker compose up -d --build
```

| Env var | Default | Meaning |
|---|---|---|
| `QIVO_BIND` | `0.0.0.0:8080` | Listen address |
| `QIVO_DATA_DIR` | `./data` | SQLite DB + media store |
| `QIVO_WEB_DIR` | `./web` | Dashboard / player static files |
| `QIVO_ALLOW_SIGNUP` | `0` | `1` lets anyone create a new organization (SaaS mode) |
| `DATABASE_URL` | `sqlite://$QIVO_DATA_DIR/qivo.db` | Database connection |
| `RUST_LOG` | `qivo_server=info` | Log level |

**On a public server**, use the production setup in `deploy/` (Caddy with automatic HTTPS, health checks, nightly backups): `sudo ./deploy/install.sh`. Full steps in **[DEPLOY.md](DEPLOY.md)**.

Server commands: `qivo-server healthcheck` · `qivo-server backup [file]` · `qivo-server version`.

## What's in the MVP

- **Multi-tenant workspaces** with owner, admin, editor and viewer roles, plus an audit log.
- **Media library**: upload with progress. Files are stored by SHA-256, so duplicates are kept once and cache forever. Video duration is detected automatically.
- **Playlists** of media and live widgets: text, ticker, clock, weather, countdown, web page and sandboxed HTML. They support drag-to-reorder, shuffle, and per-item day-parting.
- **Layout designer** with drag-and-resize zones, templates (full screen, L-bar, split, quad…), any resolution including portrait and video walls, per-zone transitions, and live preview.
- **Scheduling** by date range, weekday and daily time window (overnight windows work). Higher priority wins, and equal priorities rotate. A weekly timeline shows exactly what each screen will play.
- **Screens**: 6-character pairing codes, groups, a default layout, live online/offline status and what's playing now. You can send identify, reload or clear-cache commands, or disable a screen.
- **Instant push**: any content change reaches every affected screen over WebSocket within about a second.
- **Offline-first player**: the service worker caches the app and all media. The player evaluates the *full* schedule locally, so it keeps switching layouts correctly with no network.
- **Triggers / webhooks**: `POST /api/hooks/<key>` takes over targeted screens with a layout for N seconds. JSON `data` fills `{{placeholders}}` in widgets, for example "Order 42 ready".
- **Emergency alerts**: one click switches all screens, a group or a single screen to a fire, evacuation, lockdown, severe-weather or custom alert. Templates are in French, Arabic and English. The alert sits above every schedule, plays a siren, and stays until cleared, even through reboots and network loss. Clearing it shows "All clear". Test/drill mode, a webhook, and **CAP 1.2** feeds from campus emergency systems are supported.
- **Seamless playback**: each item is revealed only once it's decoded or actually playing, so there's no black flash between videos.
- **AI layout agent** (optional, Anthropic API key): describe a screen in plain words, in French, Arabic or English, and get a real, editable layout with zones, designed slides, a clock, weather and a ticker. Ask for changes in the designer. Results are drafts until a person assigns them. Usage is metered per organization.
- **Platform admin** for the SaaS owner: create customer organizations, set plans, screen limits and AI allowances, suspend or reactivate customers, and see usage across the platform.
- **Accounts and security**: invitations by email, password reset, a 15-minute lockout after 5 failed logins, rate limits, single-use expiring links, sign-out everywhere on password change, and security headers.
- **Proof of play**: the device records every play, even offline, and syncs it later. Reports break plays down by day, content and screen, and export to CSV.

## Tests

`cargo test` runs the unit tests and an in-process API suite. Each test builds the whole app on a fresh database and covers sign-up, lockout, password reset, invitations, sessions, organization isolation, roles, emergency alerts, suspension and website leads. GitHub Actions runs it on every push, then builds the Docker image, checks it starts healthy, and validates both Caddy configs (`.github/workflows/ci.yml`).

## Website

`site/` holds the public website for qivomedia.com in English, French and Arabic, with full right-to-left layout. The hero shows the real player running live, with a button that triggers a demo fire alert, and there's a demo-request form that feeds leads into the Platform page. Build it with `python3 site/build.py`. See DEPLOY.md, section 10.

## Layout

```
src/            Rust server (axum + sqlx)
  main.rs       wiring, static serving
  auth.rs       sessions, roles, users, audit
  media.rs      uploads, content-addressed storage
  content.rs    playlists + layouts (validation)
  displays.rs   pairing, groups, commands
  schedules.rs  day-parting rules
  triggers.rs   webhooks → interrupts
  player.rs     player protocol: register, manifest, proof of play, push socket
  hub.rs        real-time event bus + presence
  reports.rs    proof of play + overview stats
migrations/     SQL schema
web/app/        CMS dashboard (vanilla ES modules, no build step)
web/player/     web player + service worker
web/shared/     schedule engine shared by player and dashboard
site/           public website (src/ → build.py → dist/)
deploy/         production compose, Caddyfiles, installer, backups
```

See `ARCHITECTURE.md` for the design and roadmap.

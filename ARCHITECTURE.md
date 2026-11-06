# Qivomedia: architecture and roadmap

## 1. Goal

Build a digital signage platform that does what Xibo does and goes further in three places: it is **real-time** by default, it plays **offline** without breaking, and it is **simple to run**. It must work as a self-hosted install and as a multi-tenant SaaS on qivomedia.com.

## 2. Where we stand against Xibo

| Area | Xibo (typical) | Qivomedia MVP | Qivomedia target |
|---|---|---|---|
| Server stack | PHP CMS + MySQL + separate XMR push service, often Docker multi-container | **Single Rust binary** (API, push, dashboard, player, files) | Same binary; Postgres + S3 option for SaaS scale |
| Content updates | Players poll on a collection interval; XMR can nudge | **Push over WebSocket**, screens update in ~1 s | + delta manifests, staged rollouts |
| Offline | Player caches required files | Service worker caches app + media; **full schedule evaluated on device** | + signed manifests, storage quotas, pre-download windows |
| Scheduling | Events, day-parting, priorities, campaigns | Date range, weekdays, daily window (overnight), priorities, rotation, per-item day-parting, **visual week timeline** | Campaigns, share-of-voice, timezone per screen, exceptions/holidays |
| Interrupts | Interrupt layouts, action triggers | **Webhook triggers with JSON data → `{{placeholders}}`** | Rules engine (if sensor/API value → layout), IFTTT/Zapier/MQTT |
| Multi-tenancy | Single-tenant CMS with folders/permissions | **Organizations built in**, roles, audit log | Reseller → client hierarchy, white-label, billing |
| Players | Windows, Android, webOS, Tizen, Linux, ChromeOS (some licensed) | Web player (any browser, TV, kiosk, PWA) | Android/Android TV (Rust core + WebView), Tizen/webOS packages, Raspberry Pi image |
| Data | DataSets, tickers, RSS | Text/ticker/clock/weather/countdown/web/HTML widgets | DataSets & live feeds (RSS, Google Sheets, REST/JSON), menu boards |
| Reporting | Proof of play, display stats | Device-side PoP synced offline, per day/content/screen, CSV | Audience analytics, uptime SLA, screenshots, alerts |

## 3. System design

```
           ┌──────────────── qivo-server (Rust, tokio) ────────────────┐
Browser ──►│  /           dashboard SPA (static)                        │
(CMS)  ◄──►│  /api/*      REST  (axum)  ──► sqlx ──► SQLite | Postgres  │
       ◄───│  /ws/admin   live status / events                         │
           │                       ▲                                    │
Screens ──►│  /api/player/*   register · manifest · proof-of-play      │
       ◄───│  /ws/player      push: refresh · interrupt · command      │
       ──► │  /files/<sha256> immutable media (Range, cache 1y)         │
           │  event bus: tokio broadcast  →  per-socket filtering       │
           └────────────────────────────────────────────────────────────┘
POS / sensors / Zapier ──► POST /api/hooks/<secret>  → interrupt targeted screens
```

**Principles**
1. **Fat manifest, thin server.** A screen receives *all* of its schedules, layouts, playlists and media in one bundle with a content hash (`version`). The shared engine (`web/shared/schedule.js`) decides what plays on the device, so playback is correct offline and the server stays stateless per screen.
2. **Content-addressed media.** Files are stored as `sha256.ext`. Uploads dedupe, URLs never change, and players and CDNs can cache them forever.
3. **Push first, poll as backup.** Every write calls `content_changed(org)` and the screens re-fetch the manifest. If the socket is down, the player polls every 60 s.
4. **Org scoping on every query.** Every table carries `org_id`, and every handler filters by the caller's org, including when it validates IDs referenced inside JSON (playlist → media, layout → playlist).

**Player protocol**
1. `POST /api/player/register` returns `device_key` (stored on the device) and a 6-char `pairing_code`.
2. An admin claims the code in the dashboard, and the server pushes `claimed` to the waiting socket.
3. `GET /api/player/manifest` (header `X-Device-Key`) returns the bundle plus its `version`.
4. `WS /ws/player?key=…` handles presence and carries `refresh`, `interrupt{layout_id,duration_s,data}`, `command{reload|identify|clear_cache}` and `unpaired`. The player sends back `status{layout_id,version,info}`.
5. `POST /api/player/pop` sends proof-of-play records in batches. They are queued on the device while it is offline.

**Data model** (`migrations/0001_init.sql`): organizations, users, sessions, media, playlists (items JSON), layouts (regions JSON), display_groups, displays, schedules, triggers, proof_of_play, audit_log.

**Security:** Argon2id passwords. Session tokens are random, 48 characters, and stored only as SHA-256. Device keys are stored the same way. Role checks run per handler (viewer < editor < admin < owner). HTML widgets render in sandboxed iframes. Webhook keys are 32-character random secrets.

## 4. Roadmap

**Phase 1 — MVP (built)**
CMS, web player, pairing, layouts/zones, playlists/widgets, scheduling, triggers, push, offline, proof of play, multi-tenant roles.

**Phase 1.5 — built (v0.2)**
- Production deployment: Docker + Caddy (automatic HTTPS), health checks, non-root container, nightly backups with restore, one-command installer (`deploy/`, `DEPLOY.md`)
- **Platform admin** (SaaS owner): customer organizations with plan, screen limit, AI on/off + monthly allowance, suspend/reactivate (users blocked, screens disabled, nothing deleted), delete; platform-wide stats and AI activity log; `qivo-server make-admin <email>`
- **AI layout agent**: brief → complete editable layout (zones, playlists, HTML slides, native clock/weather/ticker/countdown widgets, library media) via Claude with a forced `build_signage` tool; "Ask AI to change" revisions in the designer; drafts only (never auto-scheduled); output validated with the same rules as the editor (invented media ids dropped); per-org monthly metering and token accounting. Config: `ANTHROPIC_API_KEY`, `QIVO_AI_MODEL` (default `claude-sonnet-5-5`).

**Phase 1.6 — built (v0.3)**
- **Emergency alerts**: templates in FR/AR/EN (fire, evacuation, lockdown, severe weather) plus custom alerts in red/amber/blue; targets all/group/screen; above all schedules and triggers; included in the manifest, so they survive reboots and offline periods; siren; "All clear" on clear; test/drill mode; JSON webhook + **CAP 1.2** (Alert/Update start, Cancel clears matching `references`); audit-logged; active-alert banner across the dashboard. Editors can raise alerts.
- **Seamless playback**: double-buffered swap reveals the next item only when it is ready (decoded image / playing video, 2.5 s cap), so there is no black frame between items.

**Phase 2 — Production hardening (next 4–6 weeks)**
- Postgres backend + S3/R2 media storage + signed URLs; Redis or Postgres LISTEN/NOTIFY so several server instances share the event bus
- Per-screen timezone; holiday/exception dates; campaigns (ordered layout sets) and share-of-voice
- Screenshots from players, uptime history, offline alerts (email/Slack/WhatsApp)
- Rate limiting, login lockout, 2FA, password reset, invitations by email
- Server-side thumbnails + video transcoding (ffmpeg) and image resizing per screen resolution
- Integration tests (API) + Playwright E2E in CI

**Phase 3 — Players everywhere**
- **Windows player (Tauri / Rust + WebView2)**, same web player in a ~10 MB native shell:
  - Kiosk mode: fullscreen, always on top, hidden cursor/taskbar, auto-start at boot without login
  - Watchdog auto-restart on hang/crash + scheduled daily restart
  - Remote commands: reboot PC, restart player, real screenshot, volume, display on/off schedule (HDMI-CEC via USB adapter, or monitor power saving)
  - Device health in dashboard: CPU, RAM, disk, temperature, uptime + alerts
  - Silent self-update; Windows Update restarts held to off-hours; MSI/EXE installer
  - Image quality: GPU video decode (H.264/HEVC/VP9/AV1; check for and warn if the HEVC extension is missing), pixel-exact rendering under Windows 150/200% scaling
- Dashboard warning when a layout or media file is lower resolution than the target screen (e.g. 1080p photo on a 4K TV)
- Installer guide for TV settings: overscan off (“Just Scan”/“Screen Fit”), HDMI input in PC/Game mode, motion smoothing & extra sharpening off
- Android / Android TV app (Kotlin shell + WebView of the web player, kiosk mode, boot-start, remote reboot, OTA)
- Samsung Tizen (SSSP) and LG webOS Signage packages; Raspberry Pi OS image; Windows/Linux Tauri app (Rust)
- **Synchronized video walls**: clock-sync via the socket and frame-accurate playback groups

**Phase 4 — Beyond Xibo**
- DataSets & feeds (Google Sheets, RSS, REST/JSON, Excel) bound to templates; menu-board designer
- Rules engine: "if weather > 30 °C show cold drinks", "if queue > 10 show wait-time", MQTT/IoT input
- AI tools (beyond the layout agent): AI images via a separate image model, auto-resize designs for portrait/landscape, auto-captions, translation (Arabic/French/English), smart playlist ordering using proof-of-play data
- Audience analytics (anonymous camera counting), A/B testing of content
- SaaS: reseller → client hierarchy, white-label domains, Stripe billing per screen, usage metering

## 5. Market: universities

Campuses run hundreds of screens across faculties, libraries, cafeterias and residences, managed by many departments, and their IT teams often prefer self-hosting (Docker fits). Typical buying path: tender or pilot of 10–20 screens in one faculty, then campus-wide rollout.

**Priority 1 — needed to win the deal**
1. **University login (SSO):** SAML 2.0 / OpenID Connect with Microsoft 365 (Azure AD / Entra ID) and Google Workspace; auto-create users on first login; map IdP groups to roles. Often a hard requirement from university IT.
2. **Emergency alerts (built in v0.3):** one-click campus-wide or building-level takeover within seconds (fire, lockdown, weather), highest priority above all schedules, stays until cleared. Accept the standard alert format **CAP (Common Alerting Protocol)** so campus emergency systems can trigger it directly. Builds on existing webhook triggers. Audit-logged; tested periodically.
3. **Approval workflow:** departments, clubs and students submit content; communications approves before it goes live. Draft → pending → approved/rejected, with comments and email notification. Same mechanism as the AI drafts-first feature.

**Priority 2 — strong differentiators**
4. **Departments as sub-accounts:** university → faculties/departments, each managing its own screens and content; central communications can view and override everything and push content to all departments.
5. **Calendars and timetables:** Outlook/Exchange and Google Calendar feeds; room-door sign templates (current/next class, room free/busy); event screens; exam schedules from CSV/Excel or the student information system.
6. **Multilingual & accessibility:** Arabic + French + English on one screen with correct right-to-left text and Arabic fonts; contrast checks and minimum text-size guidance; captions/subtitles on videos.

**Priority 3 — nice to have**
7. Wayfinding and interactive campus maps (touch kiosks at entrances).
8. Social media walls with moderation; student news feeds.
9. Usage reports per department (screens, airtime, approvals) for internal chargeback.

**Commercial:** per-screen pricing with an education discount; on-premise licence option for data-residency requirements; pilot package (10–20 screens, fixed price, 3 months).

## 6. Running at scale (SaaS target)

- Stateless `qivo-server` instances behind a load balancer, with sticky WebSockets not required.
- Event bus on Postgres LISTEN/NOTIFY to start, then NATS if the fan-out outgrows it.
- Media on S3-compatible storage behind a CDN. Manifests carry CDN URLs.
- Rough capacity: one 2-vCPU instance should hold roughly 10k idle player sockets, because tokio keeps a socket at a few KB. This has not been measured yet; a load test before launch should confirm it.

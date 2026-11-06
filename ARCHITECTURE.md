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

**Security:** see `SECURITY.md`. In short:
- Argon2id passwords under a NIST 800-63B policy with a HIBP breach check.
- TOTP 2FA with recovery codes and replay protection, which an organization can require for everyone.
- Sessions in an HttpOnly, SameSite=Strict `__Host-` cookie (stored server-side as SHA-256), with an idle timeout, a list of sessions you can revoke, and CSRF checks through Origin and Fetch Metadata.
- Content Security Policy per area. Uploads are identified by their content, and uploaded HTML and SVG are served as sandboxed documents.
- Read-only, capability-free containers. CI runs cargo-audit and Trivy and produces an SBOM. Dependabot keeps dependencies current.

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

**Phase 1.7 — built (v0.4)**
- **Public website** (`site/`): EN `/`, FR `/fr/`, AR `/ar/` (RTL) generated from one template plus `i18n.json`. The hero embeds the real player in demo mode (`/player/?demo=1`, which loads the layout chosen with "Show on website", or a built-in demo) and drives a demo alert via postMessage. The demo form sends leads to `/api/public/demo-requests` (honeypot, 5/hour/IP), and they appear in Platform.
- **SaaS routing**: `deploy/Caddyfile.saas` serves the site on the apex domain, proxies `/api/public`, `/player`, `/shared` and `/files` on the same origin, and serves the app on `app.<domain>`. The default `Caddyfile` stays single-domain for self-hosted customers.

**Phase 1.8 — built (v0.5): ready for paying customers**
- Email over SMTP (lettre, STARTTLS/TLS). Without it, messages are logged. Used for password reset (1-hour single-use link), team invitations (7-day link where the person chooses their own password), and demo-request alerts to platform admins.
- Login lockout (5 failures → 15 minutes per account, ×6 per IP), rate limits on sign-up, reset, token checks, screen registration and webhooks; sign-out of other sessions on password change, and of all sessions on reset; `nosniff`, `Referrer-Policy` and `X-Frame-Options: SAMEORIGIN` headers.
- Test suite: 16 tests, including 12 in-process API tests on fresh databases. GitHub Actions CI runs the tests, JS syntax checks, the site build and compose validation, then builds the Docker image, checks it starts healthy and backs up, and validates both Caddyfiles with the real Caddy.

**Phase 1.9 — built (v0.6): security hardening**
- **Two-factor authentication:** TOTP with QR enrollment, 10 hashed recovery codes and replay protection. Organizations can require it, enforced on the server. A reset link can't bypass 2FA. A lost device can be recovered by an organization admin, by the platform operator (with an identity-check reason logged in both audit logs), or by `qivo-server reset-mfa` on the server. Every reset emails the person.
- **Cookie sessions:** HttpOnly, SameSite=Strict, `__Host-` over HTTPS. No tokens in localStorage, URLs or WebSocket query strings. Sessions expire after 12 hours idle (configurable) and 30 days at most, and each person can list and revoke them.
- **CSRF guard:** Origin and Sec-Fetch-Site checks on cookie-authenticated writes. The permissive CORS setting is gone.
- **Content Security Policy** per area (strict dashboard with no inline script, player, API, files). Uploaded HTML, SVG and XML are served with `CSP: sandbox`, and uploads are identified by their content with `infer`. Also COOP, CORP, Permissions-Policy, HSTS, `no-store` on the API, and `security.txt`.
- **Password policy** per NIST 800-63B: 12 to 128 characters, a common-password list, no passwords built from your own name, email or organization, and a HIBP k-anonymity check that fails open.
- **Security audit events** with IP address and browser. The client IP is taken from the rightmost X-Forwarded-For entry only.
- **Infrastructure:** read-only root filesystem, `cap_drop: ALL`, `no-new-privileges` and a process limit. CI runs cargo-audit (exceptions in `.cargo/audit.toml`) and Trivy, produces an SBOM, and runs weekly. Dependabot covers cargo, Actions and Docker. The workflow token is read-only.

**Phase 1.10 — built (v0.7): stress-tested for 15,000 screens**
- Per-organization real-time channels for players and dashboards; each message is serialized once and shared. Emergency alerts reach 10,000 screens in 0.37 s instead of 7.6 s.
- Small WebSocket buffers (10 KB per screen instead of 191 KB); batched "last seen" writes; SQLite `synchronous=NORMAL`.
- Manifest bundles cached per organization and computed once (single-flight), emptied on any content change. Refreshes carry `spread_ms` so large fleets don't all download at once.
- Load generator in `tools/loadtest`, with results in `STRESS-TEST.md`.

**Phase 1.11 — built (v0.8): desktop player for Windows and Linux** (`players/desktop`)
- Tauri 2 app (Rust; WebView2 on Windows, WebKitGTK on Linux) that loads the server's web player, so a server update reaches every screen with no reinstall. Setup screen in EN/FR; tries HTTPS then HTTP; machine-wide defaults (`%ProgramData%`) and `--server` for mass deployment.
- Kiosk: full screen, always on top, hidden cursor, autoplay with sound, keep-awake. Watchdog supervisor restarts the player on crash, freeze (no heartbeat for 2 min) or exit, plus a daily restart, with back-off.
- Dashboard: live screenshot, device health (CPU, memory, disk, temperature, uptime, resolution, refresh rate, scaling) and remote Restart player / Restart computer / Screen off-on (admin-only, audit-logged).
- Native functions are callable only from the configured server's origin (Tauri capabilities). Setup functions stay local.
- `kiosk/setup-kiosk.ps1` (Windows): signage account, auto sign-in, Shell Launcher (no desktop), no sleep or lock screen, update restarts outside 06:00–23:00.
- `kiosk/setup-kiosk.sh` (Debian 13, e.g. HP t430 thin clients after removing ThinPro): X11 + Openbox kiosk session with auto sign-in, Intel VA-API decoding with GStreamer GL zero-copy, PipeWire audio, Arabic fonts, polkit rule for remote restart, small logs for eMMC, unattended security updates with a 03:30 restart, optional portrait rotation.
- CI builds the NSIS and MSI installers on Windows and the .deb and AppImage on Linux, then installs the .deb with the kiosk script on a real Debian 13 container and starts the player there.

**Phase 2 — Production hardening (next 4–6 weeks)**
- Postgres backend + S3/R2 media storage + signed URLs; Redis or Postgres LISTEN/NOTIFY so several server instances share the event bus
- Per-screen timezone; holiday/exception dates; campaigns (ordered layout sets) and share-of-voice
- Uptime history, offline alerts (email/Slack/WhatsApp). Screenshots from Windows players were built in v0.8.
- Passkeys (WebAuthn) for passwordless, phishing-resistant sign-in; university SSO (see section 5); encrypt TOTP secrets at rest
- Server-side thumbnails + video transcoding (ffmpeg) and image resizing per screen resolution
- Browser end-to-end tests (Playwright) in CI

**Phase 3 — Players everywhere**
- **Desktop player (Windows / Linux)**, next steps after v0.8:
  - Silent self-update of the app itself (signed updates served by the Qivomedia server)
  - Display on/off **schedule** (per group, e.g. 07:00–22:00) and HDMI-CEC TV power through a USB adapter; volume control
  - Alerts when a screen goes offline, runs hot or is low on disk (email/WhatsApp)
  - Hardware video-format report (which codecs the GPU decodes) so the dashboard warns before a file that would stutter goes live
  - Tested hardware list (Intel N150 fanless reference box) and a signed installer (code-signing certificate)
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
- Measured capacity (`STRESS-TEST.md`): one 2-vCPU / 8 GB instance held 15,000 screens. It used 169 MB of memory (about 10 KB per screen) and 1–2 % CPU at idle. An emergency alert reached all 15,000 screens in under 0.5 s. Manifests are served at about 2,400 per second. The next limits are SQLite's single writer and running a single process, both covered by Phase 2.

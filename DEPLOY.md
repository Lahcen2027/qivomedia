# Installing Qivomedia on a server

This gets Qivomedia running at `https://your-domain` with automatic HTTPS, nightly backups and automatic restarts. It takes about 15 minutes.

## 1. What you need

- **A Linux server** running Ubuntu 22.04/24.04 or Debian 12. A VPS with **2 vCPU, 4 GB RAM and 40 GB+ disk** is plenty to start. Disk space depends mostly on how much video you upload.
- **A domain**, e.g. `qivomedia.com` or `signage.youruniversity.ma`.
- **SSH access** as root, or as a user with `sudo`.

## 2. Point the domain at the server

At your DNS provider, create two records pointing to the server's public IP:

| Type | Name | Value |
|---|---|---|
| A | `@` (or `signage`) | `your.server.ip` |
| A | `www` | `your.server.ip` |

Wait until `ping your-domain` answers with that IP. HTTPS certificates can't be issued before then.

## 3. Copy Qivomedia to the server

From your computer:

```bash
scp qivomedia.zip root@your.server.ip:/opt/
ssh root@your.server.ip
apt-get update && apt-get install -y unzip
cd /opt && unzip qivomedia.zip && cd qivomedia
```

(Once the code is in a Git repository, this becomes `git clone … /opt/qivomedia`.)

## 4. Install

```bash
sudo ./deploy/install.sh
```

The installer:
1. installs Docker if it's missing,
2. asks for your domain and an email address for the certificates (saved in `deploy/.env`),
3. checks that the domain really points to this server,
4. opens ports 22, 80 and 443 in the firewall (if `ufw` is installed),
5. builds and starts Qivomedia behind Caddy, which gets and renews HTTPS certificates automatically,
6. schedules a nightly backup at 03:15.

The first build compiles the Rust server and takes about 5–10 minutes. Later starts take seconds.

## 5. First login

Open `https://your-domain`. The **first visitor creates the organization and the owner account**, so do this straight away. After that, sign-up is closed unless you set `QIVO_ALLOW_SIGNUP=1` in `deploy/.env`. That setting is for SaaS mode, where every customer creates their own organization.

## 6. Pair your first TV

1. On the TV, PC, kiosk or media box, open a browser at `https://your-domain/player`.
2. The screen shows a 6-character code.
3. In the dashboard, go to **Displays → Add screen**, type the code, and choose a default layout.
4. The screen starts playing within a second or two. Double-click (or press OK) for full screen.

**For a sharp picture on a TV:**
- Set the computer's output to the TV's native resolution (usually 1920×1080 or 3840×2160) at 60 Hz.
- On the TV, turn **overscan off**. It's called "Just Scan", "Screen Fit" or "Fit to screen" depending on the brand.
- Set the HDMI input to **PC** or **Game** mode.
- Turn off motion smoothing and extra sharpening.
- Upload images and videos at the screen's resolution or higher.

## 7. You are the platform admin

The account created at first login is the **platform admin**. You get an extra **Platform → Customers & usage** menu, where you can:
- create customer organizations, each with an owner account, a plan and a screen limit,
- turn the AI agent on or off per customer and set their monthly AI allowance,
- suspend a customer. Their users can't sign in and their screens show a "disabled" message, but nothing is deleted, and you can reactivate them any time.
- see every customer's screens, storage, AI usage and recent AI activity.

To give a colleague the same rights, first create their account, then run:

```bash
qivo exec qivomedia qivo-server make-admin colleague@example.com
```

## 8. Turn on the AI agent (optional)

1. Create an API key at console.anthropic.com. Billing is per use.
2. Add it to `deploy/.env`: `ANTHROPIC_API_KEY=sk-ant-...`
3. Apply it: `qivo up -d`

The **Create with AI** button then appears in **Layouts**, and an **Ask AI to change** bar appears in the layout designer. Everything the AI makes is a draft. It isn't shown on any screen until someone assigns it. Each AI design is usually a few thousand tokens. The Platform page shows token use per month so you can watch the cost.

## 9. Set up email

Email is used for **password resets**, **team invitations** and **new demo-request alerts**. Any SMTP provider works, for example Brevo, Mailjet, Amazon SES, Google Workspace or Microsoft 365. Add your provider's settings to `deploy/.env`:

```
SMTP_HOST=smtp-relay.brevo.com
SMTP_PORT=587
SMTP_TLS=starttls
SMTP_USER=your-login
SMTP_PASSWORD=your-smtp-key
SMTP_FROM=Qivomedia <no-reply@qivomedia.com>
QIVO_PUBLIC_URL=https://app.qivomedia.com
```

Then run `qivo up -d`.

- So that your emails don't land in spam, add your provider's **SPF and DKIM** records to your domain's DNS. Every provider documents the exact records.
- **Without email**, everything still works. Reset links are written to the log (`qivo logs qivomedia | grep reset`), and you can add team members with a temporary password instead of an invitation.

**Security built in:** see [SECURITY.md](SECURITY.md) for the full list. After installing, we recommend three things:
1. Turn on **two-factor authentication** for your own account under **Team & audit → Security**.
2. Tick **Require two-factor authentication for every member**.
3. Set `QIVO_SECURITY_CONTACT` in `deploy/.env` so researchers know where to report issues.

## 10. Website + app on qivomedia.com (SaaS mode)

For your own SaaS you can run the public website on the main domain and the app on `app.` with the same server:

1. DNS: add an **A record for `app`** pointing to the server, next to `@` and `www`.
2. In `deploy/.env` set `CADDYFILE=Caddyfile.saas`.
3. Build the website and restart:
   ```bash
   python3 site/build.py      # writes site/dist (English /, French /fr/, Arabic /ar/)
   qivo up -d
   ```

- `https://qivomedia.com` is the website. `https://app.qivomedia.com` is the dashboard, and screens open `https://app.qivomedia.com/player`.
- **Hero demo:** in the dashboard, open a layout and click **Show on website**. It plays live in the hero. Until you choose one, a built-in demo plays.
- **Demo requests** from the form appear under **Platform → Customers & usage → Website demo requests**, where you can mark them contacted, won or lost. The form is protected by a bot trap and a limit of 5 requests per hour per visitor.
- **Edit the text and prices** in `site/src/i18n.json`. The prices are the `*_price` keys and currently say "On request". Run `python3 site/build.py` again afterwards. No restart is needed.

Universities and other self-hosted customers keep the default `CADDYFILE=Caddyfile`: one domain, no website.

## Everyday operations

All commands run from `/opt/qivomedia`. To save typing:

```bash
alias qivo='docker compose -f deploy/docker-compose.prod.yml --env-file deploy/.env'
```

| Task | Command |
|---|---|
| Status | `qivo ps` |
| Live logs | `qivo logs -f qivomedia` |
| Restart | `qivo restart qivomedia` |
| Stop / start | `qivo down` / `qivo up -d` |
| Back up now | `./deploy/backup.sh` |

### Updating to a new version

```bash
cd /opt/qivomedia
# replace the code with the new version (unzip / git pull), keeping deploy/.env
qivo up -d --build
```

The database updates itself on start. Screens reconnect on their own and keep playing their cached content during the few seconds of restart.

### Backups

`deploy/backup.sh` creates one `.tar.gz` holding a consistent copy of the database and all media, in `/var/backups/qivomedia`. It keeps the last 14. **Copy these off the server regularly**, for example to another server, an S3 bucket or your NAS. A backup on the same disk doesn't protect you if that disk fails.

### Restoring a backup

```bash
qivo stop qivomedia
mkdir -p /tmp/restore && tar -xzf /var/backups/qivomedia/qivomedia-YYYYMMDD-HHMMSS.tar.gz -C /tmp/restore
docker run --rm -v qivomedia_qivo-data:/data -v /tmp/restore:/restore debian:bookworm-slim sh -c '
  rm -f /data/qivo.db /data/qivo.db-wal /data/qivo.db-shm &&
  cp /restore/backups/qivo-*.db /data/qivo.db &&
  cp -a /restore/media/. /data/media/ &&
  chown -R 10001:10001 /data'
qivo start qivomedia
```

## Troubleshooting

| Problem | Check |
|---|---|
| Browser shows a certificate error | DNS must point to this server, and ports 80 and 443 must be open. Check `qivo logs caddy`. |
| Screen stuck on "Loading content…" | Open `https://your-domain/api/health` on that screen's browser. It should say `ok`. |
| Screen shows "Offline" in the dashboard but plays | It's playing from its cache. Check that device's network. It reconnects automatically. |
| Upload fails for big videos | Check free disk space with `df -h`. |
| Forgot a password | Use **Forgot your password?** on the sign-in page. Without email set up, copy the link from `qivo logs qivomedia`. |
| "Too many failed attempts" | The account locks for 15 minutes after 5 wrong passwords. Wait, or reset the password. |
| Lost phone with 2FA | Use one of the recovery codes on the code screen, or ask an admin to click **Reset 2FA** next to your name in Team. If the person is their organization's only admin, the platform admin can reset it under **Customers & usage → Users**. |
| Platform admin lost their 2FA phone | On the server: `qivo exec qivomedia qivo-server reset-mfa you@example.com`, then sign in with your password and set 2FA up again. |
| Signed out unexpectedly | Sessions end after 12 hours without activity (`QIVO_SESSION_IDLE_HOURS`), or when someone signs out your other sessions. |

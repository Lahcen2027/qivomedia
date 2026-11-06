#!/usr/bin/env bash
# One-shot installer for Ubuntu 22.04+/Debian 12+ (run as root from the repository root):
#   sudo ./deploy/install.sh
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(id -u)" -ne 0 ]; then echo "Run as root: sudo ./deploy/install.sh"; exit 1; fi

echo "== Qivomedia installer =="

# 1. Docker
if ! command -v docker >/dev/null 2>&1; then
  echo "-> Installing Docker"
  curl -fsSL https://get.docker.com | sh
fi
docker compose version >/dev/null

# 2. Settings
if [ ! -f deploy/.env ]; then
  read -rp "Domain (e.g. qivomedia.com): " DOMAIN
  read -rp "Email for HTTPS certificates: " EMAIL
  sed -e "s/^QIVO_DOMAIN=.*/QIVO_DOMAIN=${DOMAIN}/" -e "s/^ACME_EMAIL=.*/ACME_EMAIL=${EMAIL}/" \
      deploy/.env.example > deploy/.env
  echo "-> Wrote deploy/.env"
fi
. deploy/.env

# 3. DNS sanity check
SERVER_IP="$(curl -fsS https://api.ipify.org || true)"
DNS_IP="$(getent ahostsv4 "$QIVO_DOMAIN" | awk 'NR==1{print $1}' || true)"
if [ -n "$SERVER_IP" ] && [ "$SERVER_IP" != "$DNS_IP" ]; then
  echo "!! $QIVO_DOMAIN points to '${DNS_IP:-nothing}' but this server is $SERVER_IP."
  echo "   HTTPS will fail until the DNS A record points here. Continue anyway? [y/N]"
  read -r ok; [ "$ok" = "y" ] || exit 1
fi

# 4. Firewall (if ufw is present)
if command -v ufw >/dev/null 2>&1; then
  ufw allow 22/tcp >/dev/null; ufw allow 80/tcp >/dev/null; ufw allow 443/tcp >/dev/null; ufw allow 443/udp >/dev/null
  ufw --force enable >/dev/null
  echo "-> Firewall: 22, 80, 443 open"
fi

# 5. Start
docker compose -f deploy/docker-compose.prod.yml --env-file deploy/.env up -d --build

# 6. Nightly backups
chmod +x deploy/backup.sh
CRON="15 3 * * * cd $(pwd) && ./deploy/backup.sh >> /var/log/qivo-backup.log 2>&1"
( crontab -l 2>/dev/null | grep -v 'deploy/backup.sh' ; echo "$CRON" ) | crontab -
echo "-> Nightly backup scheduled at 03:15 (kept 14 days in /var/backups/qivomedia)"

echo
echo "Done. Open https://$QIVO_DOMAIN to create your organization,"
echo "then open https://$QIVO_DOMAIN/player on each screen to pair it."

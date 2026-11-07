#!/usr/bin/env bash
# Back up Qivomedia: a consistent copy of the database + all media files + the encryption key.
# The archive can read stored SSO client secrets: keep it as private as the server itself.
# Run from the repository root. Keeps the last $KEEP backups.
# Cron example (every night at 03:15):
#   15 3 * * * cd /opt/qivomedia && ./deploy/backup.sh >> /var/log/qivo-backup.log 2>&1
set -euo pipefail

BACKUP_DIR="${BACKUP_DIR:-/var/backups/qivomedia}"
KEEP="${KEEP:-14}"
COMPOSE="docker compose -f deploy/docker-compose.prod.yml --env-file deploy/.env"
STAMP="$(date +%Y%m%d-%H%M%S)"

mkdir -p "$BACKUP_DIR"

# 1. Database: online copy made inside the container (safe while running).
$COMPOSE exec -T qivomedia qivo-server backup "/data/backups/qivo-$STAMP.db"

# 2. Database copy + media + encryption key (needed to read stored SSO secrets) → one archive on the host.
$COMPOSE exec -T qivomedia sh -c "cd /data && tar -czf - backups/qivo-$STAMP.db media \$(ls secret.key 2>/dev/null)" > "$BACKUP_DIR/qivomedia-$STAMP.tar.gz"
$COMPOSE exec -T qivomedia rm -f "/data/backups/qivo-$STAMP.db"

# 3. Rotate.
ls -1t "$BACKUP_DIR"/qivomedia-*.tar.gz | tail -n +"$((KEEP + 1))" | xargs -r rm -f

echo "$(date -Is) backup OK: $BACKUP_DIR/qivomedia-$STAMP.tar.gz ($(du -h "$BACKUP_DIR/qivomedia-$STAMP.tar.gz" | cut -f1))"

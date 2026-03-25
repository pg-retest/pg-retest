#!/bin/bash
# Take a pg_basebackup from db-a into the shared volume.
# Run this BEFORE starting workload capture.
#
# Usage (from project root):
#   docker compose -f docker-compose.benchmark.yml exec db-a /pitr/pitr-basebackup.sh
#
# Or from host:
#   docker compose -f docker-compose.benchmark.yml exec db-a bash -c \
#     'pg_basebackup -D /backups/base -U demo -Fp -Xs -P -R'

set -e

BACKUP_DIR="/backups/base"

echo "=== pg-retest PITR: Taking base backup ==="

# Clean previous backup
if [ -d "$BACKUP_DIR" ]; then
    echo "  Removing previous backup..."
    rm -rf "$BACKUP_DIR"
fi

# Force a WAL segment switch so archived WAL is current
psql -U demo -d ecommerce -c "SELECT pg_switch_wal();" 2>/dev/null || true

# Take the base backup
# -D: target directory
# -U: connect as this user
# -Fp: plain format (directory)
# -Xs: stream WAL during backup
# -P: show progress
# -R: write recovery config (standby.signal + postgresql.auto.conf)
echo "  Running pg_basebackup..."
pg_basebackup \
    -D "$BACKUP_DIR" \
    -U demo \
    -Fp \
    -Xs \
    -P \
    -R \
    --checkpoint=fast

echo "  Base backup complete: $BACKUP_DIR"
echo "  Size: $(du -sh "$BACKUP_DIR" | cut -f1)"
echo ""
echo "  Next: start workload, capture via proxy (restore point auto-created),"
echo "  then restore db-b to that point."

#!/bin/bash
# PITR-equivalent restore for Docker demo
# Usage: ./demo/pitr-restore.sh [restore-point-name]
#
# In production, you'd use pg_basebackup + WAL replay to the restore point.
# In Docker, we use pg_dump at capture time + pg_restore.

set -e

DB_A="${DB_A:-host=localhost port=5450 dbname=ecommerce user=demo password=demo}"
DB_B="${DB_B:-host=localhost port=5451 dbname=ecommerce user=demo password=demo}"
BACKUP_FILE="${BACKUP_FILE:-/tmp/pg-retest-pitr.dump}"

echo "=== pg-retest PITR Restore ==="
echo "This creates a point-in-time copy of db-a on db-b."
echo ""

if [ "$1" = "backup" ]; then
    echo "Taking backup of db-a..."
    pg_dump "$DB_A" --clean --if-exists -Fc -f "$BACKUP_FILE"
    echo "Backup saved to $BACKUP_FILE ($(du -h "$BACKUP_FILE" | cut -f1))"
elif [ "$1" = "restore" ]; then
    echo "Restoring backup to db-b..."
    pg_restore --clean --if-exists -d "$DB_B" "$BACKUP_FILE" 2>/dev/null || true
    echo "Restore complete"
    echo "db-b customers: $(psql "$DB_B" -t -c 'SELECT count(*) FROM customers' 2>/dev/null | tr -d ' ')"
    echo "db-b orders: $(psql "$DB_B" -t -c 'SELECT count(*) FROM orders' 2>/dev/null | tr -d ' ')"
else
    echo "Usage: $0 backup|restore"
    echo ""
    echo "  backup   Take a pg_dump of db-a (run this JUST BEFORE starting capture)"
    echo "  restore  Restore the dump to db-b (run this BEFORE replaying)"
fi

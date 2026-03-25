#!/bin/bash
mkdir -p /var/lib/postgresql/wal_archive
# Delegate to the standard entrypoint
exec docker-entrypoint.sh "$@"

# ID Correlation

## Problem Statement

When PostgreSQL uses sequences (serial/IDENTITY columns) to generate primary keys, replaying a captured workload against a restored database backup produces different IDs than the original execution. This causes foreign key violations, incorrect JOINs, and misleading error rates in replay results. The ID correlation feature ensures that sequence-generated values are consistent between capture and replay, enabling accurate performance comparison for write-heavy workloads.

## ID Handling Modes

pg-retest provides four ID handling modes via the `--id-mode` flag:

### `none` (default)

No ID handling. Sequences on the target database are left as-is. This is the existing behavior and works well for read-only replays or when the target is freshly restored from the same backup point.

### `sequence`

Snapshots all user-defined sequences from the source database at capture time and restores them on the target before replay. This ensures that `nextval()` calls during replay produce the same values as the original execution, provided the replay order matches.

**When to use:** Write workloads where INSERT ordering is deterministic and you want 1:1 ID reproduction without proxy-level capture changes.

### `correlate`

*(Phase 2 - not yet implemented)* Captures RETURNING clause values during proxy capture and builds an ID mapping table. During replay, substitutes old IDs with new ones in subsequent queries. This handles non-deterministic ordering where sequence reset alone is insufficient.

### `full`

*(Phase 2 - not yet implemented)* Combines `sequence` reset with `correlate` substitution for maximum fidelity. Sequences are reset first, then any remaining mismatches are handled by the correlation map.

## Usage Examples

### Proxy Capture with Sequence Snapshot

Capture a workload through the proxy while snapshotting sequences from the source database:

```bash
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target localhost:5432 \
  --output workload.wkl \
  --id-mode sequence \
  --source-db "host=localhost port=5432 dbname=myapp user=myuser password=mypass"
```

The `--source-db` flag provides a connection string used to query `pg_sequences` before capture begins. The snapshot is embedded in the `.wkl` profile file.

### Replay with Sequence Restore

Replay the workload, restoring sequences on the target before execution:

```bash
pg-retest replay \
  --workload workload.wkl \
  --target "host=target-host dbname=myapp user=myuser password=mypass" \
  --output results.wkl \
  --id-mode sequence
```

Before replay begins, pg-retest connects to the target database and calls `setval()` for each sequence in the snapshot, resetting them to their captured state.

### Log-Based Capture

For log-based capture (`--source-type pg-csv`), there is no live database connection available. A warning is emitted if `--id-mode sequence` is used:

```bash
pg-retest capture \
  --source-log /var/log/postgresql/postgresql.csv \
  --output workload.wkl \
  --id-mode sequence
# WARNING: Sequence snapshot will not be included. Use proxy capture with --source-db.
```

## Known Limitations

- **Sequence mode requires deterministic replay ordering.** If the original workload had concurrent INSERTs across sessions, the sequence values may not match exactly because session scheduling during replay is not guaranteed to be identical.

- **Log-based capture cannot snapshot sequences.** Only proxy-based capture supports sequence snapshots, because it requires a live database connection at capture time.

- **Persistent proxy mode does not yet support sequence snapshots.** The snapshot is only taken for non-persistent (single-run) proxy mode. Persistent proxy sessions will need separate handling in a future update.

- **The `correlate` and `full` modes are not yet implemented.** These are planned for Phase 2 and will add RETURNING value capture and ID substitution during replay.

- **Sequences in `pg_catalog` and `information_schema` are excluded.** Only user-defined sequences are captured.

- **TLS for sequence snapshot in proxy mode.** The proxy `--source-db` connection currently uses NoTls. If TLS is required, include `sslmode=require` in the connection string (handled by the PostgreSQL driver).

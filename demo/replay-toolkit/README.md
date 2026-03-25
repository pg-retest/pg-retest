# pg-retest Replay Toolkit

Drop a `.wkl` workload file in, replay it against any PostgreSQL target. Zero setup.

## Quick Start

```bash
# 1. Copy your captured workload
cp /path/to/my-workload.wkl workloads/

# 2. Start a fresh target DB (or point to your own)
docker compose up -d target-db

# 3. Load your schema into the target
docker compose exec target-db psql -U replay -d replay_target -f /path/to/schema.sql

# 4. Replay with ID correlation
docker compose run replay my-workload.wkl

# 5. Compare results
docker compose run compare my-workload.wkl
```

## Use Your Own Target

Skip the built-in `target-db` and point directly at your database:

```bash
TARGET="host=your-db.example.com dbname=myapp user=myuser password=secret" \
ID_MODE=full \
docker compose run replay my-workload.wkl
```

## Capture New Workloads

Point your application at the proxy to capture traffic:

```bash
# Start the proxy (listens on port 5433, forwards to target-db)
docker compose up proxy

# In your app config, change DB host to localhost:5433
# Traffic flows: app → proxy (captures) → target-db

# When done, the workload is at workloads/capture.wkl
```

## ID Modes

| Mode | Env Var | What It Does |
|------|---------|-------------|
| `none` | `ID_MODE=none` | No ID handling — raw replay |
| `sequence` | `ID_MODE=sequence` | Reset sequences before replay |
| `correlate` | `ID_MODE=correlate` | Remap RETURNING values during replay |
| `full` | `ID_MODE=full` | Both (default) — maximum fidelity |

## Files

```
workloads/      ← Put .wkl files here
results/        ← Replay results appear here
```

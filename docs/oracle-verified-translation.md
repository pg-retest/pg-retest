# Oracle-Verified SQL Translation & Replay (experimental)

> **Status:** experimental, behind the `polyglot-transform` Cargo feature (OFF by
> default). Branch `experimental/polyglot-transform`. Nothing here is compiled or linked
> into a default build.

This is pg-retest's **migration-validation** path: translate a captured MySQL workload to
PostgreSQL using several tools at once, and **prove each translation is behavior-preserving
by executing it** — never trust a translation on syntax alone. The verifier (the
"oracle") is the sole arbiter of correctness, which is what makes it safe to throw a
brittle regex rule, an AST transpiler, and even a nondeterministic LLM at the same problem.

---

## 1. Turn it on

The whole subsystem is gated by one Cargo feature:

```bash
cargo build  --features polyglot-transform      # compiles the transpiler + oracle
cargo test   --features polyglot-transform       # runs everything (DB-gated parts skip)
```

The default build is unchanged — PG-only, no extra dependency linked:

```bash
cargo build          # polyglot-sql is NOT compiled or linked
```

`Cargo.toml`:

```toml
[features]
polyglot-transform = ["dep:polyglot-sql"]   # OFF by default
```

## 2. The pieces

**Candidate generators** — each proposes one PostgreSQL translation; all implement one
async trait (`transform::oracle::engine::CandidateGenerator`):

| Generator | Kind | Needs | Notes |
|---|---|---|---|
| `regex` | deterministic, in-tree | nothing | the legacy 7 rules — now safe because oracle-gated |
| `polyglot` | deterministic, in-tree | nothing | AST transpiler (MIT crate); never mangles literals |
| `sqlglot` | subprocess | a Python with `sqlglot` | more complete (e.g. `IF()`→`CASE`, `DATE_FORMAT` codes) |
| `llm` | HTTP | an OpenAI-compatible endpoint | translates what the others can't; verified, so safe |

**Oracles** — establish "behavior-preserving" (`transform::oracle`):

| Oracle | Truth | Verifies | Needs |
|---|---|---|---|
| `GoldenOracle` | an author-verified reference query, run on PG | result-set reads | PostgreSQL |
| `LiveDiffOracle` | the **original on a real MySQL** | result-set reads (truest) | PostgreSQL + MySQL |
| `LiveDiffOracle::verify_write` | resulting **table state** | INSERT/UPDATE/DELETE | PostgreSQL + MySQL |

The engine (`translate_verified`) tries generators cheapest-first and accepts the first the
oracle certifies `Equivalent`; everything is recorded for an audit trail, and anything no
generator can faithfully translate is honestly **skipped**.

## 3. Setup (step by step)

### 3a. PostgreSQL target (required)

Any reachable PostgreSQL works. A throwaway container:

```bash
docker run -d --name pgretest-oracle \
  -e POSTGRES_USER=oracle -e POSTGRES_PASSWORD=oracle -e POSTGRES_DB=oracle \
  -p 5441:5432 postgres:16
export PG_RETEST_ORACLE_URL="host=localhost port=5441 user=oracle password=oracle dbname=oracle"
```

### 3b. MySQL reference (optional — for the live differential & writes oracles)

```bash
docker run -d --name pgretest-oracle-mysql \
  -e MYSQL_ROOT_PASSWORD=root -e MYSQL_DATABASE=oracle \
  -p 3310:3306 mysql:8.0
# wait until ready: docker exec pgretest-oracle-mysql mysql -uroot -proot -e "SELECT 1"
export PG_RETEST_MYSQL_CMD="docker exec pgretest-oracle-mysql mysql -uroot -proot -N --batch --raw oracle"
```

The oracle reaches MySQL by appending `-e <sql>` to `PG_RETEST_MYSQL_CMD` (no Rust MySQL
driver — the same external-tool pattern pg-retest uses for the aws/bedrock CLIs).

### 3c. sqlglot generator (optional)

`sqlglot` is a Python package; install it into an isolated environment and point the
generator at that interpreter:

```bash
uv venv /tmp/sqlglot-venv
uv pip install --python /tmp/sqlglot-venv/bin/python sqlglot
export PG_RETEST_SQLGLOT_PYTHON=/tmp/sqlglot-venv/bin/python
```

(If `import sqlglot` fails, the generator simply declines — it never breaks a run.)

### 3d. LLM generator (optional)

Any OpenAI-compatible chat endpoint (hosted, or local Ollama):

```bash
export PG_RETEST_LLM_URL="http://localhost:11434/v1/chat/completions"
export PG_RETEST_LLM_MODEL="llama3"        # default: gpt-4o-mini
export PG_RETEST_LLM_KEY="sk-..."          # optional, for hosted providers
```

### Environment variables

| Variable | Purpose | Default |
|---|---|---|
| `PG_RETEST_ORACLE_URL` | PostgreSQL target (libpq keyword/value string) | `host=localhost port=5441 user=oracle password=oracle dbname=oracle` |
| `PG_RETEST_MYSQL_CMD` | MySQL client invocation (argv; `-e <sql>` appended) | unset → live/writes oracle disabled |
| `PG_RETEST_SQLGLOT_PYTHON` | Python interpreter with `sqlglot` | `python3` |
| `PG_RETEST_LLM_URL` | OpenAI-compatible chat endpoint | unset → LLM generator disabled |
| `PG_RETEST_LLM_MODEL` / `PG_RETEST_LLM_KEY` | LLM model / bearer token | `gpt-4o-mini` / none |

Every DB/tool-gated test **skips cleanly** when its requirement is missing, so CI without
any infrastructure stays green.

## 4. Run the benchmarks (the proofs)

| Command (prefix `cargo test --features polyglot-transform`) | Proves | Needs |
|---|---|---|
| `--test polyglot_benefit_harness -- --nocapture` | regex vs transpiler, syntactic: regex 6 mistranslations → transpiler 0 | nothing |
| `--test oracle_translation_benchmark -- --nocapture` | behavioral: engine picks the right engine per query | PG |
| `--test oracle_live_mysql_test -- --nocapture` | translations match the **original on real MySQL** | PG + MySQL |
| `--test oracle_writes_test -- --nocapture` | INSERT/UPDATE/DELETE verified by **table state** | PG + MySQL |
| `--test oracle_sqlglot_test -- --nocapture` | sqlglot wins a case polyglot fails | PG + sqlglot |
| `--test oracle_deep_benchmark -- --nocapture` | **deep** generator × construct coverage matrix | PG + MySQL (+ sqlglot/LLM) |

> When running multiple DB-gated oracle binaries against one live PostgreSQL, run them one
> at a time or with `-- --test-threads=1` — they reset a shared `oracle_users` fixture.

Example — the deep matrix with three generators:

```bash
export PG_RETEST_ORACLE_URL="host=localhost port=5441 user=oracle password=oracle dbname=oracle"
export PG_RETEST_MYSQL_CMD="docker exec pgretest-oracle-mysql mysql -uroot -proot -N --batch --raw oracle"
export PG_RETEST_SQLGLOT_PYTHON=/tmp/sqlglot-venv/bin/python
cargo test --features polyglot-transform --test oracle_deep_benchmark -- --nocapture
```

```
  per-generator behavioral coverage (oracle-verified Equivalent):
    regex      19/23      polyglot   21/23      sqlglot    23/23
    UNION      23/23   <- multi-pass (any generator verified)
```

### End-to-end replay validation

`scripts/e2e-replay.sh` spins up throwaway PostgreSQL + MySQL containers, drives real
`pg-retest` commands, and validates replay by observing target-DB state changes — then
tears everything down. It covers: PG→PG (CSV capture), PG→PG (live proxy capture),
MySQL→PG (mysql-slow capture + transform), **Oracle→PG (SQL Trace 10046 capture →
oracle-replay → replay)**, the `oracle-replay` command, and duplicate sessions
(`replay --scale 3`).

```bash
./scripts/e2e-replay.sh
# ================ RESULTS:  9 passed, 0 failed ================
```

**Oracle sources (upload — pg-retest never connects to Oracle).** Two capture types, by
how much the DBA can produce:

| `--source-type` | Upload | Fidelity |
|---|---|---|
| `oracle-trace` | a SQL Trace (event 10046) `.trc` file | **High** — ordered, per-session, timed, with **bind values** (the `BINDS` sections are substituted: `WHERE id = :1` + `value=42` → `WHERE id = 42`). Best for OLTP replay. Top-level statements only; recursive dictionary SQL filtered. |
| `oracle-awr` | a CSV from `V$SQL` / AWR / `DBA_HIST_SQLTEXT` (one query → export) | **Summary** — distinct SQL shapes + counts, easiest to produce. No bind *values* (Oracle shares cursors via binds), so parameterized SQL skips on replay; literal/reporting SQL replays. Needs a `sql_text` column; optional `executions`/`elapsed_us`. |

`oracle-replay` is dialect-aware for both: an Oracle workload is translated by sqlglot
(`read='oracle'`), and anything sqlglot can't faithfully translate (e.g. `ROWNUM`) is
behaviorally rejected by `--verify live`, never shipped.

```bash
pg-retest capture --source-type oracle-trace --source-log orcl_ora_12345.trc --output ora.wkl
PG_RETEST_SQLGLOT_PYTHON=/path/to/venv/bin/python \
  pg-retest oracle-replay --input ora.wkl --output ora_pg.wkl --verify syntactic
pg-retest replay --workload ora_pg.wkl --target "host=… dbname=…"
```

> The script builds with `--features polyglot-transform` because `oracle-replay` is
> feature-gated — a plain `cargo build` produces a binary *without* the command.

## 5. Build an oracle replay (end-to-end workflow)

The migration-validation pipeline is **capture → transform → replay → compare**, with the
oracle gating the transform. The building blocks:

```
 ┌──────────┐   .wkl    ┌───────────────────────────┐  verified PG   ┌──────────┐   ┌─────────┐
 │ Capture  │──────────▶│ Multi-pass translate      │───────────────▶│ Replay   │──▶│ Compare │
 │ (MySQL)  │ source_   │ + ORACLE verify (skip the │  translations  │ (on PG)  │   │ report  │
 └──────────┘ dialect=  │ unverifiable)             │                └──────────┘   └─────────┘
              MySql      └───────────────────────────┘
```

**Step 1 — Capture the MySQL workload** (CLI; stamps `source_dialect = MySql`):

```bash
cargo run --features polyglot-transform -- capture \
  --source-type mysql-slow --log /path/to/mysql-slow.log \
  --output workload.wkl
```

**Step 2 — Stand up the oracle infrastructure** (§3): PG target, MySQL reference, and any
optional generators (sqlglot/LLM). Seed the target and reference with **equivalent** data
(point-in-time restore for a true 1:1 replay; for validation, an equivalent schema+data).

**Step 3 — Translate, oracle-verified** (the `oracle-replay` command). For each captured
statement, the multi-pass engine runs the generators, the oracle verifies each candidate,
and the first accepted translation wins; unverifiable statements are dropped and reported.
`original_sql` (the source-native MySQL) is retained on every translated statement, and the
output's `source_dialect` becomes `Postgres`:

```bash
# syntactic acceptance (PostgreSQL parser; no database needed):
cargo run --features polyglot-transform -- oracle-replay \
  --input workload.wkl --output translated.wkl --verify syntactic

# behavioral acceptance (execute original on real MySQL, candidate on PG, diff):
PG_RETEST_ORACLE_URL="host=... dbname=..." \
PG_RETEST_MYSQL_CMD="docker exec <mysql> mysql -uroot -proot -N --batch --raw <db>" \
cargo run --features polyglot-transform -- oracle-replay \
  --input workload.wkl --output translated.wkl --verify live
```

It prints an `Oracle-Replay Report` (statements / translated / skipped, counts per winning
generator, and the skipped statements with reasons). `--verify syntactic` proves only that
PostgreSQL *parses* each translation; use `--verify live` for a migration decision (it
requires a MySQL reference and a PG target seeded to match the captured schema/data).
sqlglot and the LLM are added to the cascade automatically when `PG_RETEST_SQLGLOT_PYTHON` /
`PG_RETEST_LLM_URL` are set.

> **Note:** skipped statements are dropped individually; transaction-aware skipping
> (dropping a whole transaction when any of its statements is unverifiable — FR-XFORM-8) is
> a follow-on. Review the report before replaying a transactional workload.

**Step 4 — Replay the verified translations on PostgreSQL** (existing engine):

```bash
cargo run -- replay --profile translated.wkl --target "host=... dbname=..."
```

**Step 5 — Compare source vs target** (existing engine):

```bash
cargo run -- compare --source source-metrics.json --target target-metrics.json
```

The result is a migration-validation report backed by a hard guarantee: every replayed
statement was **proven behavior-preserving by execution**, and anything that couldn't be is
visibly skipped rather than silently mistranslated.

## 6. How verification works

For one statement, given the generators, an oracle, the original MySQL, and a reference:

1. Ask each generator (cheapest first) for a candidate PG translation.
2. The oracle **executes** the candidate (and the reference / the original-on-MySQL) and
   diffs the canonical result rows (reads) or the resulting table state (writes).
3. The first `Equivalent` candidate is accepted; its generator is recorded as the winner.
4. If none are `Equivalent`, the statement is skipped — never replayed as if translated.

`Equivalent` means *PostgreSQL ran it and it returned the right rows / produced the right
state* — behavioral, not syntactic. Compare with the transpiler's own `pg_query` gate
(which proves only that PostgreSQL *parses* the output): the oracle is strictly stronger,
and it is what catches translations that parse, run, and still lie (e.g. `DATE_FORMAT` with
un-converted format codes).

## 7. Honest limitations

- **Result normalization is scoped to int/text/date** so cross-engine canonicalization is
  exact. Float/decimal tolerance, timezones, and values containing tabs/newlines are a
  Phase-2e follow-on.
- **Truth fidelity has tiers.** `GoldenOracle` trusts an author-verified reference;
  `LiveDiffOracle` removes the human by running the real MySQL. The live oracle is the one
  to use for a real migration decision.
- **Syntactic ≠ behavioral ≠ semantic-under-all-data.** A pass on a corpus proves behavior
  *on that data*; widen the seed/corpus for stronger guarantees.
- **The single-command orchestration** (`.wkl` → oracle-translate → replay) is not yet a
  CLI subcommand; the pieces exist and are exercised by the tests/benchmarks.
- **Pre-1.0 `polyglot-sql` (0.5.4)** — pinned + `Cargo.lock` committed; the benchmarks are
  the behavior tripwire on upgrade.

See `EXPERIMENT-REPORT.md` for the full build log and pasted results, and
`docs/superpowers/specs|plans/2026-06-15-oracle-verified-translation*` for the design.

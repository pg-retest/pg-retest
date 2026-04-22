# Binary Bind-Parameter Capture

This document explains how pg-retest captures and replays PostgreSQL bind
parameters sent in binary format by modern client drivers (pgx, libpq,
asyncpg, psycopg3, JDBC, etc.). It covers the rc.4 fix, what it enables,
and the remaining edges.

## The bug (pre-rc.4)

Before this fix, pg-retest captured workloads correctly for drivers that
used PostgreSQL's simple query protocol (psql, pgbench-default), but
produced unusable captures for anything using the extended query protocol
with binary-format bind parameters — which is the default for every
modern driver: pgx, libpq with prepared statements, asyncpg, psycopg3,
JDBC, etc. The proxy would receive a `Bind` message containing raw wire
bytes (say `[0, 0, 0, 42]` for int4 `42`, or 16 bytes of big-endian
half-words for a UUID), hand them to `format_bind_params`, and — finding
them non-UTF-8 — stringify them to the literal placeholder
`'<binary 4 bytes>'`. That placeholder got substituted into the captured
SQL as if it were a value, so every replay of a query with binary params
produced `ERROR: invalid input syntax for type integer: "<binary 4 bytes>"`.
In our first E2E against a real Go+pgx app, 845,146 queries failed with
that exact signature and the replay never produced an output artifact. 0%
functional replay. The bug hid behind pgbench and simple-protocol tests
for months because neither of them use the Bind message path.

## The fix

The fix is a **type-aware binary decoder at capture time** — no profile
format bump, no replay engine changes. The proxy now preserves the
per-parameter format codes from the `Bind` message and tracks type OIDs
per prepared statement from two sources: the client's `Parse` message
when it declares non-zero OIDs, and the server's `ParameterDescription`
response to a client `Describe Statement`. A `pending_describe` mutex
carries the statement name from the client-to-server relay to the
server-to-client relay so each `ParameterDescription` attaches to the
correct cached entry. At `Bind` time, binary-format parameters get
decoded through `pg_binary::binary_to_sql_literal` into proper SQL
text — `'42'`, `'2001:db8::1'::inet`, `'[1.5,2.5,3.5]'::vector` — which
the existing simple-protocol replay substitutes directly. The decoder
covers 24 builtin types (every integer/float width, uuid, numeric with
full base-10000 digit reconstruction, all temporal types, inet/cidr/mac
with RFC 5952 IPv6 compression, bit strings, bytea, jsonb, xml, money,
interval), 1-D arrays of the scalar types with PG array-quoting rules,
and extension types with dynamic OIDs (pgvector's `vector`, `halfvec`
with IEEE-754 binary16 decode, and `sparsevec`) that are discovered at
proxy startup by probing `pg_type`. Unknown OIDs still fall back to the
legacy `'<binary N bytes>'` placeholder so behavior only improves.
Re-run against the same 85M-query Yonk app workload: replay completed
with a 99.977% success rate and the compare report produced a clean PASS.

## Coverage reference

### Built-in types (always decoded)

| Category | Types |
|---|---|
| Boolean | `bool` |
| Integer | `int2`, `int4`, `int8`, `oid` family (oid, xid, cid, regproc, regprocedure, regoper, regoperator, regclass, regtype) |
| Floating point | `float4`, `float8` |
| Character | `text`, `varchar`, `bpchar`, `name`, `char` |
| Binary | `bytea` (as `E'\\x...'`) |
| JSON | `json`, `jsonb`, `xml` |
| Money | `money` (rendered as decimal cast to `::money`) |
| Temporal | `date`, `time`, `timetz`, `timestamp`, `timestamptz`, `interval` |
| Network | `inet`, `cidr` (IPv4+IPv6, RFC 5952 compression), `macaddr`, `macaddr8` |
| Bit strings | `bit`, `varbit` |
| Numeric | `numeric` (full base-10000 digit reconstruction, NaN/Inf) |
| UUID | `uuid` |
| Arrays | 1-D of any of the above scalar types, with PG array quoting for string-like types |

### Extension types (discovered at startup)

When `--source-db` is supplied, the proxy queries `pg_type` for dynamic
OIDs at startup. Currently probed:

- `vector` (pgvector)
- `halfvec` (pgvector, IEEE-754 binary16 → f32)
- `sparsevec` (pgvector)

A log line like `Discovered extension type OIDs: vector, halfvec, sparsevec`
confirms the discovery succeeded.

## Still unsupported (falls back to `'<binary N bytes>'`)

These types come through as placeholder and will fail on replay — either
extend the decoder or ensure the client uses text format for them:

- Multi-dimensional arrays (2-D+) and non-default lower bounds
- Geometric types: `point`, `lseg`, `path`, `box`, `polygon`, `line`, `circle`
- Range types: `int4range`, `int8range`, `numrange`, `tsrange`, `tstzrange`, `daterange`
- Full-text search: `tsvector`, `tsquery`
- Other extension types not in the discovery probe: PostGIS `geometry`/`geography`,
  `hstore`, `citext`, user-defined enums, custom composite types

Failures are not silent: PG's error response contains the literal
`<binary N bytes>` string, which is a unique signature in logs.

## Limitations by design

- **Replay stays simple-protocol.** Decoded values are substituted as SQL
  text literals so the existing replay engine works unchanged. This means
  a target database that decodes text values slightly differently from
  binary ones could produce tiny numerical deltas. In practice every
  tested type round-trips exactly.
- **Target must have matching extensions installed.** If the source has
  `CREATE EXTENSION vector` but the target doesn't, decoded `'[...]'::vector`
  literals fail replay with `type "vector" does not exist`. The proxy
  surfaces this via the normal replay error path.
- **locale-sensitive money rendering.** `money` is rendered as a plain
  decimal cast (`'12345.67'::money`), which PG accepts regardless of
  `lc_monetary`. Some 3-decimal currencies may need a follow-up.

## Implementation pointers

- `src/proxy/pg_binary.rs` — all decoders; 48 unit tests including
  pgvector, RFC 5952 IPv6 compression, and numeric base-10000 edge cases.
- `src/proxy/protocol.rs::extract_bind` — preserves per-parameter format
  codes (handles the three wire shapes: `format_count == 0` / `== 1`
  broadcast / `== N` per-param).
- `src/proxy/protocol.rs::extract_parse` — pulls declared param OIDs out
  of the client's Parse.
- `src/proxy/protocol.rs::extract_parameter_description` — parses the
  server's `'t'` response.
- `src/proxy/connection.rs::handle_connection_inner` — `stmt_oids` and
  `pending_describe` caches correlate statements across the two relay
  directions.
- `src/correlate/capture.rs::discover_extension_oids` — probes `pg_type`
  for pgvector OIDs at startup.

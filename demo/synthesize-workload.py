#!/usr/bin/env python3
"""
Workload Synthesizer for pg-retest.

Takes a captured .wkl file, analyzes its statistical fingerprint, and produces:
  1. A new .wkl file with the same patterns but all IDs are fixed literals
     (no sequence/UUID auto-gen dependencies)
  2. A matching SQL data file that contains exactly the rows the workload references

This is NOT a copy — it's a statistical clone.  Same query mix %, same timing
distribution, same session structure, same table relationships.  But the data
and IDs are pre-computed so replay has ZERO errors.

Usage:
    python3 demo/synthesize-workload.py \\
        --input captured.wkl \\
        --source-db "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \\
        --output-workload synthetic.wkl \\
        --output-data synthetic-data.sql \\
        --seed 42

Requirements:
    pip install psycopg2-binary msgpack
    pg-retest binary on PATH (for workload inspection)
"""

import argparse
import json
import math
import os
import random
import re
import subprocess
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Dict, List, Optional, Tuple

try:
    import msgpack
except ImportError:
    sys.exit("ERROR: msgpack not installed.  Run: pip install msgpack")


# ---------------------------------------------------------------------------
# WorkloadFingerprint — statistical summary of a captured workload
# ---------------------------------------------------------------------------

@dataclass
class WorkloadFingerprint:
    """Statistical fingerprint of a captured workload."""
    total_sessions: int = 0
    total_queries: int = 0
    capture_duration_us: int = 0

    # Query mix (fractions that sum to 1.0, excluding BEGIN/COMMIT/ROLLBACK)
    select_pct: float = 0.0
    insert_pct: float = 0.0
    update_pct: float = 0.0
    delete_pct: float = 0.0

    # Per-table query counts  {"customers": 120, "orders": 300, ...}
    table_query_counts: Dict[str, int] = field(default_factory=dict)
    # Per-table per-op counts  {"customers": {"Select": 50, "Insert": 70}, ...}
    table_op_counts: Dict[str, Dict[str, int]] = field(default_factory=dict)

    # Session structure
    avg_queries_per_session: float = 0.0
    min_queries_per_session: int = 0
    max_queries_per_session: int = 0
    queries_per_session: List[int] = field(default_factory=list)

    # Timing (microseconds)
    think_times_us: List[int] = field(default_factory=list)
    avg_think_time_us: int = 0
    p50_think_time_us: int = 0
    p95_think_time_us: int = 0
    query_durations_us: List[int] = field(default_factory=list)
    avg_query_duration_us: int = 0

    # Transaction patterns
    pct_in_transaction: float = 0.0
    avg_txn_size: float = 3.0  # queries per transaction (excluding BEGIN/COMMIT)

    # Per-session kind distribution (list of dicts)
    session_kind_distributions: List[Dict[str, int]] = field(default_factory=list)

    # Query templates — actual SQL patterns from captured workload with placeholders
    # Each entry: (template_str, count, kind, tables, original_sql_sample)
    query_templates: List[dict] = field(default_factory=list)

    # Capture source info
    source_host: str = ""
    capture_method: str = ""
    pg_version: str = ""


# ---------------------------------------------------------------------------
# SchemaInfo — lightweight schema metadata from source DB
# ---------------------------------------------------------------------------

@dataclass
class TableInfo:
    name: str
    pk_col: str = "id"
    columns: List[Tuple[str, str]] = field(default_factory=list)  # (col_name, data_type)
    fk_refs: List[Tuple[str, str]] = field(default_factory=list)  # (col_name, referenced_table)
    max_id: int = 0
    row_count: int = 0
    is_uuid_pk: bool = False
    # Sample values per column for realistic generation
    sample_values: Dict[str, list] = field(default_factory=dict)


class SchemaAnalyzer:
    """Connects to source DB and extracts schema metadata."""

    def __init__(self, dsn: str):
        import psycopg2
        self.conn = psycopg2.connect(dsn)
        self.conn.autocommit = True

    def analyze(self, tables: List[str]) -> Dict[str, TableInfo]:
        """Return TableInfo for each requested table."""
        result = {}
        cur = self.conn.cursor()
        for tbl in tables:
            info = TableInfo(name=tbl)

            # Columns + types
            cur.execute("""
                SELECT column_name, data_type
                FROM information_schema.columns
                WHERE table_name = %s AND table_schema = 'public'
                ORDER BY ordinal_position
            """, (tbl,))
            info.columns = [(r[0], r[1]) for r in cur.fetchall()]

            # PK column (first column of primary key)
            cur.execute("""
                SELECT a.attname
                FROM pg_index i
                JOIN pg_attribute a ON a.attrelid = i.indrelid
                    AND a.attnum = ANY(i.indkey)
                WHERE i.indrelid = %s::regclass AND i.indisprimary
                ORDER BY array_position(i.indkey, a.attnum)
                LIMIT 1
            """, (tbl,))
            row = cur.fetchone()
            if row:
                info.pk_col = row[0]

            # FK references
            cur.execute("""
                SELECT
                    kcu.column_name,
                    ccu.table_name AS referenced_table
                FROM information_schema.table_constraints tc
                JOIN information_schema.key_column_usage kcu
                    ON tc.constraint_name = kcu.constraint_name
                JOIN information_schema.constraint_column_usage ccu
                    ON tc.constraint_name = ccu.constraint_name
                WHERE tc.constraint_type = 'FOREIGN KEY'
                    AND tc.table_name = %s
                    AND tc.table_schema = 'public'
            """, (tbl,))
            info.fk_refs = [(r[0], r[1]) for r in cur.fetchall()]

            # Max ID + row count
            # Get max ID and row count — handle UUID PKs gracefully
            pk_type = next((ct for cn, ct in info.columns if cn == info.pk_col), "int4")
            if pk_type == "uuid":
                cur.execute(f'SELECT COUNT(*) FROM "{tbl}"')
                info.row_count = cur.fetchone()[0] or 0
                info.max_id = 0  # UUID doesn't have a numeric max
                info.is_uuid_pk = True
            else:
                cur.execute(f'SELECT COALESCE(MAX({info.pk_col}), 0), COUNT(*) FROM "{tbl}"')
                row = cur.fetchone()
                info.max_id = row[0] or 0
                info.row_count = row[1] or 0
                info.is_uuid_pk = False

            # Sample values for non-PK, non-FK text/numeric columns
            for col_name, col_type in info.columns:
                if col_name == info.pk_col:
                    continue
                fk_cols = {fk[0] for fk in info.fk_refs}
                if col_name in fk_cols:
                    continue
                try:
                    cur.execute(
                        f'SELECT DISTINCT "{col_name}" FROM "{tbl}" '
                        f'WHERE "{col_name}" IS NOT NULL LIMIT 20'
                    )
                    vals = [r[0] for r in cur.fetchall()]
                    if vals:
                        info.sample_values[col_name] = vals
                except Exception:
                    pass

            result[tbl] = info
        cur.close()
        return result

    def close(self):
        self.conn.close()


# ---------------------------------------------------------------------------
# Fingerprinter — extracts WorkloadFingerprint from a .wkl via pg-retest
# ---------------------------------------------------------------------------

# Regex patterns for table extraction from SQL
_TABLE_RE = re.compile(
    r'\b(?:FROM|JOIN|INTO|UPDATE|TABLE)\s+([a-zA-Z_][a-zA-Z0-9_]*)',
    re.IGNORECASE,
)


def _extract_tables(sql: str) -> List[str]:
    """Extract table names from a SQL statement."""
    return [m.lower() for m in _TABLE_RE.findall(sql)]


def _percentile(sorted_list: List[int], pct: float) -> int:
    if not sorted_list:
        return 0
    idx = int(len(sorted_list) * pct)
    idx = min(idx, len(sorted_list) - 1)
    return sorted_list[idx]


def fingerprint_workload(wkl_path: str) -> Tuple[WorkloadFingerprint, dict]:
    """Run pg-retest inspect and extract a WorkloadFingerprint.

    Returns (fingerprint, raw_profile_dict).
    """
    try:
        result = subprocess.run(
            ["pg-retest", "inspect", wkl_path, "--output-format", "json"],
            capture_output=True, text=True,
        )
    except FileNotFoundError:
        result = None

    if result is None or result.returncode != 0:
        # Try with cargo run
        result = subprocess.run(
            ["cargo", "run", "--", "inspect", wkl_path, "--output-format", "json"],
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            sys.exit(f"ERROR: pg-retest inspect failed:\n{result.stderr}")

    # Filter out any non-JSON lines (compile messages on stderr go to stderr,
    # but cargo can mix in progress on stdout too)
    stdout = result.stdout.strip()
    # Find the JSON object start
    json_start = stdout.find("{")
    if json_start < 0:
        sys.exit("ERROR: No JSON output from pg-retest inspect")
    profile = json.loads(stdout[json_start:])

    fp = WorkloadFingerprint()
    fp.source_host = profile.get("source_host", "")
    fp.capture_method = profile.get("capture_method", "")
    fp.pg_version = profile.get("pg_version", "unknown")

    meta = profile.get("metadata", {})
    fp.total_sessions = meta.get("total_sessions", len(profile["sessions"]))
    fp.total_queries = meta.get("total_queries", 0)
    fp.capture_duration_us = meta.get("capture_duration_us", 0)

    # Count query kinds and per-table stats
    kind_counts = defaultdict(int)
    table_counts = defaultdict(int)
    table_op = defaultdict(lambda: defaultdict(int))
    think_times = []
    durations = []
    txn_queries = 0
    total_dml_select = 0
    txn_sizes = []
    cur_txn_size = 0

    for sess in profile["sessions"]:
        sess_kind_dist = defaultdict(int)
        prev_end = 0
        q_count = 0
        for q in sess["queries"]:
            kind = q["kind"]
            sess_kind_dist[kind] += 1

            if kind not in ("Begin", "Commit", "Rollback"):
                kind_counts[kind] += 1
                total_dml_select += 1
                q_count += 1

                tables = _extract_tables(q["sql"])
                for t in tables:
                    table_counts[t] += 1
                    table_op[t][kind] += 1

                if q.get("transaction_id") is not None:
                    txn_queries += 1
                    cur_txn_size += 1

            if kind == "Begin":
                cur_txn_size = 0
            elif kind == "Commit":
                if cur_txn_size > 0:
                    txn_sizes.append(cur_txn_size)
                cur_txn_size = 0

            # Think time = gap between end of previous query and start of this one
            gap = q["start_offset_us"] - prev_end
            if prev_end > 0 and gap > 0:
                think_times.append(gap)
            durations.append(q["duration_us"])
            prev_end = q["start_offset_us"] + q["duration_us"]

        fp.queries_per_session.append(q_count)
        fp.session_kind_distributions.append(dict(sess_kind_dist))

    # Query mix percentages
    if total_dml_select > 0:
        fp.select_pct = kind_counts.get("Select", 0) / total_dml_select
        fp.insert_pct = kind_counts.get("Insert", 0) / total_dml_select
        fp.update_pct = kind_counts.get("Update", 0) / total_dml_select
        fp.delete_pct = kind_counts.get("Delete", 0) / total_dml_select

    fp.table_query_counts = dict(table_counts)
    fp.table_op_counts = {t: dict(ops) for t, ops in table_op.items()}

    # Session structure
    if fp.queries_per_session:
        fp.avg_queries_per_session = sum(fp.queries_per_session) / len(fp.queries_per_session)
        fp.min_queries_per_session = min(fp.queries_per_session)
        fp.max_queries_per_session = max(fp.queries_per_session)

    # Timing
    fp.think_times_us = sorted(think_times)
    if think_times:
        fp.avg_think_time_us = sum(think_times) // len(think_times)
        fp.p50_think_time_us = _percentile(fp.think_times_us, 0.50)
        fp.p95_think_time_us = _percentile(fp.think_times_us, 0.95)
    fp.query_durations_us = sorted(durations)
    if durations:
        fp.avg_query_duration_us = sum(durations) // len(durations)

    # Transaction patterns
    if total_dml_select > 0:
        fp.pct_in_transaction = txn_queries / total_dml_select
    if txn_sizes:
        fp.avg_txn_size = sum(txn_sizes) / len(txn_sizes)

    # Extract query templates — normalized SQL patterns preserving JOINs, ORDER BY, etc.
    template_counter = defaultdict(lambda: {"count": 0, "kind": "", "tables": [], "sample": ""})
    for sess in profile["sessions"]:
        for q in sess["queries"]:
            kind = q["kind"]
            if kind in ("Begin", "Commit", "Rollback"):
                continue
            sql = q["sql"]
            # Normalize: replace numeric literals with {NUM}
            tmpl = re.sub(r"(?<![a-zA-Z_])\d+(?:\.\d+)?(?![a-zA-Z_])", "{NUM}", sql)
            # Normalize: replace string literals with {STR}
            tmpl = re.sub(r"'[^']*'", "{STR}", tmpl)
            key = tmpl
            entry = template_counter[key]
            entry["count"] += 1
            entry["kind"] = kind
            entry["tables"] = _extract_tables(sql)
            if not entry["sample"]:
                entry["sample"] = sql  # keep one real example

    # Sort by frequency and store
    fp.query_templates = sorted(
        [
            {
                "template": k,
                "count": v["count"],
                "kind": v["kind"],
                "tables": v["tables"],
                "sample": v["sample"],
                "weight": v["count"] / total_dml_select if total_dml_select > 0 else 0,
            }
            for k, v in template_counter.items()
        ],
        key=lambda x: -x["count"],
    )

    return fp, profile


# ---------------------------------------------------------------------------
# WorkloadSynthesizer — generates a new workload matching the fingerprint
# ---------------------------------------------------------------------------

class WorkloadSynthesizer:
    """Generates a synthetic workload matching a captured fingerprint."""

    # Column generators by data type pattern
    _CATEGORIES = [
        "Electronics", "Clothing", "Books", "Home", "Sports",
        "Toys", "Food", "Beauty", "Garden", "Auto",
    ]
    _STATUSES = ["pending", "shipped", "delivered", "cancelled"]
    _REVIEW_BODIES = [
        "Great product, highly recommend!",
        "Good value for the price.",
        "Average quality, nothing special.",
        "Below expectations, would not buy again.",
        "Excellent! Exceeded all expectations.",
        "Fast shipping, arrived on time.",
        "Not what I expected, poor value.",
        "Perfect, exactly as described.",
        "Works well, good build quality.",
        "Amazing quality, fits perfectly.",
    ]

    def __init__(
        self,
        fingerprint: WorkloadFingerprint,
        schema: Dict[str, TableInfo],
        rng: random.Random,
        id_start: int = 100000,
        sessions_override: Optional[int] = None,
        think_time_range_ms: Optional[Tuple[int, int]] = None,
        scale_data: float = 1.0,
    ):
        self.fp = fingerprint
        self.schema = schema
        self.rng = rng
        self.id_start = id_start
        self.scale_data = scale_data

        self.num_sessions = sessions_override or fingerprint.total_sessions
        self.think_time_override = think_time_range_ms  # (min_ms, max_ms) or None

        # ID counters per table — start at id_start
        self._id_counters: Dict[str, int] = {t: id_start for t in schema}
        # Track all allocated IDs per table (for cross-reference)
        self._allocated_ids: Dict[str, List[int]] = {t: [] for t in schema}
        # Base dataset IDs (rows that exist before workload runs)
        self._base_ids: Dict[str, List[int]] = {}

        # Determine which tables are "known" from the fingerprint
        self._known_tables = [
            t for t in schema if t in fingerprint.table_query_counts
        ]
        if not self._known_tables:
            self._known_tables = list(schema.keys())

    def next_id(self, table: str) -> int:
        """Allocate the next synthetic ID for a table."""
        cid = self._id_counters.get(table, self.id_start)
        self._id_counters[table] = cid + 1
        self._allocated_ids.setdefault(table, []).append(cid)
        return cid

    def pick_known_id(self, table: str) -> int:
        """Pick a random existing ID for a table (base or allocated)."""
        base = self._base_ids.get(table, [])
        alloc = self._allocated_ids.get(table, [])
        pool = base + alloc
        if not pool:
            # Fallback: create one
            return self.next_id(table)
        return self.rng.choice(pool)

    def pick_fk_id(self, referenced_table: str) -> int:
        """Pick a valid FK reference to another table."""
        return self.pick_known_id(referenced_table)

    def sample_think_time(self) -> int:
        """Sample an inter-query delay in microseconds."""
        if self.think_time_override:
            lo, hi = self.think_time_override
            return self.rng.randint(lo * 1000, hi * 1000)
        if self.fp.think_times_us:
            return self.rng.choice(self.fp.think_times_us)
        return self.rng.randint(100, 500)

    def sample_duration(self) -> int:
        """Sample a query duration in microseconds."""
        if self.fp.query_durations_us:
            return self.rng.choice(self.fp.query_durations_us)
        return self.rng.randint(200, 5000)

    def sample_queries_per_session(self) -> int:
        """Sample how many data queries a session should have."""
        if self.fp.queries_per_session:
            # Scale proportionally if sessions differ
            base = self.rng.choice(self.fp.queries_per_session)
            return max(1, int(base * self.scale_data))
        return max(1, int(self.fp.avg_queries_per_session * self.scale_data))

    def pick_table(self, op: str) -> str:
        """Pick a table weighted by its frequency for the given operation."""
        weights = []
        tables = []
        for t in self._known_tables:
            ops = self.fp.table_op_counts.get(t, {})
            w = ops.get(op.capitalize(), 0)
            if w > 0:
                tables.append(t)
                weights.append(w)
        if not tables:
            # Fall back to any known table
            return self.rng.choice(self._known_tables)
        return self.rng.choices(tables, weights=weights, k=1)[0]

    def pick_op(self) -> str:
        """Pick an operation type based on fingerprint distribution."""
        ops = ["select", "insert", "update", "delete"]
        weights = [self.fp.select_pct, self.fp.insert_pct,
                   self.fp.update_pct, self.fp.delete_pct]
        # Ensure at least some weight
        if sum(weights) < 0.01:
            weights = [0.5, 0.3, 0.15, 0.05]
        return self.rng.choices(ops, weights=weights, k=1)[0]

    # --- SQL generation per table ---

    def _col_value(self, table: str, col_name: str, col_type: str, row_idx: int) -> str:
        """Generate a SQL literal for a column value."""
        info = self.schema.get(table)

        # Use sample values if available
        if info and col_name in info.sample_values:
            val = self.rng.choice(info.sample_values[col_name])
            if isinstance(val, str):
                return "'" + val.replace("'", "''") + "'"
            if isinstance(val, (int, float)):
                return str(val)
            if hasattr(val, 'isoformat'):
                return "'" + val.isoformat() + "'"
            return "'" + str(val).replace("'", "''") + "'"

        # Generate by type heuristics
        col_lower = col_name.lower()
        type_lower = col_type.lower()

        if col_lower == "name" and table == "customers":
            return f"'Synth_{row_idx}'"
        if col_lower == "email":
            return f"'synth_{row_idx}@test.com'"
        if col_lower == "name" and table == "products":
            return f"'SynthProduct_{row_idx}'"
        if col_lower == "category":
            return f"'{self.rng.choice(self._CATEGORIES)}'"
        if col_lower == "status":
            return f"'{self.rng.choice(self._STATUSES)}'"
        if col_lower == "rating":
            return str(self.rng.randint(1, 5))
        if col_lower == "body":
            return "'" + self.rng.choice(self._REVIEW_BODIES).replace("'", "''") + "'"
        if col_lower in ("qty", "stock"):
            return str(self.rng.randint(1, 10))
        if col_lower in ("price", "total"):
            return f"{self.rng.uniform(5.0, 250.0):.2f}"

        if "timestamp" in type_lower or col_lower.endswith("_at"):
            return "now()"
        if "numeric" in type_lower or "integer" in type_lower or "int" in type_lower:
            return str(self.rng.randint(1, 1000))
        if "text" in type_lower or "character" in type_lower or "varchar" in type_lower:
            return f"'synth_{col_name}_{row_idx}'"
        if "boolean" in type_lower:
            return self.rng.choice(["true", "false"])

        return f"'synth_{row_idx}'"

    def generate_from_template(self, template: dict, session_id: int) -> str:
        """Generate a query by filling in a captured template with realistic values.

        Uses the actual SQL structure (JOINs, ORDER BY, GROUP BY, etc.) from
        the captured workload, replacing only the placeholder values.
        """
        tmpl = template["template"]
        tables = template["tables"]
        kind = template["kind"]

        # For each {NUM} placeholder, substitute a realistic value
        def replace_num(match):
            # Try to determine context: is this an ID reference or a general number?
            # Look at what's before the placeholder
            start = match.start()
            prefix = tmpl[:start].lower()

            # If it follows a FK column pattern (customer_id =, order_id =, etc.)
            for tbl in tables:
                info = self.schema.get(tbl)
                if info:
                    for fk_col, fk_table in info.fk_refs:
                        if prefix.rstrip().endswith(fk_col + " =") or prefix.rstrip().endswith(fk_col + " in"):
                            return str(self.pick_fk_id(fk_table))
                    # If it follows the PK column
                    if prefix.rstrip().endswith(info.pk_col + " ="):
                        return str(self.pick_known_id(tbl))

            # If it's after VALUES ( — this is an INSERT value
            if "values" in prefix[-30:].lower() and kind == "Insert":
                # First numeric in VALUES is often the PK or FK
                # Count how many {NUM} we've already passed in this VALUES clause
                vals_pos = prefix.rfind("values")
                if vals_pos >= 0:
                    in_vals = prefix[vals_pos:]
                    nums_before = in_vals.count("{NUM}") + in_vals.count("{STR}")
                    # Try to match to column position
                    main_table = tables[0] if tables else None
                    if main_table:
                        info = self.schema.get(main_table)
                        if info and nums_before < len(info.columns):
                            col_name = info.columns[nums_before][0]
                            if col_name == info.pk_col:
                                return str(self.next_id(main_table))
                            for fk_col, fk_table in info.fk_refs:
                                if fk_col == col_name:
                                    return str(self.pick_fk_id(fk_table))

            # Default: random number in reasonable range
            return str(self.rng.randint(1, 10000))

        def replace_str(match):
            # Generate a realistic string value
            prefix = tmpl[:match.start()].lower()

            # Common patterns
            if "email" in prefix[-20:]:
                return f"'synth_{self.rng.randint(1,99999)}@test.com'"
            if "name" in prefix[-20:]:
                names = ["Alice", "Bob", "Carol", "Dave", "Eve", "Frank", "Grace", "Hank"]
                return f"'Synth_{self.rng.choice(names)}_{self.rng.randint(1,9999)}'"
            if "status" in prefix[-20:]:
                return f"'{self.rng.choice(['pending', 'shipped', 'delivered', 'cancelled'])}'"
            if "event_type" in prefix[-30:]:
                return f"'{self.rng.choice(['created', 'paid', 'shipped', 'delivered', 'returned'])}'"
            if "channel" in prefix[-20:]:
                return f"'{self.rng.choice(['email', 'sms', 'push'])}'"
            if "table_name" in prefix[-30:]:
                return f"'{self.rng.choice(['customers', 'orders', 'products'])}'"
            if "operation" in prefix[-30:]:
                return f"'{self.rng.choice(['INSERT', 'UPDATE', 'DELETE'])}'"
            if "payload" in prefix[-20:] or "json" in prefix[-20:]:
                return f"'{{\"synth\": true, \"id\": {self.rng.randint(1,9999)}}}'"
            if "subject" in prefix[-20:]:
                return f"'Synth notification {self.rng.randint(1,9999)}'"
            if "body" in prefix[-20:]:
                return f"'Synthetic test body {self.rng.randint(1,9999)}'"

            # Default
            return f"'synth_{self.rng.randint(1,99999)}'"

        # Apply replacements
        result = re.sub(r"\{NUM\}", replace_num, tmpl)
        result = re.sub(r"\{STR\}", replace_str, result)

        return result

    def generate_insert(self, table: str, session_id: int) -> str:
        """Generate an INSERT with fixed ID — fallback when no template matches."""
        info = self.schema.get(table)
        if not info:
            row_id = self.next_id(table)
            return f"INSERT INTO {table} (id) VALUES ({row_id})"

        row_id = self.next_id(table)
        cols = []
        vals = []

        for col_name, col_type in info.columns:
            if col_name == info.pk_col:
                cols.append(col_name)
                vals.append(str(row_id))
                continue

            # FK reference
            fk_target = None
            for fk_col, fk_table in info.fk_refs:
                if fk_col == col_name:
                    fk_target = fk_table
                    break
            if fk_target:
                cols.append(col_name)
                vals.append(str(self.pick_fk_id(fk_target)))
                continue

            cols.append(col_name)
            vals.append(self._col_value(table, col_name, col_type, row_id))

        return f"INSERT INTO {table} ({', '.join(cols)}) VALUES ({', '.join(vals)})"

    def generate_select(self, table: str) -> str:
        """Generate a SELECT — fallback when no template matches."""
        info = self.schema.get(table)
        pk = info.pk_col if info else "id"
        known_id = self.pick_known_id(table)
        return f"SELECT * FROM {table} WHERE {pk} = {known_id} LIMIT 10"

    def generate_update(self, table: str) -> str:
        """Generate an UPDATE — fallback when no template matches."""
        info = self.schema.get(table)
        pk = info.pk_col if info else "id"
        known_id = self.pick_known_id(table)

        # Pick a non-PK, non-FK column to update
        if info:
            updatable = []
            fk_cols = {fk[0] for fk in info.fk_refs}
            for col_name, col_type in info.columns:
                if col_name == info.pk_col:
                    continue
                if col_name in fk_cols:
                    continue
                updatable.append((col_name, col_type))
            if updatable:
                col_name, col_type = self.rng.choice(updatable)
                val = self._col_value(table, col_name, col_type, known_id)
                return f"UPDATE {table} SET {col_name} = {val} WHERE {pk} = {known_id}"

        return f"UPDATE {table} SET updated_at = now() WHERE {pk} = {known_id}"

    def generate_delete(self, table: str) -> str:
        """Generate a DELETE on a known row."""
        info = self.schema.get(table)
        pk = info.pk_col if info else "id"
        known_id = self.pick_known_id(table)
        return f"DELETE FROM {table} WHERE {pk} = {known_id}"

    def _kind_str(self, op: str) -> str:
        return {"select": "Select", "insert": "Insert",
                "update": "Update", "delete": "Delete"}[op]

    # --- Session generation ---

    def generate_session(self, session_id: int) -> dict:
        """Generate a complete session dict for the workload profile."""
        queries = []
        offset_us = 0
        in_txn = False
        txn_counter = session_id * 10000
        txn_id = None

        num_queries = self.sample_queries_per_session()

        for i in range(num_queries):
            op = self.pick_op()

            # Maybe start a transaction (match captured transaction patterns)
            if (not in_txn
                    and self.fp.pct_in_transaction > 0
                    and self.rng.random() < self.fp.pct_in_transaction * 0.4):
                queries.append(self._make_query(
                    "BEGIN", offset_us, self.rng.randint(50, 200), "Begin", None))
                offset_us += self.sample_think_time()
                in_txn = True
                txn_counter += 1
                txn_id = txn_counter

            # Generate the actual query — prefer templates from captured workload
            kind_str = self._kind_str(op)
            matching_templates = [
                t for t in self.fp.query_templates
                if t["kind"] == kind_str.capitalize() or t["kind"] == kind_str
            ]

            if matching_templates:
                # Weighted random selection from matching templates
                weights = [t["weight"] for t in matching_templates]
                total_w = sum(weights) or 1
                weights = [w / total_w for w in weights]
                template = self.rng.choices(matching_templates, weights=weights, k=1)[0]
                sql = self.generate_from_template(template, session_id)
            else:
                # Fallback to simple generation
                table = self.pick_table(op)
                if op == "insert":
                    sql = self.generate_insert(table, session_id)
                elif op == "select":
                    sql = self.generate_select(table)
                elif op == "update":
                    sql = self.generate_update(table)
                else:
                    sql = self.generate_delete(table)

            dur = self.sample_duration()
            queries.append(self._make_query(sql, offset_us, dur,
                                            self._kind_str(op), txn_id))
            offset_us += dur + self.sample_think_time()

            # Maybe commit (probability based on avg txn size)
            if in_txn and self.rng.random() < (1.0 / max(1, self.fp.avg_txn_size)):
                queries.append(self._make_query(
                    "COMMIT", offset_us, self.rng.randint(50, 300), "Commit", None))
                offset_us += self.sample_think_time()
                in_txn = False
                txn_id = None

        # Close any open transaction
        if in_txn:
            queries.append(self._make_query(
                "COMMIT", offset_us, self.rng.randint(50, 300), "Commit", None))

        return {
            "id": session_id,
            "user": "demo",
            "database": "ecommerce",
            "queries": queries,
        }

    @staticmethod
    def _make_query(sql: str, offset_us: int, duration_us: int,
                    kind: str, txn_id: Optional[int]) -> dict:
        return {
            "sql": sql,
            "start_offset_us": offset_us,
            "duration_us": duration_us,
            "kind": kind,
            "transaction_id": txn_id,
            "response_values": None,
        }

    # --- Base data generation ---

    def generate_base_data(self) -> Dict[str, List[int]]:
        """Pre-generate base dataset IDs for tables that need existing rows.

        Returns dict of table -> list of base IDs.
        Tables with SELECT/UPDATE/DELETE queries need base rows so those
        queries find something.
        """
        base = {}

        for table in self._known_tables:
            info = self.schema.get(table)
            if not info:
                continue

            ops = self.fp.table_op_counts.get(table, {})
            needs_existing = (
                ops.get("Select", 0) + ops.get("Update", 0) + ops.get("Delete", 0)
            )
            if needs_existing == 0:
                continue

            # Scale to roughly match the source row count
            count = max(100, int(info.row_count * self.scale_data * 0.1))
            count = min(count, 5000)  # cap for sanity

            ids = list(range(1, count + 1))
            base[table] = ids

        self._base_ids = base
        return base

    def generate_data_sql(self, base_ids: Dict[str, List[int]]) -> str:
        """Generate the SQL file that creates the base dataset.

        This includes:
        - Schema DDL (CREATE TABLE, indexes)
        - Base rows in the 1..N range
        - Sequence reset to id_start so workload INSERTs don't collide
        """
        lines = []
        lines.append("-- Synthetic base data generated by synthesize-workload.py")
        lines.append(f"-- Generated: {datetime.now(timezone.utc).isoformat()}")
        lines.append(f"-- ID start offset: {self.id_start}")
        lines.append("")

        # Schema DDL
        lines.append("-- === Schema ===")
        lines.append(self._generate_schema_ddl())
        lines.append("")

        # Determine insert order respecting FK dependencies
        ordered_tables = self._fk_sorted_tables()

        # Base data inserts
        lines.append("-- === Base Data ===")
        for table in ordered_tables:
            ids = base_ids.get(table)
            if not ids:
                continue
            info = self.schema.get(table)
            if not info:
                continue

            lines.append(f"\n-- {table}: {len(ids)} rows")
            for row_id in ids:
                cols = []
                vals = []
                for col_name, col_type in info.columns:
                    if col_name == info.pk_col:
                        cols.append(col_name)
                        vals.append(str(row_id))
                        continue
                    fk_target = None
                    for fk_col, fk_table in info.fk_refs:
                        if fk_col == col_name:
                            fk_target = fk_table
                            break
                    if fk_target:
                        parent_ids = base_ids.get(fk_target, [1])
                        cols.append(col_name)
                        vals.append(str(self.rng.choice(parent_ids)))
                        continue
                    cols.append(col_name)
                    vals.append(self._col_value(table, col_name, col_type, row_id))
                lines.append(
                    f"INSERT INTO {table} ({', '.join(cols)}) VALUES ({', '.join(vals)});"
                )

        # Reset sequences so workload INSERTs start at id_start
        lines.append("\n-- === Sequence Reset ===")
        for table in self.schema:
            info = self.schema[table]
            seq_name = f"{table}_{info.pk_col}_seq"
            lines.append(
                f"SELECT setval('{seq_name}', {self.id_start}, false);"
            )

        lines.append("\n-- Analyze for query planner")
        lines.append("ANALYZE;")
        lines.append("")

        return "\n".join(lines)

    def _generate_schema_ddl(self) -> str:
        """Generate CREATE TABLE statements from schema info."""
        lines = []
        ordered = self._fk_sorted_tables()

        for table in ordered:
            info = self.schema.get(table)
            if not info:
                continue

            col_defs = []
            for col_name, col_type in info.columns:
                pg_type = self._map_col_type(col_name, col_type, info)
                if col_name == info.pk_col:
                    col_defs.append(f"    {col_name} SERIAL PRIMARY KEY")
                else:
                    col_defs.append(f"    {col_name} {pg_type}")

            # Add FK constraints
            for fk_col, fk_table in info.fk_refs:
                fk_info = self.schema.get(fk_table)
                fk_pk = fk_info.pk_col if fk_info else "id"
                col_defs.append(
                    f"    FOREIGN KEY ({fk_col}) REFERENCES {fk_table}({fk_pk})"
                )

            lines.append(f"CREATE TABLE IF NOT EXISTS {table} (")
            lines.append(",\n".join(col_defs))
            lines.append(");")
            lines.append("")

        return "\n".join(lines)

    @staticmethod
    def _map_col_type(col_name: str, info_type: str, table_info: TableInfo) -> str:
        """Map information_schema data_type to a CREATE TABLE type."""
        t = info_type.lower()
        col = col_name.lower()

        if "integer" in t or "int" in t:
            # Check if it's an FK
            for fk_col, _ in table_info.fk_refs:
                if fk_col == col_name:
                    return "INTEGER NOT NULL"
            return "INTEGER NOT NULL DEFAULT 0"
        if "numeric" in t:
            return "NUMERIC(10,2) NOT NULL DEFAULT 0"
        if "text" in t or "character varying" in t:
            if col == "email":
                return "TEXT NOT NULL UNIQUE"
            return "TEXT NOT NULL"
        if "timestamp" in t:
            return "TIMESTAMPTZ NOT NULL DEFAULT now()"
        if "boolean" in t:
            return "BOOLEAN NOT NULL DEFAULT false"

        return info_type.upper() + " NOT NULL"

    def _fk_sorted_tables(self) -> List[str]:
        """Topologically sort tables by FK dependencies."""
        visited = set()
        order = []

        def visit(table):
            if table in visited:
                return
            visited.add(table)
            info = self.schema.get(table)
            if info:
                for _, fk_table in info.fk_refs:
                    if fk_table in self.schema:
                        visit(fk_table)
            order.append(table)

        for t in self.schema:
            visit(t)
        return order

    # --- Full synthesis ---

    def synthesize(self) -> Tuple[dict, str]:
        """Run complete synthesis.  Returns (profile_dict, data_sql_str)."""
        # Step 1: Generate base data
        print(f"  Generating base dataset for {len(self._known_tables)} tables...")
        base_ids = self.generate_base_data()
        for t, ids in base_ids.items():
            print(f"    {t}: {len(ids)} base rows (IDs 1..{len(ids)})")

        # Step 2: Generate sessions
        print(f"  Generating {self.num_sessions} sessions...")
        sessions = []
        total_q = 0
        max_offset = 0
        for sid in range(1, self.num_sessions + 1):
            sess = self.generate_session(sid)
            sessions.append(sess)
            total_q += len(sess["queries"])
            if sess["queries"]:
                last = sess["queries"][-1]
                end = last["start_offset_us"] + last["duration_us"]
                max_offset = max(max_offset, end)

        # Step 3: Build profile
        profile = {
            "version": 2,
            "captured_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ"),
            "source_host": "synthetic",
            "pg_version": self.fp.pg_version or "16",
            "capture_method": "synthetic",
            "sessions": sessions,
            "metadata": {
                "total_queries": total_q,
                "total_sessions": self.num_sessions,
                "capture_duration_us": max_offset,
                "sequence_snapshot": None,
                "pk_map": None,
            },
        }

        # Step 4: Generate data SQL
        print("  Generating data SQL...")
        data_sql = self.generate_data_sql(base_ids)

        # Summary
        id_ranges = {}
        for t in self._known_tables:
            lo = self.id_start
            hi = self._id_counters.get(t, self.id_start) - 1
            if hi >= lo:
                id_ranges[t] = (lo, hi)

        print(f"\n  Synthesis complete:")
        print(f"    Sessions:        {self.num_sessions}")
        print(f"    Total queries:   {total_q}")
        print(f"    Duration:        {max_offset / 1_000_000:.1f}s")
        print(f"    ID ranges:")
        for t, (lo, hi) in sorted(id_ranges.items()):
            print(f"      {t}: {lo}..{hi} ({hi - lo + 1} new rows)")

        return profile, data_sql


# ---------------------------------------------------------------------------
# MessagePack Writer — serialize profile to .wkl
# ---------------------------------------------------------------------------

def write_wkl(profile: dict, path: str):
    """Write a WorkloadProfile dict as MessagePack (.wkl file).

    Uses the same schema as rmp_serde in pg-retest:
    - Top-level: map with string keys
    - sessions: array of maps
    - queries: array of maps
    - None values serialized as msgpack nil
    """
    packed = msgpack.packb(profile, use_bin_type=True)
    with open(path, "wb") as f:
        f.write(packed)
    print(f"  Wrote {len(packed)} bytes to {path}")


# ---------------------------------------------------------------------------
# Fingerprint display
# ---------------------------------------------------------------------------

def print_fingerprint(fp: WorkloadFingerprint):
    """Print a human-readable summary of the fingerprint."""
    print("\n=== Workload Fingerprint ===")
    print(f"  Source:            {fp.source_host} ({fp.capture_method})")
    print(f"  Sessions:          {fp.total_sessions}")
    print(f"  Total queries:     {fp.total_queries}")
    print(f"  Duration:          {fp.capture_duration_us / 1_000_000:.1f}s")
    print(f"\n  Query Mix:")
    print(f"    SELECT: {fp.select_pct:.1%}")
    print(f"    INSERT: {fp.insert_pct:.1%}")
    print(f"    UPDATE: {fp.update_pct:.1%}")
    print(f"    DELETE: {fp.delete_pct:.1%}")
    print(f"\n  Tables:")
    for t, cnt in sorted(fp.table_query_counts.items(), key=lambda x: -x[1]):
        ops = fp.table_op_counts.get(t, {})
        ops_str = ", ".join(f"{k}={v}" for k, v in sorted(ops.items()))
        print(f"    {t}: {cnt} queries ({ops_str})")
    print(f"\n  Session Structure:")
    print(f"    Avg queries/session: {fp.avg_queries_per_session:.1f}")
    print(f"    Range: {fp.min_queries_per_session}..{fp.max_queries_per_session}")
    print(f"\n  Timing:")
    print(f"    Think time: avg={fp.avg_think_time_us}us "
          f"p50={fp.p50_think_time_us}us p95={fp.p95_think_time_us}us")
    print(f"    Query duration: avg={fp.avg_query_duration_us}us")
    print(f"\n  Transactions:")
    print(f"    % in transaction: {fp.pct_in_transaction:.1%}")
    print(f"    Avg txn size:     {fp.avg_txn_size:.1f} queries")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def parse_think_time(val: str) -> Tuple[int, int]:
    """Parse '5-25' into (5, 25) milliseconds."""
    parts = val.split("-")
    if len(parts) == 2:
        return int(parts[0]), int(parts[1])
    elif len(parts) == 1:
        v = int(parts[0])
        return v, v
    else:
        sys.exit(f"ERROR: Invalid think-time format: {val}  (expected: MIN-MAX)")


def main():
    parser = argparse.ArgumentParser(
        description="Synthesize a zero-error workload from a captured .wkl fingerprint",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Basic synthesis
  python3 demo/synthesize-workload.py \\
      --input demo/workload.wkl \\
      --source-db "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \\
      --output-workload synthetic.wkl \\
      --output-data synthetic-data.sql

  # Override sessions and timing
  python3 demo/synthesize-workload.py \\
      --input demo/workload.wkl \\
      --source-db "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \\
      --output-workload synthetic.wkl \\
      --output-data synthetic-data.sql \\
      --sessions 100 --think-time-ms 5-25 --scale-data 2.0

  # Fingerprint only (no synthesis)
  python3 demo/synthesize-workload.py --input demo/workload.wkl --fingerprint-only
""",
    )
    parser.add_argument("--input", required=True,
                        help="Path to captured .wkl file")
    parser.add_argument("--source-db",
                        help="Source database connection string (for schema analysis)")
    parser.add_argument("--output-workload", default="synthetic.wkl",
                        help="Output .wkl file path (default: synthetic.wkl)")
    parser.add_argument("--output-data", default="synthetic-data.sql",
                        help="Output SQL data file (default: synthetic-data.sql)")
    parser.add_argument("--seed", type=int, default=42,
                        help="Random seed for reproducibility (default: 42)")
    parser.add_argument("--sessions", type=int, default=None,
                        help="Override number of sessions (default: match captured)")
    parser.add_argument("--think-time-ms", default=None,
                        help="Override think time range in ms, e.g. '5-25'")
    parser.add_argument("--id-start", type=int, default=100000,
                        help="Synthetic IDs start at this offset (default: 100000)")
    parser.add_argument("--scale-data", type=float, default=1.0,
                        help="Scale factor for data volume (default: 1.0)")
    parser.add_argument("--fingerprint-only", action="store_true",
                        help="Only print the fingerprint, don't synthesize")

    args = parser.parse_args()

    # Step 1: Fingerprint
    print(f"Analyzing {args.input}...")
    fp, raw_profile = fingerprint_workload(args.input)
    print_fingerprint(fp)

    if args.fingerprint_only:
        return

    # Step 2: Schema analysis
    if not args.source_db:
        sys.exit("ERROR: --source-db is required for synthesis (not needed for --fingerprint-only)")

    tables = list(fp.table_query_counts.keys())
    if not tables:
        sys.exit("ERROR: No tables detected in workload")

    print(f"\nAnalyzing schema for {len(tables)} tables: {', '.join(tables)}...")
    analyzer = SchemaAnalyzer(args.source_db)
    schema = analyzer.analyze(tables)
    analyzer.close()

    for t, info in schema.items():
        fk_str = ""
        if info.fk_refs:
            fk_str = f" FK: {', '.join(f'{c}->{r}' for c, r in info.fk_refs)}"
        print(f"  {t}: {len(info.columns)} cols, {info.row_count} rows, "
              f"max_id={info.max_id}{fk_str}")

    # Step 3: Synthesize
    think_time = parse_think_time(args.think_time_ms) if args.think_time_ms else None
    rng = random.Random(args.seed)

    print(f"\nSynthesizing workload...")
    synth = WorkloadSynthesizer(
        fingerprint=fp,
        schema=schema,
        rng=rng,
        id_start=args.id_start,
        sessions_override=args.sessions,
        think_time_range_ms=think_time,
        scale_data=args.scale_data,
    )

    profile, data_sql = synth.synthesize()

    # Step 4: Write outputs
    print(f"\nWriting outputs...")
    write_wkl(profile, args.output_workload)

    with open(args.output_data, "w") as f:
        f.write(data_sql)
    print(f"  Wrote {len(data_sql)} bytes to {args.output_data}")

    # Step 5: Verify the .wkl is readable
    print(f"\nVerifying {args.output_workload}...")
    try:
        verify = subprocess.run(
            ["pg-retest", "inspect", args.output_workload, "--output-format", "json"],
            capture_output=True, text=True,
        )
    except FileNotFoundError:
        verify = None

    if verify is None or verify.returncode != 0:
        # Try cargo run
        verify = subprocess.run(
            ["cargo", "run", "--", "inspect", args.output_workload, "--output-format", "json"],
            capture_output=True, text=True,
        )

    if verify.returncode == 0:
        stdout = verify.stdout.strip()
        json_start = stdout.find("{")
        if json_start >= 0:
            vdata = json.loads(stdout[json_start:])
            vmeta = vdata.get("metadata", {})
            print(f"  Verified: {vmeta.get('total_sessions', '?')} sessions, "
                  f"{vmeta.get('total_queries', '?')} queries")
        else:
            print("  Verified: pg-retest inspect succeeded (no JSON output)")
    else:
        print(f"  WARNING: pg-retest inspect failed on output file.")
        print(f"  stderr: {verify.stderr[:200]}")
        print(f"  The .wkl file may need format adjustment.")

    print(f"\nDone! To use:")
    print(f"  1. Load data:   psql <target-connstring> < {args.output_data}")
    print(f"  2. Replay:      pg-retest replay {args.output_workload} --target <connstring>")


if __name__ == "__main__":
    main()

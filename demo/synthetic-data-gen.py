#!/usr/bin/env python3
"""
Synthetic data generator for pg-retest workload transform support.

Analyzes a .wkl workload file and the source database schema to produce
a SQL dump of synthetic data that matches the workload's query patterns.
This bridges the gap where `pg-retest transform` creates modified SQL
but doesn't create matching data for the target database.

Usage:
    python3 demo/synthetic-data-gen.py \
        --workload workload.wkl \
        --source-db "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \
        --scale 1 \
        --output synthetic-data.sql

    # Load into target
    psql "host=target ..." < synthetic-data.sql

    # Replay the transformed workload
    pg-retest replay transformed.wkl --target ...

Requirements:
    pip install psycopg2-binary
    pg-retest binary on PATH (for workload inspection)
"""

import argparse
import json
import os
import random
import re
import string
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone


# ---------------------------------------------------------------------------
# Schema Analyzer — connects to source DB and extracts schema + stats
# ---------------------------------------------------------------------------

class SchemaAnalyzer:
    """Connects to the source database and extracts schema metadata and
    basic column statistics for every referenced table."""

    def __init__(self, dsn):
        import psycopg2
        self.conn = psycopg2.connect(dsn)
        self.conn.autocommit = True

    def close(self):
        self.conn.close()

    def get_tables(self):
        """Return list of user table names in the public schema."""
        cur = self.conn.cursor()
        cur.execute("""
            SELECT table_name FROM information_schema.tables
            WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
            ORDER BY table_name
        """)
        return [r[0] for r in cur.fetchall()]

    def get_create_table(self, table):
        """Reconstruct CREATE TABLE DDL from information_schema."""
        cur = self.conn.cursor()

        # Columns
        cur.execute("""
            SELECT column_name, data_type, character_maximum_length,
                   numeric_precision, numeric_scale, column_default,
                   is_nullable, udt_name
            FROM information_schema.columns
            WHERE table_schema = 'public' AND table_name = %s
            ORDER BY ordinal_position
        """, (table,))
        columns = cur.fetchall()

        # Primary key columns
        cur.execute("""
            SELECT kcu.column_name
            FROM information_schema.table_constraints tc
            JOIN information_schema.key_column_usage kcu
              ON tc.constraint_name = kcu.constraint_name
             AND tc.table_schema = kcu.table_schema
            WHERE tc.table_schema = 'public' AND tc.table_name = %s
              AND tc.constraint_type = 'PRIMARY KEY'
            ORDER BY kcu.ordinal_position
        """, (table,))
        pk_cols = [r[0] for r in cur.fetchall()]

        lines = []
        for col_name, data_type, char_len, num_prec, num_scale, default, nullable, udt in columns:
            col_type = self._pg_type_str(data_type, char_len, num_prec, num_scale, udt)
            parts = [f"    {col_name} {col_type}"]
            if default and ("nextval" in str(default)):
                # Replace sequence defaults with SERIAL-style for portability
                if "bigint" in col_type.lower():
                    parts = [f"    {col_name} BIGSERIAL"]
                else:
                    parts = [f"    {col_name} SERIAL"]
            elif default:
                parts.append(f"DEFAULT {default}")
            if nullable == "NO":
                parts.append("NOT NULL")
            lines.append(" ".join(parts))

        if pk_cols:
            lines.append(f"    PRIMARY KEY ({', '.join(pk_cols)})")

        ddl = f"CREATE TABLE {table} (\n" + ",\n".join(lines) + "\n);"
        return ddl

    def _pg_type_str(self, data_type, char_len, num_prec, num_scale, udt):
        if udt in ("int4", "int8", "int2", "float4", "float8", "bool",
                    "text", "uuid", "timestamptz", "timestamp", "date",
                    "jsonb", "json", "bytea"):
            mapping = {
                "int4": "INTEGER", "int8": "BIGINT", "int2": "SMALLINT",
                "float4": "REAL", "float8": "DOUBLE PRECISION",
                "bool": "BOOLEAN", "text": "TEXT", "uuid": "UUID",
                "timestamptz": "TIMESTAMPTZ", "timestamp": "TIMESTAMP",
                "date": "DATE", "jsonb": "JSONB", "json": "JSON",
                "bytea": "BYTEA",
            }
            return mapping.get(udt, data_type.upper())
        if data_type == "character varying":
            return f"VARCHAR({char_len})" if char_len else "VARCHAR"
        if data_type == "numeric" and num_prec:
            return f"NUMERIC({num_prec},{num_scale or 0})"
        return data_type.upper()

    def get_foreign_keys(self, table):
        """Return list of (column, ref_table, ref_column) for FKs."""
        cur = self.conn.cursor()
        cur.execute("""
            SELECT kcu.column_name,
                   ccu.table_name AS ref_table,
                   ccu.column_name AS ref_column
            FROM information_schema.table_constraints tc
            JOIN information_schema.key_column_usage kcu
              ON tc.constraint_name = kcu.constraint_name
            JOIN information_schema.constraint_column_usage ccu
              ON tc.constraint_name = ccu.constraint_name
            WHERE tc.table_schema = 'public' AND tc.table_name = %s
              AND tc.constraint_type = 'FOREIGN KEY'
        """, (table,))
        return [(r[0], r[1], r[2]) for r in cur.fetchall()]

    def get_check_constraints(self, table):
        """Return list of (column_name, check_clause) pairs."""
        cur = self.conn.cursor()
        cur.execute("""
            SELECT cc.check_clause
            FROM information_schema.table_constraints tc
            JOIN information_schema.check_constraints cc
              ON tc.constraint_name = cc.constraint_name
            WHERE tc.table_schema = 'public' AND tc.table_name = %s
              AND tc.constraint_type = 'CHECK'
        """, (table,))
        return [r[0] for r in cur.fetchall()]

    def get_unique_columns(self, table):
        """Return set of column names that have UNIQUE constraints."""
        cur = self.conn.cursor()
        cur.execute("""
            SELECT kcu.column_name
            FROM information_schema.table_constraints tc
            JOIN information_schema.key_column_usage kcu
              ON tc.constraint_name = kcu.constraint_name
            WHERE tc.table_schema = 'public' AND tc.table_name = %s
              AND tc.constraint_type = 'UNIQUE'
        """, (table,))
        return {r[0] for r in cur.fetchall()}

    def get_column_stats(self, table):
        """Return per-column basic statistics: row count, min/max for
        numeric/timestamp columns, and value distribution for low-cardinality
        text columns."""
        cur = self.conn.cursor()
        cur.execute(f"SELECT count(*) FROM {table}")
        row_count = cur.fetchone()[0]

        cur.execute("""
            SELECT column_name, data_type, udt_name
            FROM information_schema.columns
            WHERE table_schema = 'public' AND table_name = %s
            ORDER BY ordinal_position
        """, (table,))
        columns = cur.fetchall()

        stats = {"row_count": row_count, "columns": {}}

        for col_name, data_type, udt in columns:
            col_stats = {"type": udt}

            if udt in ("int4", "int8", "int2", "float4", "float8", "numeric"):
                cur.execute(f"SELECT min({col_name}), max({col_name}) FROM {table}")
                mn, mx = cur.fetchone()
                col_stats["min"] = mn
                col_stats["max"] = mx

            elif udt in ("timestamptz", "timestamp", "date"):
                cur.execute(f"SELECT min({col_name}), max({col_name}) FROM {table}")
                mn, mx = cur.fetchone()
                col_stats["min"] = str(mn) if mn else None
                col_stats["max"] = str(mx) if mx else None

            elif udt in ("text", "varchar", "bpchar"):
                # Value distribution for low-cardinality columns
                cur.execute(f"""
                    SELECT {col_name}, count(*) as cnt
                    FROM {table}
                    WHERE {col_name} IS NOT NULL
                    GROUP BY {col_name}
                    ORDER BY cnt DESC
                    LIMIT 20
                """)
                dist = cur.fetchall()
                if len(dist) <= 15 and row_count > 0:
                    # Low cardinality — store full distribution
                    total = sum(d[1] for d in dist)
                    col_stats["distribution"] = {
                        str(d[0]): round(d[1] / total, 4) for d in dist
                    }
                else:
                    # High cardinality — store sample patterns
                    cur.execute(f"""
                        SELECT avg(length({col_name}))::int,
                               max(length({col_name}))
                        FROM {table} WHERE {col_name} IS NOT NULL
                    """)
                    avg_len, max_len = cur.fetchone()
                    col_stats["avg_length"] = avg_len
                    col_stats["max_length"] = max_len

            stats["columns"][col_name] = col_stats

        return stats

    def get_indexes(self, table):
        """Return CREATE INDEX statements for the table."""
        cur = self.conn.cursor()
        cur.execute("""
            SELECT indexdef FROM pg_indexes
            WHERE schemaname = 'public' AND tablename = %s
              AND indexname NOT LIKE '%%_pkey'
        """, (table,))
        return [r[0] + ";" for r in cur.fetchall()]


# ---------------------------------------------------------------------------
# Workload Analyzer — parses .wkl via pg-retest inspect
# ---------------------------------------------------------------------------

class WorkloadAnalyzer:
    """Analyzes a .wkl workload file to understand which tables and
    columns are referenced, value ranges, and join patterns."""

    def __init__(self, workload_path):
        self.workload_path = workload_path
        self.queries = self._load_queries()

    def _load_queries(self):
        """Use pg-retest inspect to load workload as JSON."""
        try:
            result = subprocess.run(
                ["pg-retest", "inspect", self.workload_path,
                 "--output-format", "json"],
                capture_output=True, text=True, check=True,
            )
            data = json.loads(result.stdout)
            queries = []
            for session in data.get("sessions", []):
                for q in session.get("queries", []):
                    sql = q.get("sql", q.get("text", ""))
                    if sql:
                        queries.append(sql)
            return queries
        except FileNotFoundError:
            print("Error: pg-retest binary not found on PATH.", file=sys.stderr)
            print("Build it with: cargo build --release", file=sys.stderr)
            sys.exit(1)
        except subprocess.CalledProcessError as e:
            print(f"Error inspecting workload: {e.stderr}", file=sys.stderr)
            sys.exit(1)

    def extract_referenced_tables(self):
        """Extract table names from FROM, JOIN, INTO, UPDATE, INSERT INTO."""
        tables = set()
        patterns = [
            r'\bFROM\s+([a-zA-Z_]\w*)',
            r'\bJOIN\s+([a-zA-Z_]\w*)',
            r'\bINTO\s+([a-zA-Z_]\w*)',
            r'\bUPDATE\s+([a-zA-Z_]\w*)',
        ]
        skip = {"select", "where", "set", "values", "and", "or", "not",
                "null", "true", "false", "as", "on", "in", "is", "by",
                "order", "group", "having", "limit", "offset", "case",
                "when", "then", "else", "end"}
        for sql in self.queries:
            for pat in patterns:
                for m in re.finditer(pat, sql, re.IGNORECASE):
                    name = m.group(1).lower()
                    if name not in skip:
                        tables.add(name)
        return tables

    def extract_value_ranges(self):
        """Extract numeric literal ranges from WHERE clauses per table."""
        ranges = defaultdict(lambda: defaultdict(lambda: {"min": float("inf"), "max": float("-inf")}))
        # Pattern: column = N or column > N etc.
        pat = re.compile(
            r'(\w+)\s*(?:=|>|<|>=|<=|!=|<>)\s*(\d+(?:\.\d+)?)',
            re.IGNORECASE,
        )
        for sql in self.queries:
            for m in pat.finditer(sql):
                col = m.group(1).lower()
                val = float(m.group(2))
                # Try to figure out which table — simplistic: first FROM
                table_m = re.search(r'\bFROM\s+(\w+)', sql, re.IGNORECASE)
                if table_m:
                    tbl = table_m.group(1).lower()
                    ranges[tbl][col]["min"] = min(ranges[tbl][col]["min"], val)
                    ranges[tbl][col]["max"] = max(ranges[tbl][col]["max"], val)
        return ranges

    def extract_limit_hints(self):
        """Extract LIMIT values to estimate required row counts."""
        limits = []
        for sql in self.queries:
            m = re.search(r'\bLIMIT\s+(\d+)', sql, re.IGNORECASE)
            if m:
                limits.append(int(m.group(1)))
        return max(limits) if limits else 100

    def query_count(self):
        return len(self.queries)


# ---------------------------------------------------------------------------
# Data Generator — produces synthetic SQL matching schema + workload
# ---------------------------------------------------------------------------

class DataGenerator:
    """Generates synthetic data as a SQL file that creates the schema and
    populates tables with fake but consistent data."""

    # Pools for realistic fake values
    FIRST_NAMES = [
        "Alice", "Bob", "Charlie", "Diana", "Eve", "Frank", "Grace",
        "Hank", "Ivy", "Jack", "Karen", "Leo", "Mia", "Noah", "Olivia",
        "Paul", "Quinn", "Ruby", "Sam", "Tina", "Uma", "Victor", "Wendy",
        "Xander", "Yara", "Zach",
    ]
    LAST_NAMES = [
        "Smith", "Johnson", "Williams", "Brown", "Jones", "Garcia",
        "Miller", "Davis", "Rodriguez", "Martinez", "Anderson", "Taylor",
        "Thomas", "Moore", "Jackson", "Martin", "Lee", "White", "Harris",
        "Clark",
    ]
    DOMAINS = ["example.com", "test.org", "demo.net", "sample.io", "fake.dev"]
    CATEGORIES = ["Electronics", "Books", "Clothing", "Home", "Sports",
                  "Toys", "Food", "Garden", "Auto", "Health"]
    STATUSES = {
        "default": ["active", "inactive", "pending"],
        "order": ["pending", "processing", "shipped", "delivered", "cancelled"],
    }

    def __init__(self, schema_analyzer, workload_analyzer, scale, seed=42):
        self.schema = schema_analyzer
        self.workload = workload_analyzer
        self.scale = scale
        self.rng = random.Random(seed)
        self.table_data = {}  # table -> list of row dicts
        self.table_ddl = {}
        self.table_indexes = {}
        self.fk_map = {}  # table -> [(col, ref_table, ref_col)]
        self.unique_cols = {}
        self.stats = {}

    def analyze(self, tables):
        """Gather schema info for all tables."""
        for table in tables:
            self.table_ddl[table] = self.schema.get_create_table(table)
            self.fk_map[table] = self.schema.get_foreign_keys(table)
            self.unique_cols[table] = self.schema.get_unique_columns(table)
            self.stats[table] = self.schema.get_column_stats(table)
            self.table_indexes[table] = self.schema.get_indexes(table)

    def topo_sort_tables(self, tables):
        """Sort tables so that FK parents come before children."""
        graph = defaultdict(set)
        for table in tables:
            for _col, ref_table, _ref_col in self.fk_map.get(table, []):
                if ref_table in tables:
                    graph[table].add(ref_table)

        visited = set()
        order = []

        def visit(t):
            if t in visited:
                return
            visited.add(t)
            for dep in graph.get(t, set()):
                visit(dep)
            order.append(t)

        for t in tables:
            visit(t)
        return order

    def compute_row_counts(self, tables):
        """Determine how many rows to generate per table, scaled."""
        counts = {}
        for table in tables:
            source_count = self.stats[table]["row_count"]
            if source_count == 0:
                source_count = max(100, self.workload.extract_limit_hints() * 2)
            counts[table] = max(10, int(source_count * self.scale))
        return counts

    def generate_all(self, tables):
        """Generate synthetic data for all tables in FK order."""
        ordered = self.topo_sort_tables(tables)
        row_counts = self.compute_row_counts(tables)

        for table in ordered:
            n = row_counts[table]
            self.table_data[table] = self._generate_table(table, n)

    def _generate_table(self, table, num_rows):
        """Generate rows for a single table."""
        stats = self.stats[table]
        fks = {fk[0]: (fk[1], fk[2]) for fk in self.fk_map.get(table, [])}
        unique = self.unique_cols.get(table, set())
        rows = []
        seen_unique = defaultdict(set)

        for i in range(1, num_rows + 1):
            row = {}
            for col_name, col_stats in stats["columns"].items():
                col_type = col_stats["type"]

                # Skip serial/identity PKs — let the DB generate them
                if col_name == "id" and col_type in ("int4", "int8"):
                    continue

                # FK reference — pick from parent data
                if col_name in fks:
                    ref_table, ref_col = fks[col_name]
                    if ref_table in self.table_data and self.table_data[ref_table]:
                        parent_rows = self.table_data[ref_table]
                        idx = self.rng.randint(0, len(parent_rows) - 1)
                        ref_val = parent_rows[idx].get(ref_col, idx + 1)
                        row[col_name] = ref_val
                        continue
                    else:
                        # Parent not generated yet or empty — use row index
                        row[col_name] = self.rng.randint(1, max(1, num_rows))
                        continue

                val = self._generate_value(col_name, col_stats, i, table, unique, seen_unique)
                row[col_name] = val

            rows.append(row)
        return rows

    def _generate_value(self, col_name, col_stats, row_idx, table, unique_cols, seen_unique):
        """Generate a single synthetic value based on column type and stats."""
        col_type = col_stats["type"]

        # Distribution-based generation for low-cardinality columns
        if "distribution" in col_stats:
            return self._pick_from_distribution(col_stats["distribution"])

        # Type-specific generation
        if col_type in ("int4", "int8", "int2"):
            mn = col_stats.get("min") or 1
            mx = col_stats.get("max") or 10000
            if mn == mx:
                mx = mn + 100
            return self.rng.randint(int(mn), int(mx))

        if col_type in ("float4", "float8"):
            mn = col_stats.get("min") or 0.0
            mx = col_stats.get("max") or 1000.0
            return round(self.rng.uniform(float(mn), float(mx)), 2)

        if col_type == "numeric":
            mn = float(col_stats.get("min") or 0)
            mx = float(col_stats.get("max") or 999.99)
            if mn == mx:
                mx = mn + 100
            return round(self.rng.uniform(mn, mx), 2)

        if col_type in ("text", "varchar", "bpchar"):
            return self._generate_text(col_name, col_stats, row_idx, table,
                                       col_name in unique_cols, seen_unique)

        if col_type in ("timestamptz", "timestamp"):
            return self._generate_timestamp(col_stats)

        if col_type == "date":
            ts = self._generate_timestamp(col_stats)
            return ts.split(" ")[0] if " " in ts else ts

        if col_type == "bool":
            return self.rng.choice(["true", "false"])

        if col_type == "uuid":
            return self._random_uuid()

        if col_type in ("jsonb", "json"):
            return json.dumps({"synthetic": True, "row": row_idx})

        # Fallback
        return f"synthetic_{row_idx}"

    def _pick_from_distribution(self, dist):
        """Weighted random pick from a value distribution dict."""
        values = list(dist.keys())
        weights = list(dist.values())
        return self.rng.choices(values, weights=weights, k=1)[0]

    def _generate_text(self, col_name, col_stats, row_idx, table,
                       is_unique, seen_unique):
        """Generate realistic text based on column name heuristics."""
        name_lower = col_name.lower()

        if "email" in name_lower:
            for _ in range(100):
                first = self.rng.choice(self.FIRST_NAMES).lower()
                last = self.rng.choice(self.LAST_NAMES).lower()
                num = self.rng.randint(1, 9999)
                domain = self.rng.choice(self.DOMAINS)
                val = f"{first}.{last}{num}@{domain}"
                if not is_unique or val not in seen_unique[col_name]:
                    seen_unique[col_name].add(val)
                    return val
            return f"user{row_idx}@{self.rng.choice(self.DOMAINS)}"

        if "name" in name_lower and "user" not in name_lower:
            first = self.rng.choice(self.FIRST_NAMES)
            last = self.rng.choice(self.LAST_NAMES)
            if is_unique:
                val = f"{first} {last} {row_idx}"
            else:
                val = f"{first} {last}"
            return val

        if "category" in name_lower or "type" in name_lower:
            return self.rng.choice(self.CATEGORIES)

        if "status" in name_lower:
            if "order" in table.lower():
                return self._pick_from_distribution({
                    "pending": 0.10, "processing": 0.05,
                    "shipped": 0.55, "delivered": 0.25, "cancelled": 0.05,
                })
            return self.rng.choice(self.STATUSES["default"])

        if "body" in name_lower or "description" in name_lower or "comment" in name_lower:
            words = self.rng.randint(5, 30)
            return " ".join(
                "".join(self.rng.choices(string.ascii_lowercase, k=self.rng.randint(3, 8)))
                for _ in range(words)
            )

        if "url" in name_lower or "link" in name_lower:
            return f"https://example.com/{table}/{row_idx}"

        if "phone" in name_lower:
            return f"+1{self.rng.randint(2000000000, 9999999999)}"

        # Generic text
        avg_len = col_stats.get("avg_length") or 10
        length = max(3, int(avg_len))
        base = f"{table}_{col_name}_{row_idx}"
        if len(base) > length:
            return base[:length]
        return base

    def _generate_timestamp(self, col_stats):
        """Generate a timestamp within the observed range."""
        now = datetime.now(timezone.utc)
        start = now - timedelta(days=365)
        end = now

        if col_stats.get("min"):
            try:
                start = datetime.fromisoformat(
                    col_stats["min"].replace(" ", "T").split("+")[0]
                ).replace(tzinfo=timezone.utc)
            except (ValueError, AttributeError):
                pass
        if col_stats.get("max"):
            try:
                end = datetime.fromisoformat(
                    col_stats["max"].replace(" ", "T").split("+")[0]
                ).replace(tzinfo=timezone.utc)
            except (ValueError, AttributeError):
                pass

        if start >= end:
            end = start + timedelta(days=30)

        delta = (end - start).total_seconds()
        offset = self.rng.uniform(0, delta)
        ts = start + timedelta(seconds=offset)
        return ts.strftime("%Y-%m-%d %H:%M:%S+00")

    def _random_uuid(self):
        """Generate a random UUID v4."""
        return "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}".format(
            self.rng.getrandbits(32),
            self.rng.getrandbits(16),
            self.rng.getrandbits(12),
            0x8000 | self.rng.getrandbits(14),
            self.rng.getrandbits(48),
        )

    def write_sql(self, output_path, tables):
        """Write the complete SQL file: DDL + data + indexes."""
        ordered = self.topo_sort_tables(tables)

        with open(output_path, "w") as f:
            f.write("-- Synthetic data generated by pg-retest synthetic-data-gen.py\n")
            f.write(f"-- Generated: {datetime.now(timezone.utc).isoformat()}\n")
            f.write(f"-- Scale factor: {self.scale}\n")
            f.write(f"-- Tables: {len(ordered)}\n")
            total_rows = sum(len(self.table_data.get(t, [])) for t in ordered)
            f.write(f"-- Total rows: {total_rows}\n\n")

            f.write("BEGIN;\n\n")

            # Drop tables in reverse order (children first)
            for table in reversed(ordered):
                f.write(f"DROP TABLE IF EXISTS {table} CASCADE;\n")
            f.write("\n")

            # Create tables in FK order
            for table in ordered:
                f.write(f"{self.table_ddl[table]}\n\n")

            # Add FK constraints after all tables exist
            for table in ordered:
                for col, ref_table, ref_col in self.fk_map.get(table, []):
                    f.write(
                        f"ALTER TABLE {table} ADD FOREIGN KEY ({col}) "
                        f"REFERENCES {ref_table}({ref_col});\n"
                    )
            f.write("\n")

            # Insert data
            for table in ordered:
                rows = self.table_data.get(table, [])
                if not rows:
                    continue

                f.write(f"-- {table}: {len(rows)} rows\n")
                cols = list(rows[0].keys())
                col_list = ", ".join(cols)

                # Batch inserts for performance
                batch_size = 100
                for batch_start in range(0, len(rows), batch_size):
                    batch = rows[batch_start:batch_start + batch_size]
                    f.write(f"INSERT INTO {table} ({col_list}) VALUES\n")
                    value_lines = []
                    for row in batch:
                        vals = []
                        for c in cols:
                            v = row[c]
                            vals.append(self._sql_literal(v))
                        value_lines.append(f"  ({', '.join(vals)})")
                    f.write(",\n".join(value_lines))
                    f.write(";\n")
                f.write("\n")

            # Reset sequences so subsequent INSERTs get correct IDs
            for table in ordered:
                rows = self.table_data.get(table, [])
                if rows:
                    f.write(
                        f"SELECT setval(pg_get_serial_sequence('{table}', 'id'), "
                        f"{len(rows)}, true);\n"
                    )
            f.write("\n")

            # Create indexes
            for table in ordered:
                for idx in self.table_indexes.get(table, []):
                    f.write(f"{idx}\n")
            f.write("\n")

            f.write("COMMIT;\n")

            # Analyze for fresh stats
            f.write("\n")
            for table in ordered:
                f.write(f"ANALYZE {table};\n")

    def _sql_literal(self, value):
        """Convert a Python value to a SQL literal."""
        if value is None:
            return "NULL"
        if isinstance(value, bool):
            return "true" if value else "false"
        if isinstance(value, (int, float)):
            return str(value)
        # String — escape single quotes
        s = str(value).replace("'", "''")
        return f"'{s}'"


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="Generate synthetic data matching a pg-retest workload profile.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Basic usage
  python3 demo/synthetic-data-gen.py \\
      --workload workload.wkl \\
      --source-db "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \\
      --output synthetic-data.sql

  # 3x scale for load testing
  python3 demo/synthetic-data-gen.py \\
      --workload workload.wkl \\
      --source-db "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \\
      --scale 3 \\
      --output synthetic-data-3x.sql

  # Custom seed for reproducibility
  python3 demo/synthetic-data-gen.py \\
      --workload workload.wkl \\
      --source-db "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \\
      --seed 12345 \\
      --output synthetic-data.sql
        """,
    )
    parser.add_argument("--workload", required=True,
                        help="Path to .wkl workload file")
    parser.add_argument("--source-db", required=True,
                        help="Connection string for source database")
    parser.add_argument("--output", required=True,
                        help="Output path for generated SQL file")
    parser.add_argument("--scale", type=float, default=1.0,
                        help="Scale factor for row counts (default: 1.0)")
    parser.add_argument("--seed", type=int, default=42,
                        help="Random seed for reproducibility (default: 42)")
    parser.add_argument("--tables", nargs="*", default=None,
                        help="Specific tables to include (default: auto-detect from workload)")
    parser.add_argument("--verbose", "-v", action="store_true",
                        help="Print detailed progress")

    args = parser.parse_args()

    if not os.path.exists(args.workload):
        print(f"Error: workload file not found: {args.workload}", file=sys.stderr)
        sys.exit(1)

    # Step 1: Analyze workload
    if args.verbose:
        print(f"Analyzing workload: {args.workload}")
    wl = WorkloadAnalyzer(args.workload)
    wl_tables = wl.extract_referenced_tables()
    if args.verbose:
        print(f"  Queries: {wl.query_count()}")
        print(f"  Referenced tables: {', '.join(sorted(wl_tables))}")

    # Step 2: Connect to source DB and get schema
    if args.verbose:
        print(f"Connecting to source database...")
    sa = SchemaAnalyzer(args.source_db)

    db_tables = set(sa.get_tables())
    if args.tables:
        tables = set(args.tables) & db_tables
    else:
        tables = wl_tables & db_tables

    if not tables:
        print("Error: no matching tables found between workload and database.",
              file=sys.stderr)
        print(f"  Workload references: {', '.join(sorted(wl_tables))}",
              file=sys.stderr)
        print(f"  Database has: {', '.join(sorted(db_tables))}",
              file=sys.stderr)
        sa.close()
        sys.exit(1)

    # Also include FK-referenced tables not in the workload
    expanded = set(tables)
    for table in list(tables):
        for _col, ref_table, _ref_col in sa.get_foreign_keys(table):
            if ref_table in db_tables:
                expanded.add(ref_table)
    tables = expanded

    if args.verbose:
        print(f"  Tables to generate: {', '.join(sorted(tables))}")

    # Step 3: Generate data
    if args.verbose:
        print(f"Analyzing schema and statistics...")
    gen = DataGenerator(sa, wl, args.scale, seed=args.seed)
    gen.analyze(tables)

    if args.verbose:
        print(f"Generating synthetic data (scale={args.scale})...")
    gen.generate_all(tables)

    # Step 4: Write SQL
    if args.verbose:
        print(f"Writing SQL to: {args.output}")
    gen.write_sql(args.output, tables)

    sa.close()

    # Summary
    total_rows = sum(len(gen.table_data.get(t, [])) for t in tables)
    print(f"Generated {args.output}:")
    print(f"  Tables: {len(tables)}")
    print(f"  Total rows: {total_rows:,}")
    print(f"  Scale: {args.scale}x")
    for table in gen.topo_sort_tables(tables):
        n = len(gen.table_data.get(table, []))
        print(f"    {table}: {n:,} rows")


if __name__ == "__main__":
    main()

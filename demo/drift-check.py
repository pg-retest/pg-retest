#!/usr/bin/env python3
"""
Drift check: compare two PostgreSQL databases table by table.
Reports row counts, checksum differences, and sample mismatches.

Usage:
    python3 demo/drift-check.py \
        --db-a "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \
        --db-b "host=localhost port=5451 dbname=ecommerce user=demo password=demo"

    # Strict mode (exit 1 if any drift found, useful for CI):
    python3 demo/drift-check.py --strict \
        --db-a "host=localhost port=5450 dbname=ecommerce user=demo password=demo" \
        --db-b "host=localhost port=5451 dbname=ecommerce user=demo password=demo"

Requires: psycopg2 (pip install psycopg2-binary)
"""

import argparse
import sys

try:
    import psycopg2
    import psycopg2.extras
except ImportError:
    print("ERROR: psycopg2 is required. Install with: pip install psycopg2-binary", file=sys.stderr)
    sys.exit(2)


def parse_args():
    parser = argparse.ArgumentParser(
        description="Compare two PostgreSQL databases table by table for drift detection."
    )
    parser.add_argument(
        "--db-a",
        required=True,
        help='Connection string for database A (e.g., "host=localhost port=5450 dbname=ecommerce user=demo password=demo")',
    )
    parser.add_argument(
        "--db-b",
        required=True,
        help='Connection string for database B (e.g., "host=localhost port=5451 dbname=ecommerce user=demo password=demo")',
    )
    parser.add_argument(
        "--schema",
        default="public",
        help="Schema to compare (default: public)",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit with code 1 if any drift is detected (for CI use)",
    )
    parser.add_argument(
        "--sample-limit",
        type=int,
        default=5,
        help="Number of sample mismatched rows to display per table (default: 5)",
    )
    return parser.parse_args()


def get_tables(conn, schema):
    """Get all user tables in the given schema, sorted by name."""
    with conn.cursor() as cur:
        cur.execute(
            """
            SELECT table_name
            FROM information_schema.tables
            WHERE table_schema = %s
              AND table_type = 'BASE TABLE'
            ORDER BY table_name
            """,
            (schema,),
        )
        return [row[0] for row in cur.fetchall()]


def get_row_count(conn, schema, table):
    """Get the exact row count for a table."""
    with conn.cursor() as cur:
        cur.execute(
            'SELECT COUNT(*) FROM "{}"."{}"'.format(schema, table)
        )
        return cur.fetchone()[0]


def get_table_checksum(conn, schema, table):
    """
    Compute an aggregate MD5 checksum over all rows in a table.
    Returns None if the table is empty.
    """
    with conn.cursor() as cur:
        cur.execute(
            """
            SELECT md5(string_agg(row_hash, '' ORDER BY row_hash))
            FROM (
                SELECT md5(CAST(t.* AS text)) AS row_hash
                FROM "{}"."{}" t
            ) sub
            """.format(schema, table)
        )
        result = cur.fetchone()[0]
        return result


def get_primary_key_columns(conn, schema, table):
    """Get primary key column names for a table."""
    with conn.cursor() as cur:
        cur.execute(
            """
            SELECT kcu.column_name
            FROM information_schema.table_constraints tc
            JOIN information_schema.key_column_usage kcu
                USING (constraint_schema, constraint_name, table_schema, table_name)
            WHERE tc.constraint_type = 'PRIMARY KEY'
              AND tc.table_schema = %s
              AND tc.table_name = %s
            ORDER BY kcu.ordinal_position
            """,
            (schema, table),
        )
        return [row[0] for row in cur.fetchall()]


def get_sample_drift_rows(conn_a, conn_b, schema, table, pk_cols, limit):
    """
    Find sample rows that exist in one database but not the other,
    based on primary key. Returns (only_in_a, only_in_b) lists.
    """
    if not pk_cols:
        return [], []

    pk_select = ", ".join('"' + c + '"' for c in pk_cols)
    pk_join = " AND ".join(
        'a."{col}" = b."{col}"'.format(col=c) for c in pk_cols
    )

    only_in_a = []
    only_in_b = []

    # Rows in A but not in B
    query_a = """
        SELECT {pk}
        FROM "{schema}"."{table}" a
        WHERE NOT EXISTS (
            SELECT 1 FROM "{schema}"."{table}" b
            WHERE {join}
        )
        LIMIT {limit}
    """.format(
        pk=pk_select, schema=schema, table=table, join=pk_join, limit=limit
    )

    # We need to query across both databases. Since they are separate connections,
    # we fetch PKs from one side and check existence on the other.

    # Strategy: get all PKs from both sides and diff.
    # For large tables this is expensive, so we use a row-hash approach instead.
    # Actually, we can't do cross-database joins. Use a simpler approach:
    # fetch a sample of rows with their hashes from each side.

    # Simpler approach: if counts differ, show rows from the side with more rows
    # by ordering by PK descending (newest rows are most likely to be the diff).
    try:
        with conn_a.cursor(cursor_factory=psycopg2.extras.DictCursor) as cur:
            cur.execute(
                """
                SELECT {pk}, md5(CAST(t.* AS text)) AS _row_hash
                FROM "{schema}"."{table}" t
                ORDER BY {pk} DESC
                LIMIT {limit}
                """.format(pk=pk_select, schema=schema, table=table, limit=limit * 10)
            )
            rows_a = {tuple(row[c] for c in pk_cols): row["_row_hash"] for row in cur.fetchall()}

        with conn_b.cursor(cursor_factory=psycopg2.extras.DictCursor) as cur:
            cur.execute(
                """
                SELECT {pk}, md5(CAST(t.* AS text)) AS _row_hash
                FROM "{schema}"."{table}" t
                ORDER BY {pk} DESC
                LIMIT {limit}
                """.format(pk=pk_select, schema=schema, table=table, limit=limit * 10)
            )
            rows_b = {tuple(row[c] for c in pk_cols): row["_row_hash"] for row in cur.fetchall()}

        # Find PKs only in A (among the sampled rows)
        for pk_vals in list(rows_a.keys())[:limit]:
            if pk_vals not in rows_b:
                only_in_a.append(dict(zip(pk_cols, pk_vals)))

        # Find PKs only in B (among the sampled rows)
        for pk_vals in list(rows_b.keys())[:limit]:
            if pk_vals not in rows_a:
                only_in_b.append(dict(zip(pk_cols, pk_vals)))

        # Find rows with same PK but different hash
        changed = []
        for pk_vals in rows_a:
            if pk_vals in rows_b and rows_a[pk_vals] != rows_b[pk_vals]:
                changed.append(dict(zip(pk_cols, pk_vals)))
                if len(changed) >= limit:
                    break

        return only_in_a, only_in_b, changed

    except Exception as e:
        print("    (Could not fetch sample rows: {})".format(e))
        return [], [], []


def format_number(n):
    """Format a number with comma separators."""
    return "{:,}".format(n)


def main():
    args = parse_args()

    try:
        conn_a = psycopg2.connect(args.db_a)
        conn_a.set_session(readonly=True, autocommit=True)
    except Exception as e:
        print("ERROR: Cannot connect to db-a: {}".format(e), file=sys.stderr)
        sys.exit(2)

    try:
        conn_b = psycopg2.connect(args.db_b)
        conn_b.set_session(readonly=True, autocommit=True)
    except Exception as e:
        print("ERROR: Cannot connect to db-b: {}".format(e), file=sys.stderr)
        conn_a.close()
        sys.exit(2)

    schema = args.schema

    # Get tables from both databases
    tables_a = set(get_tables(conn_a, schema))
    tables_b = set(get_tables(conn_b, schema))
    all_tables = sorted(tables_a | tables_b)

    if not all_tables:
        print("No tables found in schema '{}'.".format(schema))
        conn_a.close()
        conn_b.close()
        sys.exit(0)

    # Report header
    print()
    print("Drift Check: {} tables in schema '{}'".format(len(all_tables), schema))
    print()

    header = "{:<30s} {:>10s} {:>10s} {:>10s}   {}".format(
        "Table", "db-a", "db-b", "Diff", "Status"
    )
    separator = "\u2500" * len(header)

    print(header)
    print(separator)

    match_count = 0
    drift_count = 0
    drift_details = []

    for table in all_tables:
        in_a = table in tables_a
        in_b = table in tables_b

        if not in_a:
            print(
                "{:<30s} {:>10s} {:>10s} {:>10s}   {}".format(
                    table, "-", "exists", "-", "MISSING in db-a"
                )
            )
            drift_count += 1
            continue

        if not in_b:
            print(
                "{:<30s} {:>10s} {:>10s} {:>10s}   {}".format(
                    table, "exists", "-", "-", "MISSING in db-b"
                )
            )
            drift_count += 1
            continue

        count_a = get_row_count(conn_a, schema, table)
        count_b = get_row_count(conn_b, schema, table)
        diff = count_b - count_a

        # Check checksums even if counts match (data could differ with same count)
        checksum_a = get_table_checksum(conn_a, schema, table)
        checksum_b = get_table_checksum(conn_b, schema, table)
        checksums_match = checksum_a == checksum_b

        if diff == 0 and checksums_match:
            status = "MATCH"
            match_count += 1
        else:
            if diff != 0 and not checksums_match:
                status = "DRIFT (rows + data)"
            elif diff != 0:
                status = "DRIFT (rows)"
            else:
                status = "DRIFT (data)"
            drift_count += 1
            drift_details.append(
                (table, count_a, count_b, diff, checksums_match)
            )

        diff_str = "{:+,}".format(diff) if diff != 0 else "0"

        print(
            "{:<30s} {:>10s} {:>10s} {:>10s}   {}".format(
                table,
                format_number(count_a),
                format_number(count_b),
                diff_str,
                status,
            )
        )

    print(separator)
    print()

    total = match_count + drift_count
    print(
        "Summary: {}/{} tables match, {}/{} have drift".format(
            match_count, total, drift_count, total
        )
    )

    # Show drift details
    if drift_details:
        print()
        print("Drift Details")
        print("=" * 60)

        for table, count_a, count_b, diff, checksums_match in drift_details:
            print()
            print("  {} (db-a: {}, db-b: {}, diff: {:+,})".format(
                table, format_number(count_a), format_number(count_b), diff
            ))

            if not checksums_match:
                print("  Checksums differ -- data content has changed")

            pk_cols = get_primary_key_columns(conn_a, schema, table)
            if pk_cols:
                result = get_sample_drift_rows(
                    conn_a, conn_b, schema, table, pk_cols, args.sample_limit
                )
                only_in_a, only_in_b, changed = result

                if only_in_a:
                    print("  Sample rows only in db-a (by PK):")
                    for row in only_in_a:
                        print("    {}".format(row))

                if only_in_b:
                    print("  Sample rows only in db-b (by PK):")
                    for row in only_in_b:
                        print("    {}".format(row))

                if changed:
                    print("  Sample rows with same PK but different data:")
                    for row in changed:
                        print("    {}".format(row))

                if not only_in_a and not only_in_b and not changed:
                    print("  (Drift exists but no sample differences found in recent rows)")
            else:
                print("  (No primary key -- cannot identify specific differing rows)")

    print()

    conn_a.close()
    conn_b.close()

    if args.strict and drift_count > 0:
        sys.exit(1)

    sys.exit(0)


if __name__ == "__main__":
    main()

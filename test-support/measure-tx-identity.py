#!/usr/bin/env python3
"""Measure COMMIT round trips on an existing deployed project database.

Usage: python3 test-support/measure-tx-identity.py CONNECTION [--samples 200]
Requires psycopg 3. CONNECTION is a libpq connection string or service name.
Run against the deployed primary with its normal synchronous standby settings.
"""

import argparse
import math
import statistics
import time

import psycopg

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("connection")
parser.add_argument("--samples", type=int, default=200)
args = parser.parse_args()
if args.samples < 1:
    parser.error("--samples must be positive")
results = {False: [], True: []}
with psycopg.connect(args.connection, autocommit=True) as connection:
    print(connection.execute(
        "SELECT version(), current_database(), current_user, "
        "current_setting('synchronous_commit'), current_setting('synchronous_standby_names')"
    ).fetchone())
    for sample in range(args.samples + 10):
        # Alternate order to reduce drift. Exclude ten warm-up pairs.
        for identity in ([False, True] if sample % 2 else [True, False]):
            if sample == 0 and identity:
                print("before_identity", connection.execute(
                    "SELECT to_jsonb(w), pg_current_wal_flush_lsn(), "
                    "(SELECT jsonb_agg(to_jsonb(i)) FROM pg_stat_io i WHERE object='wal') "
                    "FROM pg_stat_wal w"
                ).fetchone())
            connection.execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
            if identity:
                connection.execute(
                    "INSERT INTO platform.tx_identity (principal_id, role, application, operation) "
                    "VALUES ('00000000-0000-4000-8000-000000000001', 'measurement', 'measurement', 'admin:measurement')"
                )
            connection.execute("SET TRANSACTION READ ONLY")
            connection.execute("SELECT 1").fetchone()
            start = time.perf_counter_ns()
            connection.execute("COMMIT")
            elapsed = (time.perf_counter_ns() - start) / 1_000_000
            if sample == 0 and identity:
                print("after_identity", connection.execute(
                    "SELECT to_jsonb(w), pg_current_wal_flush_lsn(), "
                    "(SELECT jsonb_agg(to_jsonb(i)) FROM pg_stat_io i WHERE object='wal') "
                    "FROM pg_stat_wal w"
                ).fetchone())
            if sample >= 10:
                results[identity].append(elapsed)
for identity, samples in results.items():
    ordered = sorted(samples)
    print(f"identity={identity} samples={len(samples)} "
          f"mean_ms={statistics.mean(samples):.3f} "
          f"median_ms={statistics.median(samples):.3f} "
          f"p95_ms={ordered[math.ceil(len(ordered) * .95) - 1]:.3f}")

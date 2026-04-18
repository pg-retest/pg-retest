# Benches

Criterion benches for hot-path SQL handling.

## Benches

- `substitute_bench.rs` — `correlate::substitute::substitute_ids`
- `mask_bench.rs` — `capture::masking::mask_sql_literals`

Run with `cargo bench` (optionally `--bench <name>`).

## Baselines directory

`benches/baselines/` contains text snapshots of criterion output captured at
specific moments during the SQL parsing Phase 1 upgrade. These are journey
artifacts — **do not use them as CI no-regression gates.** Criterion's
`change:` lines reflect comparisons against local `target/criterion/` state,
not between the committed files.

If you want a clean before/after comparison on your own machine:

```bash
# 1. Check out the commit you want to baseline against.
git checkout <commit-before-change>

# 2. Clear any cached criterion state.
rm -rf target/criterion

# 3. Capture baseline.
cargo bench --bench mask_bench 2>&1 | tee /tmp/mask_before.txt
cargo bench --bench substitute_bench 2>&1 | tee /tmp/substitute_before.txt

# 4. Apply your change, then re-capture.
git checkout <commit-with-change>
cargo bench --bench mask_bench 2>&1 | tee /tmp/mask_after.txt
cargo bench --bench substitute_bench 2>&1 | tee /tmp/substitute_after.txt

# 5. Diff the `time:` lines.
grep "time:" /tmp/mask_before.txt /tmp/mask_after.txt
```

Criterion noise bands are typically ±3–5 %. Regressions outside that band
warrant investigation.

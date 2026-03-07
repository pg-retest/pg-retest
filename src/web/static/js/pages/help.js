// Help page — rendered statically
document.addEventListener('DOMContentLoaded', () => {
    const el = document.getElementById('help-content');
    if (!el) return;

    el.innerHTML = `
    <div class="fade-in space-y-6 max-w-4xl">
        <div class="card">
            <h2 class="text-lg font-semibold mb-4">Getting Started</h2>
            <div class="space-y-4 text-sm text-slate-300 leading-relaxed">
                <p><strong class="text-slate-200">pg-retest</strong> captures SQL workloads from PostgreSQL, replays them against target databases, and produces performance comparison reports. Use it to validate config changes, server migrations, and scaling scenarios.</p>

                <h3 class="text-base font-semibold text-slate-200 mt-6">Quick Start</h3>
                <ol class="list-decimal list-inside space-y-2 ml-2">
                    <li><strong>Upload</strong> — Go to <a href="#workloads" class="text-accent hover:underline">Workloads</a> and upload a PostgreSQL CSV log or MySQL slow log</li>
                    <li><strong>Replay</strong> — Go to <a href="#replay" class="text-accent hover:underline">Replay</a>, select your workload, enter a target connection string, and start</li>
                    <li><strong>Compare</strong> — Go to <a href="#compare" class="text-accent hover:underline">Compare</a> to see latency differences between source and replay</li>
                </ol>

                <h3 class="text-base font-semibold text-slate-200 mt-6">Capture Methods</h3>
                <div class="grid grid-cols-1 md:grid-cols-3 gap-3">
                    <div class="card">
                        <div class="font-mono text-accent text-xs mb-1">pg-csv</div>
                        <p class="text-xs text-slate-400">Parse PostgreSQL CSV logs (set <code class="text-slate-300">log_destination = 'csvlog'</code>)</p>
                    </div>
                    <div class="card">
                        <div class="font-mono text-accent text-xs mb-1">mysql-slow</div>
                        <p class="text-xs text-slate-400">Parse MySQL slow query logs with automatic SQL transform to PostgreSQL</p>
                    </div>
                    <div class="card">
                        <div class="font-mono text-accent text-xs mb-1">proxy</div>
                        <p class="text-xs text-slate-400">Run a PG wire protocol proxy between clients and PostgreSQL</p>
                    </div>
                </div>
            </div>
        </div>

        <div class="card">
            <h2 class="text-lg font-semibold mb-4">CLI Reference</h2>
            <div class="space-y-4 text-sm">
                <div class="overflow-x-auto">
                    <table class="data-table">
                        <thead><tr><th>Command</th><th>Description</th><th>Key Flags</th></tr></thead>
                        <tbody>
                            <tr><td class="text-accent">capture</td><td>Capture workload from logs</td><td class="text-xs">--source-log, --source-type, --mask-values</td></tr>
                            <tr><td class="text-accent">replay</td><td>Replay workload against target</td><td class="text-xs">--workload, --target, --speed, --scale</td></tr>
                            <tr><td class="text-accent">compare</td><td>Compare source vs replay</td><td class="text-xs">--source, --replay, --threshold</td></tr>
                            <tr><td class="text-accent">inspect</td><td>View workload profile</td><td class="text-xs">--classify</td></tr>
                            <tr><td class="text-accent">proxy</td><td>Capture proxy</td><td class="text-xs">--listen, --target, --pool-size</td></tr>
                            <tr><td class="text-accent">run</td><td>Full CI/CD pipeline</td><td class="text-xs">--config</td></tr>
                            <tr><td class="text-accent">ab</td><td>A/B variant testing</td><td class="text-xs">--workload, --variant</td></tr>
                            <tr><td class="text-accent">transform</td><td>AI-assisted workload transform</td><td class="text-xs">--workload, --prompt, --provider, --apply</td></tr>
                            <tr><td class="text-accent">web</td><td>Web dashboard</td><td class="text-xs">--port, --data-dir</td></tr>
                        </tbody>
                    </table>
                </div>
            </div>
        </div>

        <div class="card">
            <h2 class="text-lg font-semibold mb-4">Pipeline Config Reference</h2>
            <div class="text-sm">
                <pre class="bg-slate-950 rounded-lg p-4 font-mono text-xs text-slate-300 overflow-x-auto leading-relaxed"><code># .pg-retest.toml

[capture]
workload = "workload.wkl"          # Pre-captured workload file
# source_log = "pg.csv"            # Or capture from log
# source_type = "pg-csv"           # pg-csv | mysql-slow | rds
# mask_values = false              # PII masking

[provision]
backend = "docker"
image = "postgres:16"
restore_from = "backup.sql"
port = 5441

[replay]
target = "postgres://user:pass@localhost:5441/db"
speed = 1.0                        # 0 = max speed
read_only = false
scale = 1                          # Uniform scale
stagger_ms = 0
# scale_analytical = 2             # Per-category scaling
# scale_transactional = 4

[thresholds]
p95_max_ms = 100.0
p99_max_ms = 500.0
error_rate_max_pct = 1.0
regression_max_count = 5
regression_threshold_pct = 20.0

[output]
json = "report.json"
junit = "results.xml"

# A/B variant mode (replaces [provision] + [replay].target)
# [[variants]]
# label = "PG 15"
# target = "postgres://user:pass@pg15:5432/db"
# [[variants]]
# label = "PG 16"
# target = "postgres://user:pass@pg16:5432/db"</code></pre>
            </div>
        </div>

        <div class="card">
            <h2 class="text-lg font-semibold mb-4">Exit Codes</h2>
            <div class="overflow-x-auto text-sm">
                <table class="data-table">
                    <thead><tr><th>Code</th><th>Meaning</th></tr></thead>
                    <tbody>
                        <tr><td><span class="badge badge-success">0</span></td><td>Pass — all thresholds met</td></tr>
                        <tr><td><span class="badge badge-danger">1</span></td><td>Threshold violation</td></tr>
                        <tr><td><span class="badge badge-danger">2</span></td><td>Configuration error</td></tr>
                        <tr><td><span class="badge badge-danger">3</span></td><td>Capture error</td></tr>
                        <tr><td><span class="badge badge-danger">4</span></td><td>Provision error</td></tr>
                        <tr><td><span class="badge badge-danger">5</span></td><td>Replay error</td></tr>
                    </tbody>
                </table>
            </div>
        </div>
    </div>
    `;
});

// Drift Check page
function driftPage() {
    return {
        loading: false,
        results: null,

        async load() {
            const el = document.getElementById('drift-content');
            if (!el) return;
            this.render(el);
        },

        render(el) {
            el.innerHTML = `
            <div class="fade-in space-y-4">
                <div class="card">
                    <h3 class="section-title mb-4">Database Drift Check</h3>
                    <p class="text-sm text-slate-400 mb-4">Compare row counts between two databases to detect data drift after replay or migration.</p>
                    <div class="grid grid-cols-2 gap-4 mb-4">
                        <div>
                            <label class="label">DB-A Connection String</label>
                            <input class="input" id="drift-db-a"
                                   placeholder="host=localhost dbname=source user=postgres password=...">
                        </div>
                        <div>
                            <label class="label">DB-B Connection String</label>
                            <input class="input" id="drift-db-b"
                                   placeholder="host=localhost dbname=target user=postgres password=...">
                        </div>
                    </div>
                    <button class="btn btn-primary" id="drift-run-btn" onclick="runDriftCheck()">Run Drift Check</button>
                </div>

                <div id="drift-results" class="space-y-4"></div>
            </div>
            `;
        },
    };
}

async function runDriftCheck() {
    const dbA = document.getElementById('drift-db-a').value.trim();
    const dbB = document.getElementById('drift-db-b').value.trim();
    if (!dbA || !dbB) {
        window.showToast('Both connection strings are required', 'error');
        return;
    }

    const resultsEl = document.getElementById('drift-results');
    const runBtn = document.getElementById('drift-run-btn');
    resultsEl.innerHTML = Status.loading('Connecting to databases and comparing tables...');
    runBtn.disabled = true;

    const res = await api.driftCheck({ db_a: dbA, db_b: dbB });
    runBtn.disabled = false;

    if (res.error) {
        resultsEl.innerHTML = Status.error(res.error);
        return;
    }

    const tables = res.tables || [];
    const summary = res.summary || {};

    const summaryColor = summary.drifted > 0 ? 'danger' : 'accent';

    resultsEl.innerHTML = `
        <!-- Summary -->
        <div class="grid-stats">
            ${Status.statCard({ label: 'Total Tables', value: summary.total || 0, color: 'accent' })}
            ${Status.statCard({ label: 'Matching', value: summary.matching || 0, color: 'accent' })}
            ${Status.statCard({ label: 'Drifted', value: summary.drifted || 0, color: summaryColor })}
        </div>

        <!-- Results table -->
        <div class="card">
            <h3 class="section-title mb-3">Table Comparison</h3>
            <div class="overflow-x-auto">
                <table class="data-table">
                    <thead>
                        <tr>
                            <th>Table</th>
                            <th class="text-right">DB-A Count</th>
                            <th class="text-right">DB-B Count</th>
                            <th class="text-right">Diff</th>
                            <th>Status</th>
                        </tr>
                    </thead>
                    <tbody>
                        ${tables.map(t => {
                            const statusClass = t.status === 'MATCH'
                                ? 'text-accent'
                                : t.status === 'ERROR' ? 'text-amber-400' : 'text-danger';
                            const statusBg = t.status === 'MATCH'
                                ? 'bg-accent/10 border-accent/20 text-accent'
                                : t.status === 'ERROR' ? 'bg-amber-400/10 border-amber-400/20 text-amber-400' : 'bg-danger/10 border-danger/20 text-danger';
                            const diff = t.diff != null ? (t.diff > 0 ? '+' + t.diff : t.diff) : '-';
                            return `<tr>
                                <td class="font-mono text-sm text-slate-300">${t.name}</td>
                                <td class="text-right font-mono text-sm">${t.db_a_count != null ? t.db_a_count.toLocaleString() : '-'}</td>
                                <td class="text-right font-mono text-sm">${t.db_b_count != null ? t.db_b_count.toLocaleString() : '-'}</td>
                                <td class="text-right font-mono text-sm ${statusClass}">${diff}</td>
                                <td><span class="badge ${statusBg}">${t.status}</span></td>
                            </tr>`;
                        }).join('')}
                    </tbody>
                </table>
            </div>
        </div>
    `;
}

// Settings page — saved database connections (SQLite-backed, shared with the CLI's
// `pg-retest connections` command and with --target/--source-db `@label` resolution).
function settingsPage() {
    return {
        connections: [],

        async load() {
            const el = document.getElementById('settings-content');
            if (!el) return;
            el.innerHTML = Status.loading();

            const connections = await SavedConnections.refresh();
            this.connections = connections;
            this.render(el);
        },

        render(el) {
            el.innerHTML = `
            <div class="fade-in space-y-4">
                <div class="card">
                    <h3 class="section-title mb-2">Saved Connections</h3>
                    <p class="text-sm text-slate-500 mb-4">
                        Labeled database connection strings, stored in <code class="font-mono text-accent">pg-retest.db</code>.
                        Reference one anywhere a connection string field is expected — in this dashboard, or from the CLI via
                        <code class="font-mono text-accent">--target @label</code> / <code class="font-mono text-accent">--source-db @label</code>.
                        Manage the same list from the CLI with <code class="font-mono text-accent">pg-retest connections list|add|rm</code>.
                    </p>
                    <div class="grid grid-cols-1 md:grid-cols-3 gap-3 mb-3">
                        <input class="input" id="settings-conn-label" placeholder="label, e.g. prod-replica">
                        <input class="input md:col-span-2" id="settings-conn-string" list="conn-history-list"
                               placeholder="host=localhost dbname=... user=... password=...">
                    </div>
                    <button class="btn btn-primary" onclick="saveSettingsConnection()">Save Connection</button>
                    <div id="settings-connections-list" class="mt-4"></div>
                </div>
            </div>`;

            window.settingsPageInstance = this;
            this.renderList();
        },

        renderList() {
            const el = document.getElementById('settings-connections-list');
            if (!el) return;

            if (this.connections.length === 0) {
                el.innerHTML = '<p class="text-sm text-slate-500">No saved connections yet.</p>';
                return;
            }

            el.innerHTML = `
                <table class="data-table">
                    <thead><tr><th>Label</th><th>Connection String</th><th></th></tr></thead>
                    <tbody>
                        ${this.connections.map(c => `
                            <tr>
                                <td class="font-mono text-sm text-accent">@${this.escapeHtml(c.label)}</td>
                                <td class="font-mono text-xs text-slate-400">${this.escapeHtml(c.conn_string)}</td>
                                <td class="text-right">
                                    <button class="btn btn-danger btn-sm" onclick="deleteSettingsConnection('${this.escapeHtml(c.label)}')">Delete</button>
                                </td>
                            </tr>
                        `).join('')}
                    </tbody>
                </table>`;
        },

        escapeHtml(str) {
            if (!str) return '';
            return String(str).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
        },
    };
}

async function saveSettingsConnection() {
    const label = document.getElementById('settings-conn-label').value.trim();
    const connString = document.getElementById('settings-conn-string').value.trim();
    if (!label || !connString) {
        window.showToast('Label and connection string are required', 'error');
        return;
    }

    const res = await api.saveConnection({ label, conn_string: connString });
    if (res.error) {
        window.showToast(res.error, 'error');
        return;
    }

    window.showToast(`Saved "${label}"`, 'success');
    ConnHistory.remember(connString);
    document.getElementById('settings-conn-label').value = '';
    document.getElementById('settings-conn-string').value = '';
    if (window.settingsPageInstance) window.settingsPageInstance.load();
}

async function deleteSettingsConnection(label) {
    if (!confirm(`Delete saved connection "${label}"?`)) return;

    const res = await api.deleteConnection(label);
    if (res.error) {
        window.showToast(res.error, 'error');
        return;
    }

    window.showToast(`Deleted "${label}"`, 'success');
    if (window.settingsPageInstance) window.settingsPageInstance.load();
}

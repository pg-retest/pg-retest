// Proxy control page
function proxyPage() {
    return {
        status: null,
        queryFeed: [],
        qpsChart: null,
        loading: true,
        config: {
            listen: '0.0.0.0:5433',
            target: '',
            pool_size: 100,
            mask_values: false,
            no_capture: false,
        },

        async load() {
            const el = document.getElementById('proxy-content');
            if (!el) return;
            el.innerHTML = Status.loading();

            const res = await api.proxyStatus();
            this.status = res;
            this.loading = false;
            this.render(el);
            this.setupWsListeners();
        },

        setupWsListeners() {
            wsClient.on('ProxyQueryExecuted', (msg) => {
                this.queryFeed.unshift(msg);
                if (this.queryFeed.length > 100) this.queryFeed.pop();
                this.renderFeed();
            });
            wsClient.on('ProxyStats', (msg) => {
                Charts.updateQPSChart('qps-chart', msg.qps || 0);
            });
            wsClient.on('ProxyStarted', () => {
                this.status = { ...this.status, running: true };
                this.renderStatus();
            });
            wsClient.on('ProxyStopped', () => {
                this.status = { ...this.status, running: false, capturing: false };
                this.renderStatus();
            });
            wsClient.on('CaptureStopped', (msg) => {
                if (this.status) {
                    this.status = { ...this.status, capturing: false };
                    if (msg && msg.capture_id) {
                        // Refresh full status to get updated capture_history
                        api.proxyStatus().then(s => {
                            this.status = s;
                            this.renderStatus();
                            this.renderCaptureHistory();
                        });
                        return;
                    }
                }
                this.renderStatus();
            });
        },

        render(el) {
            el.innerHTML = `
            <div class="fade-in space-y-4">
                <!-- Proxy status & controls -->
                <div class="grid grid-cols-1 lg:grid-cols-3 gap-4">
                    <div class="lg:col-span-2 card">
                        <div class="section-header">
                            <h3 class="section-title">Proxy Configuration</h3>
                            <div class="flex items-center gap-2">
                                <div id="proxy-status-badge"></div>
                                <div id="capture-status-badge"></div>
                            </div>
                        </div>
                        <div class="space-y-3">
                            <div class="grid grid-cols-2 gap-3">
                                <div>
                                    <label class="label">Listen Address</label>
                                    <input class="input" type="text" id="proxy-listen"
                                           value="${this.config.listen}" placeholder="0.0.0.0:5433">
                                </div>
                                <div>
                                    <label class="label">Target PostgreSQL</label>
                                    <input class="input" type="text" id="proxy-target"
                                           value="${this.config.target}" placeholder="localhost:5432">
                                </div>
                            </div>
                            <div class="grid grid-cols-3 gap-3">
                                <div>
                                    <label class="label">Pool Size</label>
                                    <input class="input" type="number" id="proxy-pool-size"
                                           value="${this.config.pool_size}">
                                </div>
                                <div class="flex items-end pb-1">
                                    <label class="flex items-center gap-2 cursor-pointer text-sm text-slate-300">
                                        <input type="checkbox" id="proxy-mask"
                                               class="w-4 h-4 rounded border-slate-600 bg-slate-800">
                                        Mask PII
                                    </label>
                                </div>
                                <div class="flex items-end pb-1">
                                    <label class="flex items-center gap-2 cursor-pointer text-sm text-slate-300">
                                        <input type="checkbox" id="proxy-no-capture"
                                               class="w-4 h-4 rounded border-slate-600 bg-slate-800">
                                        No Capture
                                    </label>
                                </div>
                            </div>
                            <div class="flex gap-2 pt-2 flex-wrap">
                                <button class="btn btn-primary" id="proxy-start-btn" onclick="startProxy()">
                                    Start Proxy
                                </button>
                                <button class="btn btn-danger" id="proxy-stop-btn" onclick="stopProxyConfirm()" disabled>
                                    Stop Proxy
                                </button>
                                <button class="btn btn-secondary" id="capture-start-btn" onclick="startCapture()" style="display:none">
                                    Start Capture
                                </button>
                                <button class="btn btn-warning" id="capture-stop-btn" onclick="stopCapture()" style="display:none">
                                    Stop Capture
                                </button>
                            </div>
                        </div>
                    </div>

                    <!-- QPS chart -->
                    <div class="card">
                        <h3 class="section-title mb-2">Queries/sec</h3>
                        <div class="chart-container" style="height: 180px">
                            <canvas id="qps-chart"></canvas>
                        </div>
                    </div>
                </div>

                <!-- Live query feed -->
                <div class="card">
                    <div class="section-header">
                        <h3 class="section-title">Live Query Feed</h3>
                        <button class="btn btn-secondary btn-sm" onclick="clearQueryFeed()">Clear</button>
                    </div>
                    <div class="query-feed" id="query-feed">
                        <div class="text-center text-slate-500 text-sm py-4">
                            Start the proxy to see live queries
                        </div>
                    </div>
                </div>

                <!-- Capture History -->
                <div class="card" id="capture-history-card" style="display:none">
                    <div class="section-header">
                        <h3 class="section-title">Capture History</h3>
                    </div>
                    <div id="capture-history-body"></div>
                </div>
            </div>
            `;

            Charts.createQPSChart('qps-chart');
            this.renderStatus();
            this.renderCaptureHistory();
        },

        renderStatus() {
            const badge = document.getElementById('proxy-status-badge');
            const captureBadge = document.getElementById('capture-status-badge');
            const startBtn = document.getElementById('proxy-start-btn');
            const stopBtn = document.getElementById('proxy-stop-btn');
            const captureStartBtn = document.getElementById('capture-start-btn');
            const captureStopBtn = document.getElementById('capture-stop-btn');
            if (!badge) return;

            const running = this.status && this.status.running;
            const capturing = this.status && this.status.capturing;
            const finalizing = this.status && this.status.capture_state === 'finalizing';

            // Proxy running badge
            if (running) {
                badge.innerHTML = '<span class="badge badge-success">Running</span>';
                if (startBtn) startBtn.disabled = true;
                if (stopBtn) stopBtn.disabled = false;
            } else {
                badge.innerHTML = '<span class="badge badge-neutral">Stopped</span>';
                if (startBtn) startBtn.disabled = false;
                if (stopBtn) stopBtn.disabled = true;
            }

            // Capture state badge
            if (captureBadge) {
                if (finalizing) {
                    captureBadge.innerHTML = '<span class="badge badge-warning">Finalizing</span>';
                } else if (capturing) {
                    captureBadge.innerHTML = '<span class="badge badge-accent">Capturing</span>';
                } else if (running) {
                    captureBadge.innerHTML = '<span class="badge badge-neutral">Idle</span>';
                } else {
                    captureBadge.innerHTML = '';
                }
            }

            // Capture control buttons — only shown when proxy is running
            if (captureStartBtn) {
                captureStartBtn.style.display = (running && !capturing && !finalizing) ? 'inline-flex' : 'none';
            }
            if (captureStopBtn) {
                captureStopBtn.style.display = (running && (capturing || finalizing)) ? 'inline-flex' : 'none';
            }
        },

        renderCaptureHistory() {
            const card = document.getElementById('capture-history-card');
            const body = document.getElementById('capture-history-body');
            if (!card || !body) return;

            const history = this.status && this.status.capture_history;
            if (!history || history.length === 0) {
                card.style.display = 'none';
                return;
            }

            card.style.display = '';
            body.innerHTML = `
                <table class="w-full text-sm">
                    <thead>
                        <tr class="text-left text-slate-400 border-b border-slate-700">
                            <th class="pb-2 pr-4">Timestamp</th>
                            <th class="pb-2 pr-4">Capture ID</th>
                            <th class="pb-2 pr-4">Queries</th>
                            <th class="pb-2">Sessions</th>
                        </tr>
                    </thead>
                    <tbody>
                        ${history.map(h => `
                        <tr class="border-b border-slate-800 hover:bg-slate-800/40">
                            <td class="py-2 pr-4 text-slate-300">${h.timestamp ? new Date(h.timestamp).toLocaleString() : '—'}</td>
                            <td class="py-2 pr-4 font-mono text-slate-400 text-xs">${h.capture_id ? h.capture_id.substring(0, 12) + '…' : '—'}</td>
                            <td class="py-2 pr-4 text-slate-200">${h.queries != null ? h.queries.toLocaleString() : '—'}</td>
                            <td class="py-2 text-slate-200">${h.sessions != null ? h.sessions : '—'}</td>
                        </tr>`).join('')}
                    </tbody>
                </table>
            `;
        },

        renderFeed() {
            const feed = document.getElementById('query-feed');
            if (!feed || this.queryFeed.length === 0) return;
            feed.innerHTML = this.queryFeed.map(q => `
                <div class="query-feed-item">
                    <span class="text-slate-500 flex-shrink-0">S${q.session_id}</span>
                    <span class="text-slate-300 flex-1">${Tables.truncateSQL(q.sql_preview, 120)}</span>
                    <span class="text-accent flex-shrink-0">${Tables.formatDuration(q.duration_us)}</span>
                </div>
            `).join('');
        },
    };
}

async function startProxy() {
    const config = {
        listen: document.getElementById('proxy-listen').value,
        target: document.getElementById('proxy-target').value,
        pool_size: parseInt(document.getElementById('proxy-pool-size').value) || 100,
        mask_values: document.getElementById('proxy-mask').checked,
        no_capture: document.getElementById('proxy-no-capture').checked,
    };
    if (!config.target) {
        window.showToast('Target address is required', 'error');
        return;
    }
    const res = await api.startProxy(config);
    if (res.error) {
        window.showToast(res.error, 'error');
    } else {
        window.showToast('Proxy started', 'success');
    }
}

async function stopProxyConfirm() {
    if (!confirm('This will disconnect all active clients. Continue?')) return;
    const res = await api.stopProxy();
    if (res.error) {
        window.showToast(res.error, 'error');
    } else {
        window.showToast('Proxy stopped', 'success');
    }
}

async function startCapture() {
    const res = await api.toggleCapture();
    if (res.error) {
        window.showToast(res.error, 'error');
    } else {
        window.showToast('Capture started', 'success');
        // Update local state optimistically
        const page = Alpine.$data(document.querySelector('[x-data]'));
        if (page && page.status) {
            page.status = { ...page.status, capturing: true, capture_id: res.capture_id };
            page.renderStatus();
        }
    }
}

async function stopCapture() {
    const res = await api.toggleCapture();
    if (res.error) {
        window.showToast(res.error, 'error');
    } else {
        const queries = res.queries != null ? res.queries.toLocaleString() : '?';
        window.showToast(`Capture stopped — ${queries} queries saved`, 'success');
        // Refresh full status to get updated history
        const freshStatus = await api.proxyStatus();
        const page = Alpine.$data(document.querySelector('[x-data]'));
        if (page) {
            page.status = freshStatus;
            page.renderStatus();
            page.renderCaptureHistory();
        }
    }
}

// Keep old stopProxy as alias in case other code calls it
async function stopProxy() {
    return stopProxyConfirm();
}

function clearQueryFeed() {
    const feed = document.getElementById('query-feed');
    if (feed) feed.innerHTML = '<div class="text-center text-slate-500 text-sm py-4">Feed cleared</div>';
}

// Tuning page — AI-assisted database tuning with iteration timeline
function tuningPage() {
    return {
        workloads: [],
        loading: true,
        status: 'idle', // idle, running, completed, error
        taskId: null,
        iterations: [],
        totalImprovement: null,
        iterationsCompleted: null,
        errorMessage: '',

        async load() {
            const el = document.getElementById('tuning-content');
            if (!el) return;
            el.innerHTML = Status.loading();

            const res = await api.listWorkloads();
            this.workloads = res.workloads || [];
            this.loading = false;
            this.render(el);
            this.setupWsListeners();
        },

        setupWsListeners() {
            wsClient.on('TuningIterationStarted', (msg) => {
                if (this.taskId && msg.task_id === this.taskId) {
                    this.iterations.push({
                        iteration: msg.iteration,
                        status: 'started',
                        recommendations: null,
                        changeApplied: null,
                        changeSummary: '',
                        improvementPct: null,
                    });
                    this.renderTimeline();
                    window.showToast(`Iteration ${msg.iteration} started`, 'info');
                }
            });

            wsClient.on('TuningRecommendations', (msg) => {
                if (this.taskId && msg.task_id === this.taskId) {
                    const iter = this.iterations.find(i => i.iteration === msg.iteration);
                    if (iter) {
                        iter.status = 'recommendations';
                        iter.recommendations = msg.count;
                    }
                    this.renderTimeline();
                }
            });

            wsClient.on('TuningChangeApplied', (msg) => {
                if (this.taskId && msg.task_id === this.taskId) {
                    const iter = this.iterations.find(i => i.iteration === msg.iteration);
                    if (iter) {
                        iter.status = 'applied';
                        iter.changeApplied = msg.success;
                        iter.changeSummary = msg.summary;
                    }
                    this.renderTimeline();
                    if (msg.success) {
                        window.showToast(`Changes applied: ${msg.summary}`, 'success');
                    } else {
                        window.showToast(`Change failed: ${msg.summary}`, 'warning');
                    }
                }
            });

            wsClient.on('TuningReplayCompleted', (msg) => {
                if (this.taskId && msg.task_id === this.taskId) {
                    const iter = this.iterations.find(i => i.iteration === msg.iteration);
                    if (iter) {
                        iter.status = 'replayed';
                        iter.improvementPct = msg.improvement_pct;
                    }
                    this.renderTimeline();
                }
            });

            wsClient.on('TuningCompleted', (msg) => {
                if (this.taskId && msg.task_id === this.taskId) {
                    this.status = 'completed';
                    this.totalImprovement = msg.total_improvement_pct;
                    this.iterationsCompleted = msg.iterations_completed;
                    this.updateControls();
                    this.renderTimeline();
                    this.renderSummary();
                    window.showToast('Tuning completed!', 'success');
                }
            });

            wsClient.on('Error', (msg) => {
                if (this.status === 'running') {
                    this.status = 'error';
                    this.errorMessage = msg.message;
                    this.updateControls();
                    this.renderTimeline();
                    window.showToast(msg.message, 'error');
                }
            });
        },

        updateControls() {
            const startBtn = document.getElementById('tuning-start-btn');
            const cancelBtn = document.getElementById('tuning-cancel-btn');
            const running = this.status === 'running';
            if (startBtn) startBtn.disabled = running;
            if (cancelBtn) cancelBtn.disabled = !running;
        },

        renderTimeline() {
            const el = document.getElementById('tuning-timeline');
            if (!el) return;

            if (this.iterations.length === 0 && this.status !== 'error') {
                el.innerHTML = '';
                return;
            }

            let html = '<div class="space-y-3">';

            this.iterations.forEach(iter => {
                const statusBadge = this.getIterationBadge(iter);
                const improvementHtml = iter.improvementPct !== null
                    ? `<span class="font-mono text-sm ${iter.improvementPct > 0 ? 'text-accent' : iter.improvementPct < 0 ? 'text-danger' : 'text-slate-400'}">${iter.improvementPct > 0 ? '+' : ''}${iter.improvementPct.toFixed(1)}%</span>`
                    : '';

                html += `
                    <div class="card border-slate-700/30">
                        <div class="flex items-center justify-between mb-2">
                            <div class="flex items-center gap-3">
                                <span class="text-sm font-semibold text-slate-200">Iteration ${iter.iteration}</span>
                                ${statusBadge}
                            </div>
                            ${improvementHtml}
                        </div>
                        <div class="flex items-center gap-4 text-xs text-slate-500">
                            ${iter.recommendations !== null ? `<span>Recommendations: <span class="font-mono text-slate-300">${iter.recommendations}</span></span>` : ''}
                            ${iter.changeApplied !== null ? `<span>Applied: <span class="font-mono ${iter.changeApplied ? 'text-accent' : 'text-danger'}">${iter.changeApplied ? 'Yes' : 'No'}</span></span>` : ''}
                            ${iter.changeSummary ? `<span class="text-slate-400 truncate max-w-xs">${this.escapeHtml(iter.changeSummary)}</span>` : ''}
                        </div>
                    </div>
                `;
            });

            if (this.status === 'error') {
                html += `
                    <div class="card border-danger/30 bg-danger/5">
                        <div class="flex items-center gap-2 text-danger text-sm">
                            <svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                                <circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/>
                            </svg>
                            ${this.escapeHtml(this.errorMessage)}
                        </div>
                    </div>
                `;
            }

            html += '</div>';
            el.innerHTML = html;
        },

        getIterationBadge(iter) {
            switch (iter.status) {
                case 'started':
                    return '<span class="badge badge-info"><span class="spinner" style="width:0.7em;height:0.7em"></span> Analyzing</span>';
                case 'recommendations':
                    return '<span class="badge badge-info">Recommending</span>';
                case 'applied':
                    return iter.changeApplied
                        ? '<span class="badge badge-warning">Replaying</span>'
                        : '<span class="badge badge-danger">Apply Failed</span>';
                case 'replayed':
                    return iter.improvementPct > 0
                        ? '<span class="badge badge-success">Improved</span>'
                        : '<span class="badge badge-neutral">No Improvement</span>';
                default:
                    return '<span class="badge badge-neutral">Pending</span>';
            }
        },

        renderSummary() {
            const el = document.getElementById('tuning-summary');
            if (!el) return;

            if (this.status !== 'completed') {
                el.innerHTML = '';
                return;
            }

            const improvementColor = this.totalImprovement > 0 ? 'accent' : this.totalImprovement < 0 ? 'danger' : 'amber';
            const improvementSign = this.totalImprovement > 0 ? '+' : '';

            el.innerHTML = `
                <div class="card border-accent/30">
                    <div class="section-header">
                        <h3 class="section-title">Tuning Complete</h3>
                        <span class="badge badge-success">Done</span>
                    </div>
                    <div class="grid grid-cols-3 gap-4 mb-4">
                        ${Status.statCard({ label: 'Total Improvement', value: improvementSign + this.totalImprovement.toFixed(1) + '%', color: improvementColor })}
                        ${Status.statCard({ label: 'Iterations', value: this.iterationsCompleted, color: 'blue' })}
                        ${Status.statCard({ label: 'Status', value: 'Complete', color: 'accent' })}
                    </div>
                    <div class="flex gap-2">
                        <button class="btn btn-secondary" onclick="tuningReset()">Start New Tuning</button>
                        <button class="btn btn-secondary" onclick="location.hash='replay'">Replay</button>
                    </div>
                </div>
            `;
        },

        render(el) {
            const wklOptions = this.workloads.map(w =>
                `<option value="${w.id}">${w.name} (${w.total_sessions}s / ${w.total_queries}q)</option>`
            ).join('');

            el.innerHTML = `
            <div class="fade-in space-y-4">
                <!-- Config panel -->
                <div class="grid grid-cols-1 lg:grid-cols-3 gap-4">
                    <div class="lg:col-span-2 card">
                        <h3 class="section-title mb-4">Tuning Configuration</h3>
                        <div class="space-y-4">
                            <div class="grid grid-cols-2 gap-4">
                                <div>
                                    <label class="label">Workload</label>
                                    <select class="input" id="tuning-workload">
                                        <option value="">Select workload...</option>
                                        ${wklOptions}
                                    </select>
                                </div>
                                <div>
                                    <label class="label">Provider</label>
                                    <select class="input" id="tuning-provider" onchange="tuningProviderChanged(this.value)">
                                        <option value="claude">Claude</option>
                                        <option value="openai">OpenAI</option>
                                        <option value="ollama">Ollama</option>
                                    </select>
                                </div>
                            </div>
                            <div>
                                <label class="label">Target Connection String</label>
                                <input class="input" type="text" id="tuning-target"
                                       placeholder="postgres://user:pass@host:5432/dbname">
                            </div>
                            <div class="grid grid-cols-2 gap-4">
                                <div>
                                    <label class="label">API Key</label>
                                    <input class="input" type="password" id="tuning-api-key"
                                           placeholder="sk-...">
                                </div>
                                <div>
                                    <label class="label">Model (optional)</label>
                                    <input class="input" type="text" id="tuning-model"
                                           placeholder="claude-sonnet-4-20250514">
                                </div>
                            </div>
                            <div id="tuning-api-url-row" class="hidden">
                                <label class="label">API URL</label>
                                <input class="input" type="text" id="tuning-api-url"
                                       placeholder="http://localhost:11434">
                            </div>
                            <div>
                                <label class="label">Hint (optional)</label>
                                <textarea class="input" id="tuning-hint" rows="3"
                                          placeholder="Provide context about your workload, e.g.:
- OLTP workload with heavy writes to orders table
- Running on 16GB RAM, 4 CPU cores
- Currently seeing slow JOINs on reports"></textarea>
                            </div>
                        </div>
                    </div>

                    <!-- Options panel -->
                    <div class="card">
                        <h3 class="section-title mb-4">Options</h3>
                        <div class="space-y-4">
                            <div>
                                <label class="label">Max Iterations: <span class="text-accent" id="tuning-iter-display">3</span></label>
                                <input type="range" min="1" max="10" value="3"
                                       id="tuning-max-iterations"
                                       oninput="document.getElementById('tuning-iter-display').textContent = this.value"
                                       class="w-full h-1 bg-slate-700 rounded-lg appearance-none cursor-pointer accent-teal-500">
                            </div>
                            <div>
                                <label class="label">Speed</label>
                                <input class="input" type="number" id="tuning-speed" value="1.0"
                                       step="0.1" min="0">
                            </div>
                            <label class="flex items-center gap-2 cursor-pointer text-sm text-slate-300">
                                <input type="checkbox" id="tuning-apply"
                                       class="w-4 h-4 rounded border-slate-600 bg-slate-800">
                                Apply changes to target
                            </label>
                            <label class="flex items-center gap-2 cursor-pointer text-sm text-slate-300">
                                <input type="checkbox" id="tuning-readonly"
                                       class="w-4 h-4 rounded border-slate-600 bg-slate-800">
                                Read-only mode (strip DML)
                            </label>
                        </div>
                    </div>
                </div>

                <!-- Controls -->
                <div class="card">
                    <div class="flex items-center gap-3 mb-3">
                        <button class="btn btn-primary" id="tuning-start-btn" onclick="startTuning()">
                            Start Tuning
                        </button>
                        <button class="btn btn-danger" id="tuning-cancel-btn" onclick="cancelTuning()" disabled>
                            Cancel
                        </button>
                        <div id="tuning-status-indicator" class="text-xs text-slate-500 font-mono ml-2"></div>
                    </div>
                </div>

                <!-- Iteration timeline -->
                <div id="tuning-timeline"></div>

                <!-- Summary -->
                <div id="tuning-summary"></div>
            </div>
            `;
        },

        escapeHtml(str) {
            const div = document.createElement('div');
            div.textContent = str;
            return div.innerHTML;
        },

        rerender() {
            const el = document.getElementById('tuning-content');
            if (el) this.render(el);
        },
    };
}

// ── Global handlers ──────────────────────────────────────────────────

function getTuningPage() {
    return Alpine.$data(document.querySelector('[x-data="tuningPage()"]'));
}

function tuningProviderChanged(value) {
    const urlRow = document.getElementById('tuning-api-url-row');
    const apiKeyInput = document.getElementById('tuning-api-key');
    const modelInput = document.getElementById('tuning-model');
    if (urlRow) urlRow.classList.toggle('hidden', value !== 'ollama');
    if (apiKeyInput) apiKeyInput.placeholder = value === 'ollama' ? 'Not required' : 'sk-...';
    if (modelInput) {
        const placeholders = { claude: 'claude-sonnet-4-20250514', openai: 'gpt-4o', ollama: 'llama3.1' };
        modelInput.placeholder = placeholders[value] || '';
    }
}

async function startTuning() {
    const page = getTuningPage();
    if (!page) return;

    const workloadId = document.getElementById('tuning-workload').value;
    const target = document.getElementById('tuning-target').value;
    if (!workloadId) { window.showToast('Select a workload', 'error'); return; }
    if (!target) { window.showToast('Enter a target connection string', 'error'); return; }

    const provider = document.getElementById('tuning-provider').value;
    const apiKey = document.getElementById('tuning-api-key').value;
    const model = document.getElementById('tuning-model').value;
    const apiUrl = document.getElementById('tuning-api-url')?.value || '';
    const hint = document.getElementById('tuning-hint').value;
    const maxIterations = parseInt(document.getElementById('tuning-max-iterations').value) || 3;
    const speed = parseFloat(document.getElementById('tuning-speed').value) || 1.0;
    const apply = document.getElementById('tuning-apply').checked;
    const readOnly = document.getElementById('tuning-readonly').checked;

    if (provider !== 'ollama' && !apiKey) {
        window.showToast('API key is required', 'error');
        return;
    }

    const body = {
        workload_id: workloadId,
        target: target,
        provider: provider,
        max_iterations: maxIterations,
        apply: apply,
        speed: speed,
        read_only: readOnly,
    };
    if (apiKey) body.api_key = apiKey;
    if (model) body.model = model;
    if (apiUrl) body.api_url = apiUrl;
    if (hint) body.hint = hint;

    // Reset state
    page.iterations = [];
    page.totalImprovement = null;
    page.iterationsCompleted = null;
    page.errorMessage = '';
    page.status = 'running';

    document.getElementById('tuning-start-btn').disabled = true;
    document.getElementById('tuning-cancel-btn').disabled = false;
    const indicator = document.getElementById('tuning-status-indicator');
    if (indicator) indicator.innerHTML = '<span class="spinner" style="width:0.8em;height:0.8em"></span> Running...';

    const timelineEl = document.getElementById('tuning-timeline');
    if (timelineEl) timelineEl.innerHTML = '';
    const summaryEl = document.getElementById('tuning-summary');
    if (summaryEl) summaryEl.innerHTML = '';

    const res = await api.post('/tuning/start', body);
    if (res.error) {
        window.showToast(res.error, 'error');
        page.status = 'error';
        page.errorMessage = res.error;
        document.getElementById('tuning-start-btn').disabled = false;
        document.getElementById('tuning-cancel-btn').disabled = true;
        if (indicator) indicator.innerHTML = '';
    } else {
        page.taskId = res.task_id;
        window.showToast('Tuning started', 'success');
    }
}

async function cancelTuning() {
    const page = getTuningPage();
    if (!page || !page.taskId) return;

    await api.post(`/tuning/${page.taskId}/cancel`);
    page.status = 'idle';
    window.showToast('Tuning cancelled', 'warning');
    document.getElementById('tuning-start-btn').disabled = false;
    document.getElementById('tuning-cancel-btn').disabled = true;
    const indicator = document.getElementById('tuning-status-indicator');
    if (indicator) indicator.innerHTML = '';
}

function tuningReset() {
    const page = getTuningPage();
    if (!page) return;
    page.status = 'idle';
    page.taskId = null;
    page.iterations = [];
    page.totalImprovement = null;
    page.iterationsCompleted = null;
    page.errorMessage = '';
    page.rerender();
}

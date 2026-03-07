// Transform page — workload analysis, AI plan generation, and transform application
function transformPage() {
    return {
        workloads: [],
        selectedWorkload: '',
        analysis: null,
        prompt: '',
        provider: 'claude',
        apiKey: '',
        model: '',
        apiUrl: '',
        plan: null,
        planJson: '',
        loading: false,
        loadingMessage: '',
        error: '',
        result: null,
        step: 1, // 1=select+analyze, 2=plan, 3=apply

        async load() {
            const el = document.getElementById('transform-content');
            if (!el) return;
            await this.loadWorkloads();
            this.render(el);
        },

        async loadWorkloads() {
            const res = await api.listWorkloads();
            this.workloads = res.workloads || [];
        },

        render(el) {
            const wklOptions = this.workloads.map(w =>
                `<option value="${w.id}" ${this.selectedWorkload === w.id ? 'selected' : ''}>${w.name} (${w.total_sessions}s / ${w.total_queries}q)</option>`
            ).join('');

            el.innerHTML = `
            <div class="fade-in space-y-4">
                <!-- Step indicators -->
                <div class="flex items-center gap-2 mb-2">
                    ${this.renderStepIndicator(1, 'Analyze')}
                    <svg class="w-4 h-4 text-slate-600" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M9 18l6-6-6-6"/></svg>
                    ${this.renderStepIndicator(2, 'Plan')}
                    <svg class="w-4 h-4 text-slate-600" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M9 18l6-6-6-6"/></svg>
                    ${this.renderStepIndicator(3, 'Apply')}
                </div>

                <!-- Step 1: Select & Analyze -->
                <div class="card">
                    <h3 class="section-title mb-4">Select Workload</h3>
                    <div class="space-y-4">
                        <div class="grid grid-cols-2 gap-4">
                            <div>
                                <label class="label">Workload</label>
                                <select class="input" id="transform-workload" onchange="transformSelectWorkload(this.value)">
                                    <option value="">Select workload...</option>
                                    ${wklOptions}
                                </select>
                            </div>
                            <div class="flex items-end">
                                <button class="btn btn-primary" id="transform-analyze-btn" onclick="transformAnalyze()">
                                    Analyze Workload
                                </button>
                            </div>
                        </div>
                    </div>
                </div>

                ${this.loading ? `
                    <div class="card">
                        ${Status.loading(this.loadingMessage || 'Processing...')}
                    </div>
                ` : ''}

                ${this.error ? Status.error(this.error) : ''}

                <!-- Analysis results -->
                ${this.analysis ? this.renderAnalysis() : ''}

                <!-- Step 2: Generate Plan -->
                ${this.step >= 2 ? this.renderPlanForm() : ''}

                <!-- Plan results -->
                ${this.plan ? this.renderPlanResult() : ''}

                <!-- Step 3: Apply result -->
                ${this.result ? this.renderResult() : ''}
            </div>
            `;
        },

        renderStepIndicator(num, label) {
            const active = this.step === num;
            const done = this.step > num;
            const cls = active
                ? 'bg-accent/20 text-accent border-accent/30'
                : done
                    ? 'bg-accent/10 text-accent/60 border-accent/20'
                    : 'bg-slate-800/60 text-slate-500 border-slate-700/50';
            return `<span class="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium border ${cls}">
                ${done ? '<svg class="w-3 h-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><polyline points="20 6 9 17 4 12"/></svg>' : num}
                ${label}
            </span>`;
        },

        renderAnalysis() {
            const a = this.analysis;
            const summary = a.profile_summary;
            return `
                <div class="card">
                    <div class="section-header">
                        <h3 class="section-title">Workload Analysis</h3>
                        <span class="badge badge-info">${a.query_groups.length} groups</span>
                    </div>

                    <div class="grid grid-cols-4 gap-3 mb-4">
                        ${Status.statCard({ label: 'Total Queries', value: summary.total_queries, color: 'accent' })}
                        ${Status.statCard({ label: 'Sessions', value: summary.total_sessions, color: 'blue' })}
                        ${Status.statCard({ label: 'Duration', value: summary.capture_duration_s.toFixed(1) + 's', color: 'amber' })}
                        ${Status.statCard({ label: 'Ungrouped', value: a.ungrouped_queries, color: 'danger' })}
                    </div>

                    <div class="space-y-3">
                        ${a.query_groups.map(g => this.renderGroupCard(g)).join('')}
                    </div>
                </div>
            `;
        },

        renderGroupCard(g) {
            const kindBadges = Object.entries(g.kinds)
                .map(([k, v]) => `<span class="badge badge-neutral">${k}: ${v}</span>`)
                .join(' ');
            const tables = g.tables.map(t => `<span class="font-mono text-accent text-xs">${t}</span>`).join(', ');
            const sampleSql = g.sample_queries.length > 0
                ? `<div class="mt-2 text-xs font-mono text-slate-500 truncate">${this.escapeHtml(g.sample_queries[0])}</div>`
                : '';

            return `
                <div class="card border-slate-700/30">
                    <div class="flex items-start justify-between mb-2">
                        <div>
                            <span class="text-sm font-medium text-slate-200">Group ${g.id}</span>
                            <span class="text-xs text-slate-500 ml-2">${g.query_count} queries (${g.pct_of_total.toFixed(1)}%)</span>
                        </div>
                        <div class="text-xs font-mono text-slate-400">avg ${this.formatDuration(g.avg_duration_us)}</div>
                    </div>
                    <div class="flex items-center gap-2 mb-1">
                        <span class="text-xs text-slate-500">Tables:</span> ${tables}
                    </div>
                    <div class="flex flex-wrap gap-1 mb-1">${kindBadges}</div>
                    ${g.parameter_patterns.common_filters.length > 0
                        ? `<div class="text-xs text-slate-500">Filters: <span class="font-mono text-slate-400">${g.parameter_patterns.common_filters.join(', ')}</span></div>`
                        : ''}
                    ${sampleSql}
                </div>
            `;
        },

        renderPlanForm() {
            return `
                <div class="card">
                    <h3 class="section-title mb-4">Generate Transform Plan</h3>
                    <div class="space-y-4">
                        <div>
                            <label class="label">Transform Prompt</label>
                            <textarea class="input" id="transform-prompt" rows="4" placeholder="Describe the workload transformation you want, e.g.:
- Scale the analytical queries 3x and add a new reporting query
- Add index hint queries before heavy SELECT groups
- Inject periodic maintenance queries (VACUUM, ANALYZE)"
                            >${this.prompt}</textarea>
                        </div>
                        <div class="grid grid-cols-3 gap-4">
                            <div>
                                <label class="label">Provider</label>
                                <select class="input" id="transform-provider" onchange="transformProviderChanged(this.value)">
                                    <option value="claude" ${this.provider === 'claude' ? 'selected' : ''}>Claude</option>
                                    <option value="openai" ${this.provider === 'openai' ? 'selected' : ''}>OpenAI</option>
                                    <option value="ollama" ${this.provider === 'ollama' ? 'selected' : ''}>Ollama</option>
                                </select>
                            </div>
                            <div>
                                <label class="label">API Key</label>
                                <input class="input" type="password" id="transform-api-key" value="${this.apiKey}" placeholder="${this.provider === 'ollama' ? 'Not required' : 'sk-...'}">
                            </div>
                            <div>
                                <label class="label">Model (optional)</label>
                                <input class="input" type="text" id="transform-model" value="${this.model}" placeholder="${this.provider === 'claude' ? 'claude-sonnet-4-20250514' : this.provider === 'openai' ? 'gpt-4o' : 'llama3.1'}">
                            </div>
                        </div>
                        ${this.provider === 'ollama' ? `
                        <div>
                            <label class="label">Ollama URL</label>
                            <input class="input" type="text" id="transform-api-url" value="${this.apiUrl}" placeholder="http://localhost:11434">
                        </div>
                        ` : ''}
                        <div class="flex gap-2">
                            <button class="btn btn-primary" id="transform-plan-btn" onclick="transformGeneratePlan()">Generate Plan</button>
                        </div>
                    </div>
                </div>
            `;
        },

        renderPlanResult() {
            const p = this.plan;
            const rulesSummary = p.transforms.map(r => {
                if (r.type === 'scale') return `Scale "${r.group}" ${r.factor}x`;
                if (r.type === 'inject') return `Inject after "${r.after_group}"`;
                if (r.type === 'inject_session') return `Inject session (${r.repeat || 1} repeats)`;
                if (r.type === 'rewrite') return `Rewrite in "${r.group}"`;
                if (r.type === 'drop') return `Drop "${r.group}"`;
                if (r.type === 'retime') return `Retime "${r.group}" ${r.factor}x`;
                return r.type;
            });

            return `
                <div class="card">
                    <div class="section-header">
                        <h3 class="section-title">Transform Plan</h3>
                        <div class="flex gap-2">
                            <span class="badge badge-info">${p.groups.length} groups</span>
                            <span class="badge badge-success">${p.transforms.length} rules</span>
                        </div>
                    </div>

                    <div class="mb-4">
                        <div class="text-xs text-slate-500 uppercase mb-2">Transform Rules</div>
                        <div class="space-y-1">
                            ${rulesSummary.map(r => `<div class="text-sm font-mono text-slate-300 flex items-center gap-2">
                                <svg class="w-3 h-3 text-accent" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="9 18 15 12 9 6"/></svg>
                                ${r}
                            </div>`).join('')}
                        </div>
                    </div>

                    <div>
                        <label class="label">Plan JSON (editable)</label>
                        <textarea class="input" id="transform-plan-json" rows="16">${this.escapeHtml(this.planJson)}</textarea>
                    </div>

                    <div class="flex gap-2 mt-4">
                        <button class="btn btn-primary" id="transform-apply-btn" onclick="transformApply()">Apply Transform</button>
                        <button class="btn btn-secondary" onclick="transformReset()">Start Over</button>
                    </div>
                </div>
            `;
        },

        renderResult() {
            const r = this.result;
            return `
                <div class="card border-accent/30">
                    <div class="section-header">
                        <h3 class="section-title">Transform Complete</h3>
                        <span class="badge badge-success">Success</span>
                    </div>
                    <div class="grid grid-cols-3 gap-4 mb-4">
                        ${Status.statCard({ label: 'New Workload', value: r.workload_id.substring(0, 8), color: 'accent' })}
                        ${Status.statCard({ label: 'Sessions', value: r.total_sessions, color: 'blue' })}
                        ${Status.statCard({ label: 'Queries', value: r.total_queries, color: 'amber' })}
                    </div>
                    <div class="flex gap-2">
                        <button class="btn btn-primary" onclick="location.hash='workloads'">View Workloads</button>
                        <button class="btn btn-secondary" onclick="location.hash='replay'">Replay</button>
                        <button class="btn btn-secondary" onclick="transformReset()">Start Over</button>
                    </div>
                </div>
            `;
        },

        formatDuration(us) {
            if (us < 1000) return us + 'us';
            if (us < 1000000) return (us / 1000).toFixed(1) + 'ms';
            return (us / 1000000).toFixed(2) + 's';
        },

        escapeHtml(str) {
            const div = document.createElement('div');
            div.textContent = str;
            return div.innerHTML;
        },

        rerender() {
            const el = document.getElementById('transform-content');
            if (el) this.render(el);
        },
    };
}

// ── Global handlers ──────────────────────────────────────────────────

function getTransformPage() {
    return Alpine.$data(document.querySelector('[x-data="transformPage()"]'));
}

function transformSelectWorkload(value) {
    const page = getTransformPage();
    if (page) {
        page.selectedWorkload = value;
        // Reset downstream state when workload changes
        page.analysis = null;
        page.plan = null;
        page.planJson = '';
        page.result = null;
        page.error = '';
        page.step = 1;
    }
}

async function transformAnalyze() {
    const page = getTransformPage();
    if (!page) return;

    const workloadId = document.getElementById('transform-workload').value;
    if (!workloadId) { window.showToast('Select a workload', 'error'); return; }

    page.selectedWorkload = workloadId;
    page.loading = true;
    page.loadingMessage = 'Analyzing workload...';
    page.error = '';
    page.analysis = null;
    page.plan = null;
    page.planJson = '';
    page.result = null;
    page.rerender();

    const res = await api.post('/transform/analyze', { workload_id: workloadId });
    page.loading = false;

    if (res.analysis) {
        page.analysis = res.analysis;
        page.step = 2;
        window.showToast(`Found ${res.analysis.query_groups.length} query groups`, 'success');
    } else {
        page.error = res.error || 'Analysis failed';
    }
    page.rerender();
}

function transformProviderChanged(value) {
    const page = getTransformPage();
    if (page) {
        page.provider = value;
        page.rerender();
    }
}

async function transformGeneratePlan() {
    const page = getTransformPage();
    if (!page) return;

    const prompt = document.getElementById('transform-prompt').value;
    const apiKey = document.getElementById('transform-api-key').value;
    const model = document.getElementById('transform-model')?.value || '';
    const apiUrl = document.getElementById('transform-api-url')?.value || '';
    const provider = document.getElementById('transform-provider').value;

    if (!prompt.trim()) { window.showToast('Enter a transform prompt', 'error'); return; }
    if (provider !== 'ollama' && !apiKey.trim()) { window.showToast('API key is required', 'error'); return; }

    page.prompt = prompt;
    page.apiKey = apiKey;
    page.model = model;
    page.apiUrl = apiUrl;
    page.provider = provider;
    page.loading = true;
    page.loadingMessage = 'Generating transform plan with ' + provider + '...';
    page.error = '';
    page.plan = null;
    page.planJson = '';
    page.result = null;
    page.rerender();

    const body = {
        workload_id: page.selectedWorkload,
        prompt: prompt,
        provider: provider,
        api_key: apiKey,
    };
    if (model) body.model = model;
    if (apiUrl) body.api_url = apiUrl;

    const res = await api.post('/transform/plan', body);
    page.loading = false;

    if (res.plan) {
        page.plan = res.plan;
        page.planJson = JSON.stringify(res.plan, null, 2);
        page.step = 3;
        window.showToast('Plan generated with ' + res.plan.transforms.length + ' transform rules', 'success');
    } else {
        page.error = res.error || 'Plan generation failed';
    }
    page.rerender();
}

async function transformApply() {
    const page = getTransformPage();
    if (!page) return;

    page.loading = true;
    page.loadingMessage = 'Applying transform plan...';
    page.error = '';
    page.result = null;
    page.rerender();

    // Parse plan from textarea in case user edited it
    let planToApply = page.plan;
    try {
        const jsonText = document.getElementById('transform-plan-json').value;
        planToApply = JSON.parse(jsonText);
    } catch (e) {
        page.loading = false;
        page.error = 'Invalid JSON in plan editor: ' + e.message;
        page.rerender();
        return;
    }

    const res = await api.post('/transform/apply', {
        workload_id: page.selectedWorkload,
        plan: planToApply,
    });
    page.loading = false;

    if (res.workload_id) {
        page.result = res;
        page.loadWorkloads(); // refresh list
        window.showToast('Transform applied — new workload created', 'success');
    } else {
        page.error = res.error || 'Apply failed';
    }
    page.rerender();
}

function transformReset() {
    const page = getTransformPage();
    if (!page) return;
    page.analysis = null;
    page.plan = null;
    page.planJson = '';
    page.result = null;
    page.error = '';
    page.prompt = '';
    page.step = 1;
    page.rerender();
}

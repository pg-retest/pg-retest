// Demo page — guided wizard + scenario cards for docker-demo
function demoPage() {
    return {
        demoEnabled: false,
        dbA: '',
        dbB: '',
        wizardSteps: [
            { id: 1, title: 'Explore', desc: 'Inspect the demo workload and classify sessions', icon: '\u{1F50D}', status: 'ready', result: null },
            { id: 2, title: 'Replay', desc: 'Replay workload against Database B', icon: '\u25B6\uFE0F', status: 'locked', result: null },
            { id: 3, title: 'Compare', desc: 'Compare source vs. replay performance', icon: '\u{1F4CA}', status: 'locked', result: null },
            { id: 4, title: 'Scale', desc: 'Replay at 3x scale for capacity testing', icon: '\u{1F4C8}', status: 'locked', result: null },
            { id: 5, title: 'AI Tune', desc: 'Run AI tuning advisor (dry-run)', icon: '\u{1F916}', status: 'locked', result: null },
        ],
        scenarios: [
            { name: 'migration', title: 'Migration Test', desc: 'Replay workload against DB-B and compare performance', status: 'ready', result: null },
            { name: 'capacity', title: 'Capacity Planning', desc: 'Replay at 3x scale with per-category breakdown', status: 'ready', result: null },
            { name: 'ab', title: 'A/B Comparison', desc: 'Compare DB-A vs DB-B with identical traffic', status: 'ready', result: null },
            { name: 'tuning', title: 'AI Tuning', desc: 'Run tuning advisor against DB-B (dry-run)', status: 'ready', result: null },
        ],
        resettingDb: false,

        async load() {
            const el = document.getElementById('demo-content');
            if (!el) return;
            el.innerHTML = Status.loading();

            try {
                const res = await api.get('/demo/config');
                if (res.error) {
                    el.innerHTML = `
                    <div class="fade-in text-center py-16">
                        <div class="text-4xl mb-4 opacity-50">\u{1F6AB}</div>
                        <h2 class="text-lg font-semibold text-slate-300 mb-2">Demo Mode Not Available</h2>
                        <p class="text-sm text-slate-500">Set <code class="font-mono text-accent">PG_RETEST_DEMO=true</code> to enable the demo environment.</p>
                    </div>`;
                    return;
                }
                this.demoEnabled = res.enabled || false;
                this.dbA = res.db_a || '';
                this.dbB = res.db_b || '';

                if (!this.demoEnabled) {
                    el.innerHTML = `
                    <div class="fade-in text-center py-16">
                        <div class="text-4xl mb-4 opacity-50">\u{1F6AB}</div>
                        <h2 class="text-lg font-semibold text-slate-300 mb-2">Demo Mode Disabled</h2>
                        <p class="text-sm text-slate-500">Set <code class="font-mono text-accent">PG_RETEST_DEMO=true</code> to enable the demo environment.</p>
                    </div>`;
                    return;
                }

                this.render(el);
            } catch (e) {
                el.innerHTML = `
                <div class="fade-in text-center py-16">
                    <div class="text-4xl mb-4 opacity-50">\u26A0\uFE0F</div>
                    <h2 class="text-lg font-semibold text-slate-300 mb-2">Error Loading Demo</h2>
                    <p class="text-sm text-slate-500">${this.escapeHtml(e.message || 'Unknown error')}</p>
                </div>`;
            }
        },

        render(el) {
            el.innerHTML = `
            <div class="fade-in space-y-6">
                <!-- Header -->
                <div class="flex items-center justify-between">
                    <div>
                        <h2 class="text-xl font-semibold text-slate-100">Interactive Demo</h2>
                        <p class="text-sm text-slate-500 mt-1">Explore pg-retest capabilities with a pre-built workload and two PostgreSQL databases</p>
                    </div>
                    <div class="flex items-center gap-3">
                        <div class="flex items-center gap-2 px-3 py-1.5 rounded-lg bg-slate-900 border border-slate-800 text-xs">
                            <span class="text-slate-500">DB-A:</span>
                            <span class="font-mono text-accent">${this.escapeHtml(this.dbA)}</span>
                        </div>
                        <div class="flex items-center gap-2 px-3 py-1.5 rounded-lg bg-slate-900 border border-slate-800 text-xs">
                            <span class="text-slate-500">DB-B:</span>
                            <span class="font-mono text-amber-400">${this.escapeHtml(this.dbB)}</span>
                        </div>
                        <button onclick="demoPageInstance.resetDb()"
                                class="px-3 py-1.5 rounded-lg bg-slate-800 border border-slate-700 text-xs text-slate-400 hover:text-slate-200 hover:border-slate-600 transition-colors"
                                id="demo-reset-btn">
                            Reset Databases
                        </button>
                    </div>
                </div>

                <!-- Guided Wizard -->
                <div class="card">
                    <h3 class="section-title mb-4">Guided Walkthrough</h3>
                    <p class="text-sm text-slate-500 mb-6">Step through the core workflow: explore a workload, replay it, compare results, scale it up, and run AI tuning.</p>
                    <div id="demo-wizard"></div>
                </div>

                <!-- Scenarios -->
                <div>
                    <h3 class="section-title mb-4">Quick Scenarios</h3>
                    <p class="text-sm text-slate-500 mb-4">Run complete end-to-end scenarios with a single click.</p>
                    <div id="demo-scenarios" class="grid grid-cols-1 md:grid-cols-2 gap-4"></div>
                </div>
            </div>`;

            // Store reference for onclick handlers
            window.demoPageInstance = this;

            this.renderWizard();
            this.renderScenarios();
        },

        renderWizard() {
            const container = document.getElementById('demo-wizard');
            if (!container) return;

            const stepsHtml = this.wizardSteps.map((step, idx) => {
                const isFirst = idx === 0;
                const isLast = idx === this.wizardSteps.length - 1;
                const isLocked = step.status === 'locked';
                const isRunning = step.status === 'running';
                const isCompleted = step.status === 'completed';
                const isReady = step.status === 'ready';
                const isFailed = step.status === 'failed';

                const statusBadge = isRunning
                    ? '<span class="badge badge-warning"><span class="spinner-sm mr-1"></span>Running</span>'
                    : isCompleted
                    ? '<span class="badge badge-success">Complete</span>'
                    : isFailed
                    ? '<span class="badge badge-danger">Failed</span>'
                    : isLocked
                    ? '<span class="badge badge-muted">Locked</span>'
                    : '<span class="badge badge-info">Ready</span>';

                const connector = !isLast ? `
                    <div class="absolute left-6 top-14 w-0.5 h-6 ${isCompleted ? 'bg-accent/40' : 'bg-slate-800'}"></div>
                ` : '';

                const resultHtml = step.result ? `
                    <div class="mt-3 p-3 rounded-lg bg-slate-950 border border-slate-800 overflow-x-auto">
                        <pre class="text-xs font-mono text-slate-400 whitespace-pre-wrap">${this.escapeHtml(typeof step.result === 'string' ? step.result : JSON.stringify(step.result, null, 2))}</pre>
                    </div>
                ` : '';

                return `
                <div class="relative ${!isLast ? 'pb-6' : ''}">
                    ${connector}
                    <div class="flex items-start gap-4 ${isLocked ? 'opacity-40' : ''}">
                        <!-- Step number -->
                        <div class="w-12 h-12 rounded-xl flex items-center justify-center flex-shrink-0 text-lg
                            ${isCompleted ? 'bg-accent/20 border border-accent/30' :
                              isRunning ? 'bg-amber-400/20 border border-amber-400/30' :
                              isFailed ? 'bg-danger/20 border border-danger/30' :
                              'bg-slate-900 border border-slate-800'}">
                            ${isCompleted ? '<svg class="w-5 h-5 text-accent" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="20 6 9 17 4 12"/></svg>'
                              : isRunning ? '<span class="spinner"></span>'
                              : step.icon}
                        </div>
                        <!-- Content -->
                        <div class="flex-1 min-w-0">
                            <div class="flex items-center gap-3 mb-1">
                                <span class="text-sm font-semibold text-slate-200">Step ${step.id}: ${this.escapeHtml(step.title)}</span>
                                ${statusBadge}
                            </div>
                            <p class="text-xs text-slate-500 mb-2">${this.escapeHtml(step.desc)}</p>
                            ${(isReady || isFailed) ? `
                                <button onclick="demoPageInstance.runWizardStep(${step.id})"
                                        class="px-3 py-1 rounded-md text-xs font-medium bg-accent/10 text-accent border border-accent/20 hover:bg-accent/20 transition-colors">
                                    ${isFailed ? 'Retry' : 'Run Step'}
                                </button>
                            ` : ''}
                            ${resultHtml}
                        </div>
                    </div>
                </div>`;
            }).join('');

            container.innerHTML = stepsHtml;
        },

        renderScenarios() {
            const container = document.getElementById('demo-scenarios');
            if (!container) return;

            const icons = {
                migration: '<svg class="w-6 h-6" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"/></svg>',
                capacity: '<svg class="w-6 h-6" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="20" x2="18" y2="10"/><line x1="12" y1="20" x2="12" y2="4"/><line x1="6" y1="20" x2="6" y2="14"/></svg>',
                ab: '<svg class="w-6 h-6" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M16 3h5v5"/><path d="M8 3H3v5"/><path d="M12 22V8"/><path d="m3 3 5 5"/><path d="m21 3-5 5"/></svg>',
                tuning: '<svg class="w-6 h-6" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>',
            };

            const colors = {
                migration: 'accent',
                capacity: 'blue',
                ab: 'amber',
                tuning: 'purple',
            };

            const cardsHtml = this.scenarios.map(scenario => {
                const isRunning = scenario.status === 'running';
                const isCompleted = scenario.status === 'completed';
                const isFailed = scenario.status === 'failed';
                const icon = icons[scenario.name] || '';

                const statusBadge = isRunning
                    ? '<span class="badge badge-warning"><span class="spinner-sm mr-1"></span>Running</span>'
                    : isCompleted
                    ? '<span class="badge badge-success">Complete</span>'
                    : isFailed
                    ? '<span class="badge badge-danger">Failed</span>'
                    : '<span class="badge badge-info">Ready</span>';

                const resultHtml = scenario.result ? `
                    <div class="mt-3 p-3 rounded-lg bg-slate-950 border border-slate-800 overflow-x-auto">
                        <pre class="text-xs font-mono text-slate-400 whitespace-pre-wrap">${this.escapeHtml(typeof scenario.result === 'string' ? scenario.result : JSON.stringify(scenario.result, null, 2))}</pre>
                    </div>
                ` : '';

                return `
                <div class="card hover:border-slate-700 transition-colors">
                    <div class="flex items-start justify-between mb-3">
                        <div class="flex items-center gap-3">
                            <div class="w-10 h-10 rounded-lg bg-slate-900 border border-slate-800 flex items-center justify-center text-slate-400">
                                ${icon}
                            </div>
                            <div>
                                <h4 class="text-sm font-semibold text-slate-200">${this.escapeHtml(scenario.title)}</h4>
                                <p class="text-xs text-slate-500">${this.escapeHtml(scenario.desc)}</p>
                            </div>
                        </div>
                        ${statusBadge}
                    </div>
                    <div class="flex items-center gap-2">
                        ${!isRunning ? `
                            <button onclick="demoPageInstance.runScenario('${scenario.name}')"
                                    class="px-3 py-1.5 rounded-md text-xs font-medium bg-accent/10 text-accent border border-accent/20 hover:bg-accent/20 transition-colors">
                                ${isCompleted || isFailed ? 'Run Again' : 'Run Scenario'}
                            </button>
                        ` : `
                            <span class="text-xs text-slate-500">Running, please wait...</span>
                        `}
                        ${isCompleted || isFailed ? `
                            <button onclick="demoPageInstance.resetScenario('${scenario.name}')"
                                    class="px-3 py-1.5 rounded-md text-xs font-medium text-slate-400 border border-slate-700 hover:text-slate-200 hover:border-slate-600 transition-colors">
                                Reset
                            </button>
                        ` : ''}
                    </div>
                    ${resultHtml}
                </div>`;
            }).join('');

            container.innerHTML = cardsHtml;
        },

        async runWizardStep(stepId) {
            const step = this.wizardSteps.find(s => s.id === stepId);
            if (!step || step.status === 'locked' || step.status === 'running') return;

            step.status = 'running';
            step.result = null;
            this.renderWizard();

            try {
                const res = await api.post(`/demo/wizard/${stepId}`, {});
                if (res.error) {
                    step.status = 'failed';
                    step.result = res.error;
                    window.showToast(`Step ${stepId} failed: ${res.error}`, 'error');
                } else {
                    step.status = 'completed';
                    step.result = res;
                    window.showToast(`Step ${stepId}: ${step.title} completed`, 'success');

                    // Unlock next step
                    const nextStep = this.wizardSteps.find(s => s.id === stepId + 1);
                    if (nextStep && nextStep.status === 'locked') {
                        nextStep.status = 'ready';
                    }
                }
            } catch (e) {
                step.status = 'failed';
                step.result = e.message || 'Unknown error';
                window.showToast(`Step ${stepId} error: ${e.message}`, 'error');
            }

            this.renderWizard();
        },

        async runScenario(name) {
            const scenario = this.scenarios.find(s => s.name === name);
            if (!scenario || scenario.status === 'running') return;

            scenario.status = 'running';
            scenario.result = null;
            this.renderScenarios();

            try {
                const res = await api.post(`/demo/scenario/${name}`, {});
                if (res.error) {
                    scenario.status = 'failed';
                    scenario.result = res.error;
                    window.showToast(`Scenario "${scenario.title}" failed: ${res.error}`, 'error');
                } else {
                    scenario.status = 'completed';
                    scenario.result = res;
                    window.showToast(`Scenario "${scenario.title}" completed`, 'success');
                }
            } catch (e) {
                scenario.status = 'failed';
                scenario.result = e.message || 'Unknown error';
                window.showToast(`Scenario error: ${e.message}`, 'error');
            }

            this.renderScenarios();
        },

        resetScenario(name) {
            const scenario = this.scenarios.find(s => s.name === name);
            if (!scenario) return;
            scenario.status = 'ready';
            scenario.result = null;
            this.renderScenarios();
        },

        async resetDb() {
            if (this.resettingDb) return;
            this.resettingDb = true;
            const btn = document.getElementById('demo-reset-btn');
            if (btn) {
                btn.disabled = true;
                btn.innerHTML = '<span class="spinner-sm mr-1"></span>Resetting...';
            }

            try {
                const res = await api.post('/demo/reset-db', {});
                if (res.error) {
                    window.showToast(`Database reset failed: ${res.error}`, 'error');
                } else {
                    window.showToast('Databases reset successfully', 'success');

                    // Reset wizard state
                    this.wizardSteps.forEach((s, i) => {
                        s.status = i === 0 ? 'ready' : 'locked';
                        s.result = null;
                    });
                    this.renderWizard();

                    // Reset scenarios
                    this.scenarios.forEach(s => {
                        s.status = 'ready';
                        s.result = null;
                    });
                    this.renderScenarios();
                }
            } catch (e) {
                window.showToast(`Reset error: ${e.message}`, 'error');
            }

            this.resettingDb = false;
            if (btn) {
                btn.disabled = false;
                btn.textContent = 'Reset Databases';
            }
        },

        escapeHtml(str) {
            if (!str) return '';
            const s = String(str);
            return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
        },
    };
}

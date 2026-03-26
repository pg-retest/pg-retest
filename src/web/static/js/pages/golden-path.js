// Golden Path — 8-step guided end-to-end workflow wizard
function goldenPathPage() {
    return {
        currentStep: 1,
        totalSteps: 8,

        // Per-step state
        proxyStatus: null,
        captureRunning: false,
        captureQueryCount: 0,
        captureWorkloadId: null,
        captureInspect: null,
        compiledWorkloadId: null,
        compileStats: null,
        replayRunId: null,
        replayProgress: 0,
        replayDone: false,
        replayResult: null,
        compareReport: null,
        driftResults: null,
        syntheticWorkloadId: null,
        syntheticDataPath: null,
        syntheticStats: null,
        synthReplayRunId: null,
        synthReplayDone: false,
        synthReplayResult: null,
        synthCompareReport: null,

        // Config
        targetConn: '',
        sourceConn: '',
        driftDbA: '',
        driftDbB: '',
        synthSessions: '',
        synthScale: '',
        restorePoint: '',

        // Loading / error states
        stepLoading: {},
        stepErrors: {},
        stepComplete: {},

        // WS unsubscribers
        _wsUnsubs: [],

        async load() {
            const el = document.getElementById('golden-path-content');
            if (!el) return;

            // Subscribe to WS events
            this._wsUnsubs.push(
                wsClient.on('ProxyQuery', (msg) => {
                    this.captureQueryCount++;
                }),
                wsClient.on('ReplayProgress', (msg) => {
                    if (msg.progress != null) this.replayProgress = msg.progress;
                    if (msg.completed) {
                        if (this.currentStep === 4) {
                            this.replayDone = true;
                            this.replayResult = msg;
                        } else if (this.currentStep === 8) {
                            this.synthReplayDone = true;
                            this.synthReplayResult = msg;
                        }
                    }
                }),
                wsClient.on('ReplayCompleted', (msg) => {
                    if (this.currentStep === 4) {
                        this.replayDone = true;
                        this.replayResult = msg;
                        this.stepComplete[4] = true;
                        this.stepLoading[4] = false;
                    } else if (this.currentStep === 8) {
                        this.synthReplayDone = true;
                        this.synthReplayResult = msg;
                        this.stepComplete[8] = true;
                        this.stepLoading[8] = false;
                    }
                }),
            );
        },

        destroy() {
            this._wsUnsubs.forEach(fn => fn && fn());
            this._wsUnsubs = [];
        },

        // ── Navigation ──────────────────────────────────────────────
        nextStep() {
            if (this.currentStep < this.totalSteps && this.canAdvance()) {
                this.currentStep++;
            }
        },
        prevStep() {
            if (this.currentStep > 1) this.currentStep--;
        },
        goToStep(n) {
            if (n >= 1 && n <= this.totalSteps && (n <= this.currentStep || this.stepComplete[n - 1])) {
                this.currentStep = n;
            }
        },
        canAdvance() {
            return !!this.stepComplete[this.currentStep];
        },

        stepTitle(n) {
            const titles = {
                1: 'Live Traffic',
                2: 'Capture Complete',
                3: 'Compile',
                4: 'Restore & Replay',
                5: 'Compare',
                6: 'Drift Check',
                7: 'Synthesize',
                8: 'Benchmark',
            };
            return titles[n] || '';
        },
        stepDesc(n) {
            const descs = {
                1: 'See the proxy running and start capturing live traffic',
                2: 'Stop capture and inspect the recorded workload',
                3: 'Pre-resolve IDs for deterministic replay',
                4: 'Replay the workload against your target database',
                5: 'Compare source vs. replay performance',
                6: 'Verify data integrity between databases',
                7: 'Create a reusable synthetic benchmark',
                8: 'Run the synthetic workload and compare results',
            };
            return descs[n] || '';
        },
        stepIcon(n) {
            const icons = {
                1: '<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>',
                2: '<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/></svg>',
                3: '<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/><path d="M2 12l10 5 10-5"/></svg>',
                4: '<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="5 3 19 12 5 21 5 3"/></svg>',
                5: '<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="20" x2="18" y2="10"/><line x1="12" y1="20" x2="12" y2="4"/><line x1="6" y1="20" x2="6" y2="14"/></svg>',
                6: '<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 12h4l3-9 4 18 3-9h4"/></svg>',
                7: '<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>',
                8: '<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polygon points="10 8 16 12 10 16 10 8"/></svg>',
            };
            return icons[n] || '';
        },

        // ── Step 1: Live Traffic ─────────────────────────────────────
        async checkProxy() {
            const res = await api.proxyStatus();
            this.proxyStatus = res;
            if (res.running && res.capturing) {
                this.captureRunning = true;
            }
        },
        async startProxy() {
            this.stepLoading[1] = true;
            this.stepErrors[1] = null;
            const config = {};
            if (this.targetConn) config.target = this.targetConn;
            const res = await api.startProxy(config);
            if (res.error) {
                this.stepErrors[1] = res.error;
                this.stepLoading[1] = false;
                return;
            }
            this.proxyStatus = { running: true };
            this.stepLoading[1] = false;
        },
        async startCapture() {
            this.stepLoading[1] = true;
            this.stepErrors[1] = null;
            const res = await api.toggleCapture();
            if (res.error) {
                this.stepErrors[1] = res.error;
                this.stepLoading[1] = false;
                return;
            }
            this.captureRunning = true;
            this.captureQueryCount = 0;
            this.stepComplete[1] = true;
            this.stepLoading[1] = false;
        },

        // ── Step 2: Capture Complete ─────────────────────────────────
        async stopCapture() {
            this.stepLoading[2] = true;
            this.stepErrors[2] = null;

            // Toggle capture off
            const toggleRes = await api.toggleCapture();
            if (toggleRes.error) {
                this.stepErrors[2] = toggleRes.error;
                this.stepLoading[2] = false;
                return;
            }
            this.captureRunning = false;

            // Stop the proxy (this finalizes the workload)
            const stopRes = await api.stopProxy();
            if (stopRes.error) {
                this.stepErrors[2] = stopRes.error;
                this.stepLoading[2] = false;
                return;
            }

            // Get the workload ID from proxy sessions or workloads list
            const wklRes = await api.listWorkloads();
            if (wklRes.error) {
                this.stepErrors[2] = wklRes.error;
                this.stepLoading[2] = false;
                return;
            }
            const workloads = wklRes.workloads || wklRes || [];
            if (workloads.length > 0) {
                // Pick the most recent workload
                const latest = workloads[workloads.length - 1];
                this.captureWorkloadId = latest.id || latest.name;

                // Inspect it
                const inspRes = await api.inspectWorkload(this.captureWorkloadId);
                if (!inspRes.error) {
                    this.captureInspect = inspRes;
                    this.restorePoint = inspRes.metadata?.capture_start || '';
                }
            }

            this.stepComplete[2] = true;
            this.stepLoading[2] = false;
        },

        // ── Step 3: Compile ──────────────────────────────────────────
        async compileWorkload() {
            if (!this.captureWorkloadId) {
                this.stepErrors[3] = 'No workload to compile. Complete step 2 first.';
                return;
            }
            this.stepLoading[3] = true;
            this.stepErrors[3] = null;

            const res = await api.compileWorkload(this.captureWorkloadId);
            if (res.error) {
                this.stepErrors[3] = res.error;
                this.stepLoading[3] = false;
                return;
            }

            this.compiledWorkloadId = res.compiled_id || res.workload_id || this.captureWorkloadId;
            this.compileStats = res;
            this.stepComplete[3] = true;
            this.stepLoading[3] = false;
        },

        // ── Step 4: Restore & Replay ─────────────────────────────────
        async startReplay() {
            const wid = this.compiledWorkloadId || this.captureWorkloadId;
            if (!wid) {
                this.stepErrors[4] = 'No workload available. Complete previous steps first.';
                return;
            }
            if (!this.targetConn) {
                this.stepErrors[4] = 'Target connection string is required.';
                return;
            }
            this.stepLoading[4] = true;
            this.stepErrors[4] = null;
            this.replayDone = false;
            this.replayProgress = 0;

            const res = await api.startReplay({
                workload_id: wid,
                target: this.targetConn,
                mode: 'read-write',
            });
            if (res.error) {
                this.stepErrors[4] = res.error;
                this.stepLoading[4] = false;
                return;
            }
            this.replayRunId = res.run_id || res.id;
            // Progress tracked via WS — stepComplete[4] set when ReplayCompleted fires
        },

        // ── Step 5: Compare ──────────────────────────────────────────
        async computeCompare() {
            if (!this.replayRunId) {
                this.stepErrors[5] = 'No replay run available. Complete step 4 first.';
                return;
            }
            this.stepLoading[5] = true;
            this.stepErrors[5] = null;

            const wid = this.compiledWorkloadId || this.captureWorkloadId;
            const res = await api.computeCompare({
                workload_id: wid,
                run_id: this.replayRunId,
            });
            if (res.error) {
                this.stepErrors[5] = res.error;
                this.stepLoading[5] = false;
                return;
            }

            this.compareReport = res;
            this.stepComplete[5] = true;
            this.stepLoading[5] = false;
        },

        // ── Step 6: Drift Check ──────────────────────────────────────
        async runDriftCheck() {
            if (!this.driftDbA || !this.driftDbB) {
                this.stepErrors[6] = 'Both DB-A and DB-B connection strings are required.';
                return;
            }
            this.stepLoading[6] = true;
            this.stepErrors[6] = null;

            const res = await api.driftCheck({ db_a: this.driftDbA, db_b: this.driftDbB });
            if (res.error) {
                this.stepErrors[6] = res.error;
                this.stepLoading[6] = false;
                return;
            }

            this.driftResults = res;
            this.stepComplete[6] = true;
            this.stepLoading[6] = false;
        },

        // ── Step 7: Synthesize ───────────────────────────────────────
        async synthesize() {
            const wid = this.compiledWorkloadId || this.captureWorkloadId;
            if (!wid) {
                this.stepErrors[7] = 'No workload available. Complete previous steps first.';
                return;
            }
            this.stepLoading[7] = true;
            this.stepErrors[7] = null;

            const config = {};
            if (this.sourceConn) config.source_conn = this.sourceConn;
            if (this.synthSessions) config.sessions = parseInt(this.synthSessions) || undefined;
            if (this.synthScale) config.scale = parseFloat(this.synthScale) || undefined;

            const res = await api.synthesizeWorkload(wid, config);
            if (res.error) {
                this.stepErrors[7] = res.error;
                this.stepLoading[7] = false;
                return;
            }

            this.syntheticWorkloadId = res.workload_id || res.id;
            this.syntheticDataPath = res.data_sql_path || res.data_path || '';
            this.syntheticStats = res;
            this.stepComplete[7] = true;
            this.stepLoading[7] = false;
        },

        // ── Step 8: Benchmark ────────────────────────────────────────
        async runSyntheticReplay() {
            if (!this.syntheticWorkloadId) {
                this.stepErrors[8] = 'No synthetic workload. Complete step 7 first.';
                return;
            }
            if (!this.targetConn) {
                this.stepErrors[8] = 'Target connection string is required.';
                return;
            }
            this.stepLoading[8] = true;
            this.stepErrors[8] = null;
            this.synthReplayDone = false;

            const res = await api.startReplay({
                workload_id: this.syntheticWorkloadId,
                target: this.targetConn,
                mode: 'read-write',
            });
            if (res.error) {
                this.stepErrors[8] = res.error;
                this.stepLoading[8] = false;
                return;
            }
            this.synthReplayRunId = res.run_id || res.id;
            // Wait for completion via WS, then auto-compare
        },

        async computeSynthCompare() {
            if (!this.synthReplayRunId || !this.syntheticWorkloadId) return;
            this.stepLoading[8] = true;

            const res = await api.computeCompare({
                workload_id: this.syntheticWorkloadId,
                run_id: this.synthReplayRunId,
            });
            if (!res.error) {
                this.synthCompareReport = res;
                this.stepComplete[8] = true;
            } else {
                this.stepErrors[8] = res.error;
            }
            this.stepLoading[8] = false;
        },

        // ── Helpers ──────────────────────────────────────────────────
        escapeHtml(str) {
            if (!str) return '';
            const s = String(str);
            return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
        },

        fmtDuration(us) {
            if (!us) return '-';
            if (us < 1000) return us + ' us';
            if (us < 1000000) return (us / 1000).toFixed(1) + ' ms';
            return (us / 1000000).toFixed(2) + ' s';
        },

        fmtPct(val) {
            if (val == null) return '-';
            const sign = val > 0 ? '+' : '';
            return sign + val.toFixed(1) + '%';
        },

        pctColor(val) {
            if (val == null) return 'text-slate-400';
            if (val > 10) return 'text-danger';
            if (val > 0) return 'text-amber-400';
            return 'text-accent';
        },
    };
}

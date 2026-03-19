// Main app — Alpine.js router + global state

// Toast notification system
function toasts() {
    return {
        items: [],
        add(message, type = 'info') {
            const id = Date.now();
            this.items.push({ id, message, type });
            setTimeout(() => {
                this.items = this.items.filter(t => t.id !== id);
            }, 4000);
        },
    };
}

// Global toast helper
window.showToast = function(message, type = 'info') {
    const toastData = Alpine.$data(document.getElementById('toast-container'));
    if (toastData) toastData.add(message, type);
};

// Main app component
function app() {
    return {
        page: 'dashboard',
        version: '',
        wsConnected: false,
        activeTasks: [],
        sidebarCollapsed: false,
        demoEnabled: false,

        navItems: [
            { id: 'dashboard', label: 'Dashboard', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/></svg>' },
            { id: 'demo', label: 'Demo', icon: '<svg xmlns="http://www.w3.org/2000/svg" class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="5 3 19 12 5 21 5 3"></polygon></svg>' },
            { id: 'workloads', label: 'Workloads', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>' },
            { id: 'proxy', label: 'Proxy', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>' },
            { id: 'replay', label: 'Replay', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="5 3 19 12 5 21 5 3"/></svg>' },
            { id: 'ab', label: 'A/B Testing', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="20" x2="18" y2="10"/><line x1="12" y1="20" x2="12" y2="4"/><line x1="6" y1="20" x2="6" y2="14"/></svg>' },
            { id: 'compare', label: 'Compare', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M16 3h5v5"/><path d="M8 3H3v5"/><path d="M12 22V8"/><path d="m3 3 5 5"/><path d="m21 3-5 5"/></svg>' },
            { id: 'pipeline', label: 'Pipeline', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/></svg>' },
            { id: 'transform', label: 'Transform', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"/></svg>' },
            { id: 'tuning', label: 'Tuning', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>' },
            { id: 'history', label: 'History', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>' },
            { id: 'help', label: 'Help', icon: '<svg class="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>' },
        ],

        get currentPageLabel() {
            const item = this.navItems.find(i => i.id === this.page);
            return item ? item.label : '';
        },

        async init() {
            // Hash-based routing
            this.page = location.hash.replace('#', '') || 'dashboard';
            window.addEventListener('hashchange', () => {
                this.page = location.hash.replace('#', '') || 'dashboard';
            });

            // Store reference for Alpine.js access
            Alpine.store('app', this);

            // Fetch health for version
            const health = await api.health();
            this.version = health.version || '?';

            // Connect WebSocket
            wsClient.connect();
            wsClient.on('_connected', (connected) => {
                this.wsConnected = connected;
            });

            // Check demo mode
            api.get('/demo/config').then(res => {
                this.demoEnabled = res.enabled || false;
            }).catch(() => {
                this.demoEnabled = false;
            });

            // Poll active tasks
            this.pollTasks();
        },

        async pollTasks() {
            const res = await api.tasks();
            this.activeTasks = (res.tasks || []).filter(t => t.running);
            setTimeout(() => this.pollTasks(), 5000);
        },
    };
}

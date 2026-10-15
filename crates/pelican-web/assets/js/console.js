// Server console: xterm.js terminal on the Wings websocket, power actions,
// command input with history, live stats and charts
// (resources/views/filament/components/server-console.blade.php of the original).
(function () {
  'use strict';

  function start() {
    const cfgEl = document.getElementById('console-config');
    if (!cfgEl || !window.Terminal) return;
    const cfg = JSON.parse(cfgEl.textContent);

    // ----- terminal -----
    const theme = {
      background: 'rgba(19,26,32,0.7)', cursor: 'transparent', black: '#000000', red: '#E54B4B', green: '#9ECE58',
      yellow: '#FAED70', blue: '#396FE2', magenta: '#BB80B3', cyan: '#2DDAFD', white: '#d0d0d0',
      brightBlack: 'rgba(255, 255, 255, 0.2)', brightRed: '#FF5370', brightGreen: '#C3E88D', brightYellow: '#FFCB6B',
      brightBlue: '#82AAFF', brightMagenta: '#C792EA', brightCyan: '#89DDFF', brightWhite: '#ffffff', selectionBackground: '#FAF089',
    };
    const term = new window.Terminal({
      fontSize: cfg.fontSize, fontFamily: cfg.font + ', monospace', lineHeight: 1.2, disableStdin: true,
      cursorStyle: 'underline', cursorInactiveStyle: 'underline', allowTransparency: true, rows: cfg.rows, theme,
    });
    const fit = new window.FitAddon.FitAddon();
    const search = new window.SearchAddon.SearchAddon();
    term.loadAddon(fit);
    term.loadAddon(new window.WebLinksAddon.WebLinksAddon());
    term.loadAddon(search);
    term.open(document.getElementById('terminal'));
    fit.fit();
    window.addEventListener('resize', () => fit.fit());
    term.attachCustomKeyEventHandler((e) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'c' && e.type === 'keydown') {
        navigator.clipboard?.writeText(term.getSelection());
        return false;
      }
      if ((e.ctrlKey || e.metaKey) && e.key === 'f' && e.type === 'keydown') {
        e.preventDefault();
        const q = window.prompt('Search');
        if (q) search.findNext(q);
        return false;
      }
      return true;
    });

    const PRELUDE = '\u001b[1m\u001b[33m' + cfg.prelude + '@' + cfg.name + ' ~ \u001b[0m';
    const strip = (l) => String(l).replace(/(?:\r\n|\r|\n)$/im, '');
    const out = (line, prelude) => term.writeln((prelude ? PRELUDE : '') + strip(line) + '\u001b[0m');

    // ----- stats -----
    let state = cfg.statusIsLive ? cfg.status : cfg.status;
    let statusIsLive = cfg.statusIsLive;
    const samples = [];
    let lastNet = null;
    const units = cfg.binaryPrefix ? ['Bytes', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] : ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB'];
    const base = cfg.binaryPrefix ? 1024 : 1000;
    function bytes(n) {
      if (!n || n <= 0) return '0 ' + units[0];
      const e = Math.min(Math.floor(Math.log(n) / Math.log(base)), units.length - 1);
      return (n / Math.pow(base, e)).toFixed(2) + ' ' + units[e];
    }
    const setText = (id, v) => { const el = document.getElementById(id); if (el) el.textContent = v; };

    function render() {
      const latest = samples[samples.length - 1];
      if (latest) {
        setText('server-network-heading', '- ↓' + bytes(latest.rx) + ' - ↑' + bytes(latest.tx));
        setText('server-stat-disk', latest.disk === 0 ? cfg.unknown : bytes(latest.disk));
      }
      const usage = (id, fmt) => {
        const el = document.getElementById(id);
        if (!el) return;
        if (state === 'offline') { el.textContent = cfg.offlineLabel; return; }
        el.textContent = (state === null || !latest ? cfg.unknown : fmt(latest)) + (el.dataset.limit || '');
      };
      usage('server-stat-cpu', (s) => s.cpu.toFixed(2) + ' %');
      usage('server-stat-memory', (s) => bytes(s.memory));
      if (statusIsLive) setText('server-stat-status', state ? (cfg.statusLabels[state] || state) : cfg.unknown);
      chart('chart-cpu', [samples.map((s) => s.cpu)], (v) => v.toFixed(1) + ' %');
      chart('chart-memory', [samples.map((s) => s.memory)], bytes);
      chart('chart-network', [samples.map((s) => s.rx), samples.map((s) => s.tx)], bytes);
    }

    function chart(id, series, fmt) {
      const c = document.getElementById(id);
      if (!c) return;
      const dpr = window.devicePixelRatio || 1;
      const w = c.clientWidth, h = c.clientHeight;
      if (c.width !== w * dpr) { c.width = w * dpr; c.height = h * dpr; }
      const g = c.getContext('2d');
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
      g.clearRect(0, 0, w, h);
      const max = Math.max(1, ...series.flat());
      const style = getComputedStyle(document.documentElement);
      g.strokeStyle = style.getPropertyValue('--border') || 'rgba(128,128,128,.2)';
      g.lineWidth = 1;
      for (let i = 0; i <= 4; i++) { const y = Math.round((h - 1) * i / 4) + 0.5; g.beginPath(); g.moveTo(0, y); g.lineTo(w, y); g.stroke(); }
      const colors = ['#3b82f6', '#22c55e'];
      series.forEach((data, si) => {
        if (data.length < 2) return;
        g.strokeStyle = colors[si % colors.length];
        g.fillStyle = colors[si % colors.length] + '22';
        g.lineWidth = 2;
        g.beginPath();
        const step = w / Math.max(cfg.period - 1, 1);
        const x0 = w - (data.length - 1) * step;
        data.forEach((v, i) => { const x = x0 + i * step, y = h - 2 - (v / max) * (h - 6); if (i === 0) g.moveTo(x, y); else g.lineTo(x, y); });
        g.stroke();
        g.lineTo(w, h); g.lineTo(x0, h); g.closePath(); g.fill();
      });
      g.fillStyle = style.getPropertyValue('--text-2') || '#888';
      g.font = '11px ' + (style.getPropertyValue('--font') || 'sans-serif');
      g.fillText(fmt(max), 4, 12);
    }

    function pushStats(raw) {
      let s;
      try { s = typeof raw === 'string' ? JSON.parse(raw) : raw; } catch (_) { return; }
      const net = s.network || {};
      let rx = 0, tx = 0;
      if (lastNet) { rx = Math.max(0, (net.rx_bytes || 0) - lastNet.rx); tx = Math.max(0, (net.tx_bytes || 0) - lastNet.tx); }
      lastNet = { rx: net.rx_bytes || 0, tx: net.tx_bytes || 0 };
      samples.push({ cpu: s.cpu_absolute || 0, memory: s.memory_bytes || 0, disk: s.disk_bytes || 0, rx, tx });
      while (samples.length > cfg.period) samples.shift();
      if (s.state) setState(s.state, false);
      render();
    }

    // ----- power buttons & command input -----
    const startable = (s) => !['running', 'starting', 'stopping', 'restarting', 'missing'].includes(s);
    const offline = (s) => ['offline', 'missing'].includes(s);
    const restartable = (s) => startable(s) || !offline(s);
    const stoppable = (s) => !['starting', 'stopping', 'restarting', 'exited', 'offline', 'missing'].includes(s);
    const killable = (s) => !['offline', 'running', 'exited', 'missing'].includes(s);
    const btn = (n) => document.querySelector('[data-power="' + n + '"]');
    const input = document.getElementById('send-command');

    function setState(s, announce) {
      if (announce) out('Server marked as ' + s + '...', true);
      state = s;
      statusIsLive = true;
      const b = { start: btn('start'), restart: btn('restart'), stop: btn('stop'), kill: btn('kill') };
      if (b.start) b.start.disabled = cfg.conflict || !startable(s);
      if (b.restart) b.restart.disabled = cfg.conflict || !restartable(s);
      if (b.stop) { b.stop.hidden = killable(s); b.stop.disabled = cfg.conflict || !stoppable(s); }
      if (b.kill) { b.kill.hidden = !killable(s); b.kill.disabled = cfg.conflict || !killable(s); }
      if (input) {
        const can = cfg.canCommand && !cfg.conflict && ['starting', 'running'].includes(s);
        input.readOnly = !can;
        input.placeholder = can ? cfg.strings.command : cfg.strings.commandBlocked;
        input.title = can ? '' : cfg.strings.commandBlockedTitle;
      }
    }
    setState(state, false);

    document.querySelectorAll('[data-power]').forEach((b) => b.addEventListener('click', () => {
      const action = b.dataset.power;
      const go = () => sendAction({ event: 'set state', args: [action] });
      if (action === 'kill') {
        if (window.confirm(b.title || 'Kill?')) go();
      } else go();
    }));

    const histKey = 'console-history-' + cfg.uuid;
    let history = [];
    try { history = JSON.parse(sessionStorage.getItem(histKey) || '[]'); } catch (_) { history = []; }
    let hIndex = -1;
    input?.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        const v = input.value;
        if (v && !input.readOnly) {
          sendAction({ event: 'send command', args: [v] });
          history.unshift(v);
          history = history.slice(0, 50);
          try { sessionStorage.setItem(histKey, JSON.stringify(history)); } catch (_) { /* ignore */ }
          hIndex = -1;
          input.value = '';
        }
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        hIndex = Math.min(hIndex + 1, history.length - 1);
        input.value = history[hIndex] || '';
      } else if (e.key === 'ArrowDown') {
        hIndex = Math.max(hIndex - 1, -1);
        input.value = history[hIndex] || '';
      }
    });

    // ----- feature modals (`Console::mountFeature()`) -----
    const featureUrl = (key) => cfg.websocket.replace(/\/websocket$/, '') + '/feature/' + encodeURIComponent(key);

    // The loaded fragment replaces an already open modal (replaceMountedAction).
    function installModal(html) {
      const tpl = document.createElement('template');
      tpl.innerHTML = html.trim();
      const dialog = tpl.content.querySelector('dialog');
      if (!dialog) return;
      document.getElementById('feature-modal')?.remove();
      document.body.appendChild(dialog);
      window.openModal('feature-modal');
    }

    async function mountFeature(data) {
      let d;
      try { d = typeof data === 'string' ? JSON.parse(data) : data; } catch (_) { return; }
      const key = d && d.key;
      if (!key) return;
      try {
        const r = await fetch(featureUrl(key), { credentials: 'same-origin', headers: { Accept: 'text/html' } });
        if (r.ok) installModal(await r.text());
      } catch (_) { /* offline: nothing to mount */ }
    }

    // One delegated submit handler for every feature modal (guarded so htmx
    // re-navigations don't stack listeners).
    if (!window.__featureFormSubmit) {
      window.__featureFormSubmit = true;
      document.addEventListener('submit', async (e) => {
        const form = e.target.closest('[data-feature-form]');
        if (!form) return;
        e.preventDefault();
        try {
          const r = await fetch(form.action, {
            method: 'POST', body: new URLSearchParams(new FormData(form)), credentials: 'same-origin',
            headers: {
              Accept: 'application/json',
              'X-CSRF-TOKEN': document.querySelector('meta[name="csrf-token"]')?.content || '',
            },
          });
          if ((r.headers.get('Content-Type') || '').includes('application/json')) {
            const n = await r.json();
            if (n.title) window.notify?.(n);
            closeModalNow();
          } else if (r.ok) {
            installModal(await r.text());
          }
        } catch (_) { /* network error: leave the modal open */ }
      });
    }
    function closeModalNow() {
      document.getElementById('feature-modal')?.close();
      document.getElementById('feature-modal')?.remove();
    }

    // ----- websocket -----
    let socket = null, authenticated = false, attempts = 0, tornDown = false;
    const pending = [];
    const send = (p) => { if (socket && socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(p)); };
    function sendAction(p) { if (authenticated && socket.readyState === WebSocket.OPEN) send(p); else pending.push(p); }

    async function credentials() {
      const r = await fetch(cfg.websocket, { headers: { Accept: 'application/json' }, credentials: 'same-origin' });
      if (!r.ok) throw new Error('websocket credentials: ' + r.status);
      return r.json();
    }

    async function authenticate() {
      try { const c = await credentials(); send({ event: 'auth', args: [c.token] }); } catch (e) { console.error(e); }
    }

    async function connect() {
      let c;
      try { c = await credentials(); } catch (e) { console.error(e); return failed(); }
      socket = new WebSocket(c.socket);
      socket.onopen = () => send({ event: 'auth', args: [c.token] });
      socket.onclose = () => {
        authenticated = false;
        if (tornDown) return;
        failed();
      };
      socket.onmessage = (m) => {
        let msg;
        try { msg = JSON.parse(m.data); } catch (_) { return; }
        const args = msg.args || [];
        switch (msg.event) {
          case 'console output':
          case 'install output':
            out(args[0], false);
            break;
          case 'install completed':
            setTimeout(() => window.location.reload(), 1000);
            break;
          case 'status':
            setState(args[0], true);
            render();
            break;
          case 'transfer status':
            if (args[0] === 'failure') out('Transfer has failed.', true);
            break;
          case 'daemon error':
            term.writeln(PRELUDE + '\u001b[1m\u001b[41m' + strip(args[0]) + '\u001b[0m');
            break;
          case 'stats':
            pushStats(args[0]);
            break;
          case 'feature match':
            mountFeature(args[0]);
            break;
          case 'auth success':
            attempts = 0;
            authenticated = true;
            send({ event: 'send logs', args: [null] });
            pending.splice(0).forEach(send);
            break;
          case 'token expiring':
          case 'token expired':
            authenticate();
            break;
          case 'jwt error':
            console.error('websocket jwt error', args[0]);
            break;
        }
      };
    }

    function failed() {
      if (attempts >= 5) {
        window.notify?.({ status: 'danger', title: cfg.strings.wsErrorTitle, body: cfg.strings.wsErrorBody });
        return;
      }
      attempts++;
      setTimeout(connect, 2000);
    }

    window.addEventListener('beforeunload', () => { tornDown = true; socket?.close(); });
    connect();
    render();
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start); else start();
})();

// Node overview charts (NodeCpuChart / NodeMemoryChart / NodeStorageChart): poll Node::statistics()
// every 5 s. Like the widgets: CPU = cpu_percent × threads of threads × 100 %, memory in GB (GiB with
// the binary prefix), storage as convert_bytes_to_readable() in a used/unused pie.
(function () {
  'use strict';
  function start() {
    const box = document.getElementById('node-charts');
    if (!box) return;
    const strings = JSON.parse(document.getElementById('node-strings')?.textContent || '{}');
    const threads = parseInt(box.dataset.cpuCount || '0', 10) || 0;
    const binary = box.dataset.binary === '1';
    const locale = (box.dataset.locale || 'en').replace('_', '-');
    const num = (n, opts) => { try { return Number(n).toLocaleString(locale, opts); } catch (_) { return Number(n).toLocaleString('en', opts); } };
    const fmt = (n) => num(n, { maximumFractionDigits: 2 });
    const unit = binary ? 1024 : 1000;
    const gb = (b) => Math.round((b || 0) / unit / unit / unit * 100) / 100;
    const gbLabel = binary ? ' GiB' : ' GB';
    // convert_bytes_to_readable()
    const readable = (b) => {
      const suffix = binary ? ['Bytes', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] : ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB'];
      if (!b || b <= 0) return '0 ' + suffix[0];
      const from = Math.log(b) / Math.log(unit);
      const base = Math.min(Math.floor(from), suffix.length - 1);
      return num(Math.pow(unit, from - base), { minimumFractionDigits: 2, maximumFractionDigits: 2 }) + ' ' + suffix[base];
    };
    const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    const cpu = [], mem = [], times = [];

    function canvas(id) {
      const c = document.getElementById(id); if (!c) return null;
      const dpr = window.devicePixelRatio || 1, w = c.clientWidth, h = c.clientHeight;
      if (c.width !== w * dpr || c.height !== h * dpr) { c.width = w * dpr; c.height = h * dpr; }
      const g = c.getContext('2d'); g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, w, h);
      g.font = '11px ' + (css('--font') || 'sans-serif');
      return { g, w, h };
    }
    const line = (id, data) => window.PelicanChart?.line(document.getElementById(id), { series: [{ data }], labels: times, showX: true });
    // Chart.js pie with its legend on top: used (blue) and unused (green).
    function pie(id, used, unused) {
      const c = canvas(id); if (!c) return;
      const { g, w, h } = c;
      const labels = [strings.used || 'Used', strings.unused || 'Unused'], colors = ['rgb(59, 130, 246)', 'rgb(74, 222, 128)'];
      g.textBaseline = 'middle'; g.textAlign = 'left';
      const widths = labels.map((l) => g.measureText(l).width + 50);
      let lx = (w - widths.reduce((a, b) => a + b, 0)) / 2;
      labels.forEach((l, i) => { g.fillStyle = colors[i]; g.fillRect(lx, 6, 40, 12); g.fillStyle = css('--text-2'); g.fillText(l, lx + 46, 12); lx += widths[i]; });
      const total = used + unused; if (total <= 0) return;
      const r = Math.max(0, Math.min(w, h - 34) / 2 - 4), cx = w / 2, cy = 30 + (h - 30) / 2;
      let a = -Math.PI / 2;
      [used, unused].forEach((v, i) => {
        const b = a + (v / total) * Math.PI * 2;
        g.beginPath(); g.moveTo(cx, cy); g.arc(cx, cy, r, a, b); g.closePath();
        g.fillStyle = colors[i]; g.fill(); g.strokeStyle = css('--surface'); g.lineWidth = 2; g.stroke(); a = b;
      });
    }
    const head = (k, text) => { const el = box.querySelector('[data-heading="' + k + '"]'); if (el) el.textContent = text; };
    async function tick() {
      let s;
      try {
        const r = await fetch(box.dataset.stats, { credentials: 'same-origin' });
        s = await r.json();
      } catch (_) { return; /* panel unreachable */ }
      const cpuNow = Math.round((s.cpu_percent || 0) * threads * 100) / 100;
      cpu.push(cpuNow); mem.push(gb(s.memory_used));
      const d = new Date(); times.push([d.getHours(), d.getMinutes(), d.getSeconds()].map((n) => String(n).padStart(2, '0')).join(':'));
      if (cpu.length > 60) { cpu.shift(); mem.shift(); times.shift(); }
      line('node-cpu', cpu); line('node-memory', mem);
      const total = (s.disk_total || 0), used = (s.disk_used || 0);
      pie('node-disk', Math.round(used / unit / unit / unit * 100) / 100, Math.round((total - used) / unit / unit / unit * 100) / 100);
      head('cpu', (strings.cpu || 'CPU').replace(':cpu', fmt(cpuNow)).replace(':max', fmt(threads * 100)));
      head('memory', (strings.memory || 'Memory').replace(':used', fmt(gb(s.memory_used)) + gbLabel)
        .replace(':total', fmt((s.memory_total || 0) / unit / unit / unit) + gbLabel));
      head('disk', (strings.disk || 'Storage').replace(':used', readable(used)).replace(':total', readable(total)));
    }
    tick();
    setInterval(tick, 5000);
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start); else start();
})();

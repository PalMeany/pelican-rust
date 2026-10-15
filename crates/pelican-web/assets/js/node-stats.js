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
    // A Chart.js-like filled line: y from 0 with ~10 ticks, the sample times under the x axis.
    function line(id, data) {
      const c = canvas(id); if (!c) return;
      const { g, w, h } = c;
      const top = Math.max(...data, 0) || 1;
      const step = Math.pow(10, Math.floor(Math.log10(top / 10)));
      const tick = [1, 2, 2.5, 5, 10].map((m) => m * step).find((t) => top / t <= 10) || step * 10;
      const max = Math.ceil(top / tick) * tick;
      // Chart.js formats ticks in the browser's locale with the step's decimals.
      const digits = Math.max(0, Math.min(20, -Math.floor(Math.log10(tick))));
      const tickLabel = (v) => Number(v).toLocaleString(undefined, { minimumFractionDigits: digits, maximumFractionDigits: digits });
      const left = 34, bottom = 40, right = 8, topPad = 8;
      const ph = h - bottom - topPad, pw = w - left - right;
      g.strokeStyle = css('--border'); g.fillStyle = css('--text-2'); g.lineWidth = 1; g.textAlign = 'right'; g.textBaseline = 'middle';
      for (let v = 0; v <= max + tick / 2; v += tick) {
        const y = topPad + ph - (v / max) * ph;
        g.beginPath(); g.moveTo(left, y); g.lineTo(w - right, y); g.stroke();
        g.fillText(tickLabel(v), left - 6, y);
      }
      if (!data.length) return;
      const x = (i) => left + (data.length === 1 ? pw / 2 : (i / (data.length - 1)) * pw);
      const y = (v) => topPad + ph - (v / max) * ph;
      g.save(); g.textAlign = 'right'; g.textBaseline = 'middle';
      const every = Math.max(1, Math.ceil(data.length / Math.max(1, Math.floor(pw / 24))));
      times.forEach((t, i) => { if (i % every) return; g.save(); g.translate(x(i), topPad + ph + 6); g.rotate(-Math.PI / 4); g.fillText(t, 0, 0); g.restore(); });
      g.restore();
      g.beginPath(); data.forEach((v, i) => { if (i === 0) g.moveTo(x(i), y(v)); else g.lineTo(x(i), y(v)); });
      g.strokeStyle = 'rgba(54, 162, 235, 1)'; g.lineWidth = 2; g.stroke();
      g.lineTo(x(data.length - 1), y(0)); g.lineTo(x(0), y(0)); g.closePath();
      g.fillStyle = 'rgba(96, 165, 250, 0.3)'; g.fill();
      g.fillStyle = 'rgba(54, 162, 235, 1)';
      data.forEach((v, i) => { g.beginPath(); g.arc(x(i), y(v), 2.5, 0, Math.PI * 2); g.fill(); });
    }
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

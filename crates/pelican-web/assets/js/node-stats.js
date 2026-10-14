// Node overview charts (NodeCpuChart / NodeMemoryChart / NodeStorageChart): poll Node::statistics().
(function () {
  'use strict';
  function start() {
    const box = document.getElementById('node-charts');
    if (!box) return;
    const strings = JSON.parse(document.getElementById('node-strings')?.textContent || '{}');
    const cpuCount = parseInt(box.dataset.cpuCount || '0', 10) || 1;
    const cpu = [], mem = [];
    const units = ['Bytes', 'KiB', 'MiB', 'GiB', 'TiB'];
    const bytes = (n) => { if (!n) return '0 Bytes'; const e = Math.min(Math.floor(Math.log(n) / Math.log(1024)), 4); return (n / Math.pow(1024, e)).toFixed(2) + ' ' + units[e]; };
    function draw(id, data, max) {
      const c = document.getElementById(id); if (!c) return;
      const dpr = window.devicePixelRatio || 1, w = c.clientWidth, h = c.clientHeight;
      if (c.width !== w * dpr) { c.width = w * dpr; c.height = h * dpr; }
      const g = c.getContext('2d'); g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, w, h);
      if (data.length < 2) return;
      const top = Math.max(max || 1, ...data);
      g.strokeStyle = '#3b82f6'; g.fillStyle = '#3b82f622'; g.lineWidth = 2; g.beginPath();
      const step = w / 49;
      data.forEach((v, i) => { const x = w - (data.length - 1 - i) * step, y = h - 2 - (v / top) * (h - 6); if (i === 0) g.moveTo(x, y); else g.lineTo(x, y); });
      g.stroke(); g.lineTo(w, h); g.lineTo(w - (data.length - 1) * step, h); g.closePath(); g.fill();
    }
    const head = (k, text) => { const el = box.querySelector('[data-heading="' + k + '"]'); if (el) el.textContent = text; };
    async function tick() {
      try {
        const r = await fetch(box.dataset.stats, { credentials: 'same-origin' });
        const s = await r.json();
        cpu.push(s.cpu_percent || 0); mem.push(s.memory_used || 0);
        if (cpu.length > 50) cpu.shift(); if (mem.length > 50) mem.shift();
        draw('node-cpu', cpu, cpuCount * 100);
        draw('node-memory', mem, s.memory_total || 0);
        head('cpu', (strings.cpu || 'CPU').replace(':cpu', (s.cpu_percent || 0).toFixed(2)).replace(':max', cpuCount * 100));
        head('memory', (strings.memory || 'Memory').replace(':used', bytes(s.memory_used)).replace(':total', bytes(s.memory_total)));
        head('disk', (strings.disk || 'Disk').replace(':used', bytes(s.disk_used)).replace(':total', bytes(s.disk_total)));
        const d = document.getElementById('node-disk');
        if (d && s.disk_total) d.style.width = (s.disk_used / s.disk_total * 100).toFixed(1) + '%';
      } catch (_) { /* node unreachable */ }
    }
    tick();
    setInterval(tick, 5000);
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start); else start();
})();

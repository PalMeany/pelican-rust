// pelican-rs web UI glue: CSRF for htmx, toasts, modals, confirmations,
// theme switching, copy-to-clipboard, clickable rows.
(function () {
  'use strict';

  const csrf = () => document.querySelector('meta[name="csrf-token"]')?.content || '';
  const strings = () => window.PELICAN_STRINGS || {};

  // ----- theme -----
  function applyTheme(t) {
    if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t;
    else delete document.documentElement.dataset.theme;
  }
  // The initial theme (the stored choice, else the panel's default mode) is applied by the
  // inline script in the page head before the first paint.
  window.setTheme = function (t) {
    try { localStorage.setItem('theme', t); } catch (_) { /* ignore */ }
    applyTheme(t);
  };

  // ----- toasts -----
  const ICONS = {
    success: '<path d="M12 12m-9 0a9 9 0 1 0 18 0a9 9 0 1 0 -18 0"/><path d="M9 12l2 2l4 -4"/>',
    danger: '<path d="M12 12m-9 0a9 9 0 1 0 18 0a9 9 0 1 0 -18 0"/><path d="M10 10l4 4m0 -4l-4 4"/>',
    warning: '<path d="M12 9v4"/><path d="M10.363 3.591l-8.106 13.534a1.914 1.914 0 0 0 1.636 2.871h16.214a1.914 1.914 0 0 0 1.636 -2.87l-8.106 -13.536a1.914 1.914 0 0 0 -3.274 0z"/><path d="M12 16h.01"/>',
    info: '<path d="M3 12a9 9 0 1 0 18 0a9 9 0 0 0 -18 0"/><path d="M12 9h.01"/><path d="M11 12h1v4h1"/>',
  };
  function toastBox() {
    let box = document.getElementById('toasts');
    if (!box) {
      box = document.createElement('div');
      box.id = 'toasts';
      box.className = 'toasts';
      box.setAttribute('aria-live', 'polite');
      document.body.appendChild(box);
    }
    return box;
  }
  function notify(n) {
    const status = n.status || 'info';
    const el = document.createElement('div');
    el.className = 'toast ' + status;
    el.innerHTML = '<svg class="icon-lg t-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' +
      (ICONS[status] || ICONS.info) + '</svg><div class="t-text"><div class="t-title"></div><div class="t-body"></div></div>' +
      '<button type="button" class="icon-btn" aria-label="Close"><svg class="icon-sm" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6l-12 12M6 6l12 12"/></svg></button>';
    el.querySelector('.t-title').textContent = n.title || '';
    const body = el.querySelector('.t-body');
    if (n.body) body.textContent = n.body; else body.remove();
    el.querySelector('button').onclick = () => el.remove();
    toastBox().appendChild(el);
    setTimeout(() => el.remove(), n.duration || 6000);
  }
  window.notify = notify;

  function flushFlash(root) {
    (root || document).querySelectorAll('script[type="application/json"][data-flash]').forEach((s) => {
      try { JSON.parse(s.textContent).forEach(notify); } catch (_) { /* ignore */ }
      s.remove();
    });
  }

  // ----- modals -----
  function openModal(id) {
    const d = document.getElementById(id);
    if (d && typeof d.showModal === 'function' && !d.open) d.showModal();
  }
  window.openModal = openModal;
  window.closeModal = (id) => document.getElementById(id)?.close();

  let confirmResolve = null;
  function confirmDialog(question, opts) {
    let d = document.getElementById('confirm-dialog');
    if (!d) return Promise.resolve(window.confirm(question));
    d.querySelector('[data-confirm-title]').textContent = opts.title || question;
    const body = d.querySelector('[data-confirm-body]');
    body.textContent = opts.title ? question : (strings().confirm_body || '');
    const ok = d.querySelector('[data-confirm-ok]');
    ok.textContent = opts.ok || strings().confirm || 'Confirm';
    ok.className = 'btn ' + (opts.color === 'gray' ? 'btn-gray' : opts.color === 'primary' ? '' : 'btn-danger');
    d.showModal();
    return new Promise((resolve) => { confirmResolve = resolve; });
  }
  document.addEventListener('click', (e) => {
    const t = e.target.closest('[data-confirm-ok],[data-confirm-cancel]');
    if (t && confirmResolve) {
      const r = confirmResolve; confirmResolve = null;
      document.getElementById('confirm-dialog')?.close();
      r(t.hasAttribute('data-confirm-ok'));
    }
  });
  document.addEventListener('close', (e) => {
    if (e.target.id === 'confirm-dialog' && confirmResolve) { const r = confirmResolve; confirmResolve = null; r(false); }
  }, true);

  // Plain forms / links with data-confirm.
  document.addEventListener('submit', (e) => {
    const f = e.target;
    // The submitting button may carry its own confirmation (bulk actions).
    const src = e.submitter && e.submitter.dataset.confirm ? e.submitter : f;
    if (!(f instanceof HTMLFormElement) || !src.dataset.confirm || f.dataset.confirmed || f.hasAttribute('hx-post')) return;
    e.preventDefault();
    confirmDialog(src.dataset.confirm, { title: src.dataset.confirmTitle, ok: src.dataset.confirmOk, color: src.dataset.confirmColor }).then((ok) => {
      if (ok) { f.dataset.confirmed = '1'; f.requestSubmit(e.submitter || undefined); }
    });
  }, true);

  // ----- htmx integration -----
  document.addEventListener('htmx:configRequest', (e) => { e.detail.headers['X-CSRF-TOKEN'] = csrf(); });
  document.addEventListener('htmx:confirm', (e) => {
    const q = e.detail.question;
    if (!q) return;
    e.preventDefault();
    const el = e.detail.elt;
    confirmDialog(q, { title: el.dataset.confirmTitle, ok: el.dataset.confirmOk, color: el.dataset.confirmColor }).then((ok) => { if (ok) e.detail.issueRequest(true); });
  });
  document.addEventListener('htmx:responseError', (e) => {
    const x = e.detail.xhr;
    const s = strings();
    if (x.status === 419) { notify({ status: 'danger', title: s.page_expired || 'Page Expired' }); return; }
    if (x.status === 429) { notify({ status: 'danger', title: s.too_many || 'Too Many Requests' }); return; }
    notify({ status: 'danger', title: (s.error || 'Error') + ' ' + x.status });
  });
  document.addEventListener('htmx:sendError', () => notify({ status: 'danger', title: strings().offline || 'Network error' }));
  document.addEventListener('notify', (e) => notify(e.detail || {}));
  document.addEventListener('close-modal', (e) => { if (e.detail?.id) window.closeModal(e.detail.id); else document.querySelectorAll('dialog[open]').forEach((d) => d.close()); });
  document.addEventListener('htmx:afterSettle', (e) => { flushFlash(e.detail.elt); });

  // Collapsible admin sidebar (sidebarCollapsibleOnDesktop): icons only, labels as tooltips.
  function sidebarTitles() {
    const on = document.documentElement.classList.contains('sidebar-collapsed');
    document.querySelectorAll('.shell.collapsible .sidebar .nav-item').forEach((a) => {
      if (on) a.title = a.querySelector('span')?.textContent.trim() || ''; else a.removeAttribute('title');
    });
  }
  sidebarTitles();
  document.addEventListener('click', (e) => {
    if (!e.target.closest('[data-sidebar-collapse]')) return;
    const on = document.documentElement.classList.toggle('sidebar-collapsed');
    try { localStorage.setItem('sidebar-collapsed', on ? '1' : '0'); } catch (_) { /* ignore */ }
    sidebarTitles();
  });

  // UploadIcon's file tab (server settings, admin server edit): the multipart form posts with an
  // X-CSRF-TOKEN header (the CSRF middleware only buffers urlencoded bodies).
  document.addEventListener('submit', function (e) {
    var form = e.target.closest('form[data-icon-upload]');
    if (!form) return;
    e.preventDefault();
    var token = form.querySelector('[name="_token"]');
    var errorEl = form.querySelector('[data-upload-error]');
    fetch(form.action, {
      method: 'POST',
      body: new FormData(form),
      credentials: 'same-origin',
      headers: { 'X-CSRF-TOKEN': token ? token.value : '' },
    })
      .then(function (r) { return r.json(); })
      .then(function (j) {
        if (j.ok) { window.location.reload(); return; }
        if (errorEl) { errorEl.hidden = false; errorEl.textContent = j.message || 'Upload failed'; }
      })
      .catch(function () {
        if (errorEl) { errorEl.hidden = false; errorEl.textContent = 'Upload failed'; }
      });
  });

  // ----- Chart.js-like line charts (Filament chart widgets: console, node overview) -----
  // series: [{ data, stroke, fill }]; the points span the width, y starts at 0 with about ten ticks
  // in the browser's locale (or `tick(v)`), x labels only with `labels` + `showX`; tension 0.3.
  window.PelicanChart = {
    line(canvas, opts) {
      if (!canvas) return;
      const dpr = window.devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
      if (canvas.width !== w * dpr || canvas.height !== h * dpr) { canvas.width = w * dpr; canvas.height = h * dpr; }
      const g = canvas.getContext('2d');
      g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, w, h);
      const css = (n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
      g.font = '11px ' + (css('--font') || 'sans-serif');
      const series = (opts.series || []).filter((s) => s && s.data);
      const top = Math.max(0, ...series.flatMap((s) => s.data)) || 1;
      const step = Math.pow(10, Math.floor(Math.log10(top / 10)));
      const tick = [1, 2, 2.5, 5, 10].map((m) => m * step).find((t) => top / t <= 10) || step * 10;
      const max = Math.ceil(top / tick - 1e-9) * tick;
      const digits = Math.max(0, Math.min(20, -Math.floor(Math.log10(tick))));
      const label = opts.tick || ((v) => Number(v).toLocaleString(undefined, { minimumFractionDigits: digits, maximumFractionDigits: digits }));
      const ticks = [];
      for (let v = 0; v <= max + tick / 2; v += tick) ticks.push(v);
      const left = 8 + Math.max(...ticks.map((v) => g.measureText(label(v)).width)), right = 8, topPad = 8;
      const bottom = opts.showX ? 40 : 6, ph = h - bottom - topPad, pw = w - left - right;
      const y = (v) => topPad + ph - (v / max) * ph;
      g.strokeStyle = css('--border'); g.fillStyle = css('--text-2'); g.lineWidth = 1; g.textAlign = 'right'; g.textBaseline = 'middle';
      ticks.forEach((v) => { g.beginPath(); g.moveTo(left, y(v)); g.lineTo(w - right, y(v)); g.stroke(); g.fillText(label(v), left - 6, y(v)); });
      const n = Math.max(0, ...series.map((s) => s.data.length));
      if (!n) return;
      const x = (i) => left + (n === 1 ? pw / 2 : (i / (n - 1)) * pw);
      if (opts.showX && opts.labels) {
        const every = Math.max(1, Math.ceil(n / Math.max(1, Math.floor(pw / 24))));
        opts.labels.forEach((t, i) => { if (i % every) return; g.save(); g.translate(x(i), topPad + ph + 6); g.rotate(-Math.PI / 4); g.fillText(t, 0, 0); g.restore(); });
      }
      series.forEach((s) => {
        const pts = s.data.map((v, i) => [x(i), y(v)]);
        const path = () => {
          g.moveTo(pts[0][0], pts[0][1]);
          for (let i = 1; i < pts.length; i++) {
            // Chart.js splineCurve, tension 0.3
            const cp = (k) => {
              const p0 = pts[Math.max(0, k - 1)], p1 = pts[k], p2 = pts[Math.min(pts.length - 1, k + 1)];
              const d01 = Math.hypot(p1[0] - p0[0], p1[1] - p0[1]), d12 = Math.hypot(p2[0] - p1[0], p2[1] - p1[1]);
              const fa = d01 + d12 ? 0.3 * d01 / (d01 + d12) : 0, fb = d01 + d12 ? 0.3 * d12 / (d01 + d12) : 0;
              return { prev: [p1[0] - fa * (p2[0] - p0[0]), p1[1] - fa * (p2[1] - p0[1])], next: [p1[0] + fb * (p2[0] - p0[0]), p1[1] + fb * (p2[1] - p0[1])] };
            };
            const a = cp(i - 1).next, b = cp(i).prev;
            g.bezierCurveTo(a[0], Math.min(a[1], topPad + ph), b[0], Math.min(b[1], topPad + ph), pts[i][0], pts[i][1]);
          }
        };
        g.beginPath(); path(); g.lineTo(pts[pts.length - 1][0], y(0)); g.lineTo(pts[0][0], y(0)); g.closePath();
        g.fillStyle = s.fill || 'rgba(96, 165, 250, 0.3)'; g.fill();
        g.beginPath(); path(); g.strokeStyle = s.stroke || 'rgba(54, 162, 235, 1)'; g.lineWidth = 2; g.stroke();
        g.fillStyle = s.stroke || 'rgba(54, 162, 235, 1)';
        if (pts.length <= 60) pts.forEach((p) => { g.beginPath(); g.arc(p[0], p[1], 2.5, 0, Math.PI * 2); g.fill(); });
      });
    },
  };

  // Reorderable key/value rows (KeyValue::reorderable()): drag a row by its handle; the inputs'
  // order is what gets submitted.
  let kvDragged = null;
  document.addEventListener('pointerdown', (e) => {
    const handle = e.target.closest('[data-kv-handle]');
    if (handle) handle.closest('[data-kv-row]').draggable = true;
  });
  document.addEventListener('pointerup', () => {
    if (!kvDragged) document.querySelectorAll('[data-kv-row][draggable="true"]').forEach((r) => { r.draggable = false; });
  });
  document.addEventListener('dragstart', (e) => {
    const row = e.target.closest && e.target.closest('[data-kv-row]');
    if (!row || !row.draggable) return;
    kvDragged = row;
    row.classList.add('dragging');
    e.dataTransfer.effectAllowed = 'move';
    try { e.dataTransfer.setData('text/plain', ''); } catch (_) { /* ignore */ }
  });
  document.addEventListener('dragover', (e) => {
    const row = kvDragged && e.target.closest && e.target.closest('[data-kv-row]');
    if (!row || row === kvDragged || row.parentElement !== kvDragged.parentElement) return;
    e.preventDefault();
    const r = row.getBoundingClientRect();
    row.parentElement.insertBefore(kvDragged, e.clientY < r.top + r.height / 2 ? row : row.nextSibling);
  });
  document.addEventListener('dragend', () => {
    if (!kvDragged) return;
    kvDragged.classList.remove('dragging');
    kvDragged.draggable = false;
    kvDragged = null;
  });

  // ----- Filament wizards (create node / server) -----
  // One step at a time; Next validates the step's fields, earlier steps are reachable from the
  // header, Create shows the step of the first invalid field. The step survives htmx refreshes
  // of the form (sessionStorage), a fresh page starts at the first step with an error, else the first.
  const wizardKey = () => 'wizard:' + location.pathname;
  function wizardShow(w, i) {
    const panels = [...w.querySelectorAll('[data-wizard-panel]')];
    i = Math.max(0, Math.min(i, panels.length - 1));
    w.dataset.step = String(i);
    panels.forEach((p, k) => { p.hidden = k !== i; });
    w.querySelectorAll('[data-wizard-step]').forEach((st, k) => { st.classList.toggle('active', k === i); st.classList.toggle('completed', k < i); });
    const last = i === panels.length - 1;
    w.querySelectorAll('[data-wizard-prev]').forEach((b) => { b.hidden = i === 0; });
    w.querySelectorAll('[data-wizard-next]').forEach((b) => { b.hidden = last; });
    w.querySelectorAll('[data-wizard-submit]').forEach((b) => { b.hidden = !last; });
    try { sessionStorage.setItem(wizardKey(), String(i)); } catch (_) { /* ignore */ }
  }
  function wizardInit(root, keep) {
    const list = root.matches && root.matches('[data-wizard]') ? [root] : [...root.querySelectorAll('[data-wizard]')];
    const ws = list.length ? list : (root.closest ? [root.closest('[data-wizard]')].filter(Boolean) : []);
    ws.forEach((w) => {
      const panels = [...w.querySelectorAll('[data-wizard-panel]')];
      const err = panels.findIndex((p) => p.querySelector('.has-error'));
      let stored = 0;
      try { stored = parseInt(sessionStorage.getItem(wizardKey()) || '0', 10) || 0; } catch (_) { /* ignore */ }
      wizardShow(w, err >= 0 ? err : (keep ? stored : 0));
    });
  }
  wizardInit(document, false);
  document.addEventListener('htmx:afterSettle', (e) => { if (e.detail.elt?.closest?.('[data-wizard]') || e.detail.elt?.querySelector?.('[data-wizard-panel]')) wizardInit(e.detail.elt.closest('[data-wizard]') || e.detail.elt, true); });
  // A step's fields that count: not inside something hidden within the step (shown-if fields).
  const wizardFields = (p) => [...p.querySelectorAll('input, select, textarea')].filter((f) => {
    if (!f.willValidate) return false;
    const h = f.parentElement && f.parentElement.closest('[hidden]');
    return !h || h === p;
  });
  document.addEventListener('click', (e) => {
    const w = e.target.closest('[data-wizard]');
    if (!w) return;
    const cur = parseInt(w.dataset.step || '0', 10);
    const panels = [...w.querySelectorAll('[data-wizard-panel]')];
    if (e.target.closest('[data-wizard-prev]')) { wizardShow(w, cur - 1); return; }
    if (e.target.closest('[data-wizard-next]')) {
      const bad = wizardFields(panels[cur]).find((f) => !f.checkValidity());
      if (bad) bad.reportValidity(); else wizardShow(w, cur + 1);
      return;
    }
    const goto = e.target.closest('[data-wizard-goto]');
    if (goto) { const k = parseInt(goto.dataset.wizardGoto, 10); if (k < cur) wizardShow(w, k); return; }
    const submit = e.target.closest('[data-wizard-submit]');
    if (submit) {
      const bad = panels.flatMap(wizardFields).find((f) => !f.checkValidity());
      if (bad) { e.preventDefault(); wizardShow(w, panels.indexOf(bad.closest('[data-wizard-panel]'))); bad.reportValidity(); }
    }
  });

  // Random server name (`hint_random`): one of the names the page was rendered with.
  document.addEventListener('click', (e) => {
    const b = e.target.closest('[data-random-name]');
    if (!b) return;
    let names = [];
    try { names = JSON.parse(b.dataset.randomName || '[]'); } catch (_) { /* ignore */ }
    const input = document.getElementById(b.dataset.target);
    if (input && names.length) { input.value = names[Math.floor(Math.random() * names.length)]; input.dispatchEvent(new Event('input', { bubbles: true })); }
  });

  // ----- admin global search (charrafimed/global-search-modal) -----
  // Recent results live in this browser (the plugin's search history); arrows move between the
  // input and the results, Enter in the input opens the first one.
  const GS_KEY = 'global-search-history';
  const gsHistory = () => { try { return JSON.parse(localStorage.getItem(GS_KEY) || '[]'); } catch (_) { return []; } };
  const gsSave = (list) => { try { localStorage.setItem(GS_KEY, JSON.stringify(list.slice(0, 10))); } catch (_) { /* ignore */ } };
  function gsRecent() {
    const box = document.getElementById('gs-results');
    if (!box || !box.querySelector('[data-gs-empty]')) return;
    const list = gsHistory();
    if (!list.length) return;
    const ul = document.createElement('ul'); ul.className = 'gs-groups';
    const li = document.createElement('li'); const h = document.createElement('h3'); h.className = 'gs-group'; h.textContent = 'recent'; li.appendChild(h);
    const items = document.createElement('ul');
    list.forEach((r, i) => {
      const it = document.createElement('li'); it.className = 'gs-item'; it.setAttribute('role', 'option');
      const a = document.createElement('a'); a.href = r.url; a.dataset.gsResult = ''; a.dataset.gsGroup = r.group;
      const t = document.createElement('h4'); t.textContent = r.title; a.appendChild(t);
      const x = document.createElement('button'); x.type = 'button'; x.className = 'icon-btn gs-remove'; x.setAttribute('aria-label', 'delete'); x.textContent = '×';
      x.addEventListener('click', (e) => { e.preventDefault(); const l = gsHistory(); l.splice(i, 1); gsSave(l); box.innerHTML = '<p class="gs-empty" data-gs-empty>' + (box.dataset.empty || '') + '</p>'; gsRecent(); });
      it.append(a, x); items.appendChild(it);
    });
    li.appendChild(items); ul.appendChild(li);
    box.dataset.empty = box.querySelector('[data-gs-empty]').textContent;
    box.replaceChildren(ul);
  }
  document.addEventListener('click', (e) => {
    const r = e.target.closest('#global-search [data-gs-result]');
    if (r) {
      const entry = { title: r.textContent.trim(), group: r.dataset.gsGroup || '', url: r.getAttribute('href') };
      gsSave([entry, ...gsHistory().filter((x) => x.url !== entry.url)]);
      return;
    }
    if (e.target.id === 'global-search') e.target.close(); // backdrop
    if (e.target.closest('[data-modal-open="global-search"]')) setTimeout(gsRecent);
  });
  document.addEventListener('htmx:afterSwap', (e) => { if (e.detail.target?.id === 'gs-results') gsRecent(); });
  document.addEventListener('keydown', (e) => {
    const d = document.getElementById('global-search');
    if (!d || !d.open) return;
    const input = document.getElementById('gs-input');
    const links = [...d.querySelectorAll('[data-gs-result]')];
    const i = links.indexOf(document.activeElement);
    if (e.key === 'ArrowDown') { e.preventDefault(); (links[i + 1] || links[0])?.focus(); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); if (i <= 0) input?.focus(); else links[i - 1].focus(); }
    else if (e.key === 'Enter' && document.activeElement === input && links[0]) { e.preventDefault(); links[0].click(); }
  });

  // ----- clicks -----
  document.addEventListener('click', (e) => {
    const open = e.target.closest('[data-modal-open]');
    if (open) { e.preventDefault(); openModal(open.dataset.modalOpen); return; }
    const close = e.target.closest('[data-modal-close]');
    if (close) { e.preventDefault(); close.closest('dialog')?.close(); return; }
    const copy = e.target.closest('[data-copy]');
    if (copy) {
      e.preventDefault();
      navigator.clipboard?.writeText(copy.dataset.copy).then(() => notify({ status: 'success', title: strings().copied || 'Copied!', duration: 2000 }));
      return;
    }
    const nav = e.target.closest('[data-nav-toggle]');
    if (nav) { document.querySelector('.shell')?.classList.toggle('nav-open'); return; }
    const row = e.target.closest('tr[data-href]');
    if (row && !e.target.closest('a,button,input,select,label,summary,details,form')) {
      if (e.metaKey || e.ctrlKey) window.open(row.dataset.href, '_blank'); else window.location.href = row.dataset.href;
    }
    // Close open dropdowns when clicking elsewhere.
    document.querySelectorAll('details.dropdown[open]').forEach((d) => { if (!d.contains(e.target)) d.removeAttribute('open'); });
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') document.querySelectorAll('details.dropdown[open]').forEach((d) => d.removeAttribute('open'));
  });

  // In-page tabs: buttons [data-tab=id] and panels [data-tab-panel=id] inside [data-tabs-root].
  function showTab(b) {
    const root = b.closest('[data-tabs-root]') || document;
    // Only this root's own tabs/panels (roots can nest).
    const own = (sel) => [...root.querySelectorAll(sel)].filter((x) => (x.closest('[data-tabs-root]') || document) === root
      || (x.parentElement?.closest('[data-tabs-root]') || document) === root && x.matches('[data-tabs-root]'));
    own('[data-tab]').forEach((x) => x.classList.toggle('active', x === b));
    own('[data-tab-panel]').forEach((p) => { p.hidden = p.dataset.tabPanel !== b.dataset.tab; });
    own('input[name="_tab"]').forEach((i) => { i.value = b.dataset.tab; });
    if (root !== document && root.hasAttribute('data-tabs-local')) return;
    try { const u = new URL(window.location.href); u.searchParams.set('tab', b.dataset.tab); history.replaceState(null, '', u); } catch (_) { /* ignore */ }
  }
  document.addEventListener('click', (e) => {
    const b = e.target.closest('[data-tab]');
    if (b) { e.preventDefault(); showTab(b); }
  });
  // Open the first tab that holds a validation error.
  document.addEventListener('DOMContentLoaded', () => {
    const err = document.querySelector('[data-tab-panel] .has-error');
    const panel = err?.closest('[data-tab-panel]');
    const root = panel?.closest('[data-tabs-root]');
    const btn = root?.querySelector('[data-tab="' + panel.dataset.tabPanel + '"]');
    if (btn) showTab(btn);
  });

  // Conditional fields: [data-show-if="name=v1,v2"] visible when the control `name` has one of the values.
  function controlValue(form, name) {
    const els = (form || document).querySelectorAll('[name="' + name + '"]');
    for (const el of els) {
      if ((el.type === 'radio' || el.type === 'checkbox')) { if (el.checked) return el.value; continue; }
      if (el.type !== 'hidden' || els.length === 1) return el.value;
    }
    return '';
  }
  function applyShowIf(root) {
    (root || document).querySelectorAll('[data-show-if]').forEach((el) => {
      const [name, values] = el.dataset.showIf.split('=');
      const form = el.closest('form');
      const show = values.split(',').includes(controlValue(form, name));
      el.hidden = !show;
      el.querySelectorAll('input,select,textarea').forEach((i) => { if (i.dataset.keepDisabled === undefined) i.disabled = !show && i.type !== 'hidden'; });
    });
  }
  document.addEventListener('change', () => applyShowIf(document));
  document.addEventListener('DOMContentLoaded', () => applyShowIf(document));

  // Key/value repeaters: [data-kv] with rows [data-kv-row], add button [data-kv-add].
  document.addEventListener('click', (e) => {
    const add = e.target.closest('[data-kv-add]');
    if (!add) return;
    const list = add.closest('.field')?.querySelector('[data-kv]');
    const row = list?.querySelector('[data-kv-row]:last-child');
    if (!row) return;
    const clone = row.cloneNode(true);
    clone.querySelectorAll('input').forEach((i) => { i.value = ''; });
    list.appendChild(clone);
    clone.querySelector('input')?.focus();
  });
  document.addEventListener('click', (e) => {
    const rm = e.target.closest('[data-kv-remove]');
    if (rm) {
      const row = rm.closest('[data-kv-row]');
      if (row.parentElement.querySelectorAll('[data-kv-row]').length > 1) row.remove();
      else row.querySelectorAll('input').forEach((i) => { i.value = ''; });
      return;
    }
    const reset = e.target.closest('[data-kv-reset]');
    if (!reset) return;
    const list = reset.closest('.field')?.querySelector('[data-kv]');
    const rows = list ? [...list.querySelectorAll('[data-kv-row]')] : [];
    if (!rows.length) return;
    let pairs = [];
    try { pairs = JSON.parse(reset.dataset.kvReset); } catch (_) { /* keep empty */ }
    if (!pairs.length) pairs = [['', '']];
    rows.slice(1).forEach((r) => r.remove());
    pairs.forEach((p, i) => {
      const r = i === 0 ? rows[0] : rows[0].cloneNode(true);
      const inputs = r.querySelectorAll('input');
      if (inputs[0]) inputs[0].value = p[0] ?? '';
      if (inputs[1]) inputs[1].value = p[1] ?? '';
      if (i > 0) list.appendChild(r);
    });
  });

  // TagsInput: [data-tags] keeps a JSON array in its hidden input; Enter (or leaving the entry) adds a tag.
  function syncTags(box) {
    const values = [...box.querySelectorAll('[data-tag]')].map((c) => c.dataset.tag);
    const hidden = box.querySelector('input[type=hidden]');
    if (hidden) hidden.value = JSON.stringify(values);
  }
  function addTag(entry) {
    const box = entry.closest('[data-tags]');
    const value = entry.value.trim();
    entry.value = '';
    if (!box || !value) return;
    if ([...box.querySelectorAll('[data-tag]')].some((c) => c.dataset.tag === value)) return;
    const chip = document.createElement('span');
    chip.className = 'chip'; chip.dataset.tag = value; chip.textContent = value;
    const rm = document.createElement('button');
    rm.type = 'button'; rm.setAttribute('data-tag-remove', ''); rm.setAttribute('aria-label', '\u00d7'); rm.textContent = '\u00d7';
    chip.appendChild(rm);
    box.insertBefore(chip, entry);
    syncTags(box);
  }
  document.addEventListener('keydown', (e) => {
    const entry = e.target.closest('.tags-entry');
    if (!entry) return;
    if (e.key === 'Enter') { e.preventDefault(); addTag(entry); }
    if (e.key === 'Backspace' && !entry.value) {
      const chips = entry.closest('[data-tags]').querySelectorAll('[data-tag]');
      if (chips.length) { chips[chips.length - 1].remove(); syncTags(entry.closest('[data-tags]')); }
    }
  });
  document.addEventListener('focusout', (e) => { const entry = e.target.closest?.('.tags-entry'); if (entry) addTag(entry); });
  document.addEventListener('click', (e) => {
    const rm = e.target.closest('[data-tag-remove]');
    if (rm) { const box = rm.closest('[data-tags]'); rm.closest('[data-tag]').remove(); syncTags(box); return; }
    const box = e.target.closest('[data-tags]');
    if (box && e.target === box) box.querySelector('.tags-entry')?.focus();
  });

  // Collapsible sections remembered per browser (`persistCollapsed()`): <details data-persist="key">.
  function restoreDetails(root) {
    root.querySelectorAll('details[data-persist]').forEach((d) => {
      try { const v = localStorage.getItem('collapsed:' + d.dataset.persist); if (v !== null) d.open = v !== '1'; } catch (_) { /* storage blocked */ }
    });
  }
  document.addEventListener('toggle', (e) => {
    const d = e.target;
    if (!(d instanceof HTMLDetailsElement) || !d.dataset.persist) return;
    try { localStorage.setItem('collapsed:' + d.dataset.persist, d.open ? '0' : '1'); } catch (_) { /* storage blocked */ }
  }, true);
  document.addEventListener('DOMContentLoaded', () => restoreDetails(document));
  document.addEventListener('htmx:afterSwap', (e) => restoreDetails(e.target));

  // Direct file uploads: <input type=file data-upload="/url"> posts the raw file, then reloads.
  document.addEventListener('change', async (e) => {
    const input = e.target.closest?.('input[type=file][data-upload]');
    const file = input?.files?.[0];
    if (!file) return;
    try {
      const r = await fetch(input.dataset.upload, {
        method: 'POST', body: file,
        headers: { 'X-CSRF-TOKEN': csrf(), 'Content-Type': file.type || 'application/octet-stream' },
      });
      if (r.ok) { window.location.reload(); return; }
    } catch (_) { /* reported below */ }
    input.value = '';
    input.setCustomValidity('Upload failed'); input.reportValidity(); input.setCustomValidity('');
  });

  // Table bulk selection: [data-bulk-all] toggles the rows, the bar shows with a selection.
  function updateBulk(id) {
    const items = [...document.querySelectorAll('[data-bulk-item="' + id + '"]')];
    const n = items.filter((i) => i.checked).length;
    const bar = document.getElementById(id + '-bulk');
    if (bar) {
      bar.hidden = n === 0;
      const c = bar.querySelector('[data-bulk-count]');
      if (c) c.textContent = n === 1 ? (c.dataset.one || '1') : (c.dataset.many || ':count').replace(':count', n);
    }
    const all = document.querySelector('[data-bulk-all="' + id + '"]');
    if (all) { all.checked = n > 0 && n === items.length; all.indeterminate = n > 0 && n < items.length; }
  }
  document.addEventListener('change', (e) => {
    const all = e.target.closest?.('[data-bulk-all]');
    if (all) {
      document.querySelectorAll('[data-bulk-item="' + all.dataset.bulkAll + '"]').forEach((i) => { i.checked = all.checked; });
      updateBulk(all.dataset.bulkAll);
      return;
    }
    const item = e.target.closest?.('[data-bulk-item]');
    if (item) updateBulk(item.dataset.bulkItem);
  });

  // Reveal / hide password inputs.
  document.addEventListener('click', (e) => {
    const b = e.target.closest('[data-reveal]');
    if (!b) return;
    const input = document.getElementById(b.dataset.reveal);
    if (input) input.type = input.type === 'password' ? 'text' : 'password';
  });

  // ----- passkeys (the original's resources/js/passkeys on @laravel/passkeys) -----
  const b64u = {
    decode: (s) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4)), (c) => c.charCodeAt(0)).buffer,
    encode: (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, ''),
  };
  async function passkeyFetch(url, body) {
    const r = await fetch(url, {
      method: body ? 'POST' : 'GET',
      credentials: 'same-origin',
      headers: { Accept: 'application/json', 'Content-Type': 'application/json', 'X-CSRF-TOKEN': csrf() },
      body: body ? JSON.stringify(body) : undefined,
    });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(data.message || r.statusText);
    return data;
  }
  function credentialJSON(c) {
    const r = c.response;
    const out = { id: c.id, rawId: b64u.encode(c.rawId), type: c.type, response: { clientDataJSON: b64u.encode(r.clientDataJSON) } };
    if (r.attestationObject) {
      out.response.attestationObject = b64u.encode(r.attestationObject);
      out.response.transports = r.getTransports ? r.getTransports() : [];
    } else {
      out.response.authenticatorData = b64u.encode(r.authenticatorData);
      out.response.signature = b64u.encode(r.signature);
      if (r.userHandle) out.response.userHandle = b64u.encode(r.userHandle);
    }
    out.clientExtensionResults = c.getClientExtensionResults ? c.getClientExtensionResults() : {};
    return out;
  }
  const descriptors = (list) => (list || []).map((d) => ({ ...d, id: b64u.decode(d.id) }));
  window.registerPasskey = async function (name) {
    const { options: o } = await passkeyFetch('/user/passkeys/options');
    const publicKey = { ...o, challenge: b64u.decode(o.challenge), user: { ...o.user, id: b64u.decode(o.user.id) }, excludeCredentials: descriptors(o.excludeCredentials) };
    const cred = await navigator.credentials.create({ publicKey });
    return passkeyFetch('/user/passkeys', { name, credential: credentialJSON(cred) });
  };
  window.authenticateWithPasskey = async function () {
    const { options: o } = await passkeyFetch('/passkeys/login/options');
    const publicKey = { ...o, challenge: b64u.decode(o.challenge), allowCredentials: descriptors(o.allowCredentials) };
    const cred = await navigator.credentials.get({ publicKey });
    return passkeyFetch('/passkeys/login', { credential: credentialJSON(cred) });
  };
  document.addEventListener('click', async (e) => {
    const reg = e.target.closest('[data-passkey-register]');
    if (reg) {
      try {
        await window.registerPasskey(document.getElementById('passkey-name')?.value || '');
        location.reload();
      } catch (err) {
        notify({ status: 'danger', title: reg.dataset.failed, body: err?.message });
      }
      return;
    }
    const login = e.target.closest('[data-passkey-login]');
    if (login) {
      try {
        const r = await window.authenticateWithPasskey();
        if (r?.redirect) location.href = r.redirect;
      } catch (err) {
        // Dismissing the OS / password-manager prompt is not a failure worth surfacing.
        if (err?.name === 'NotAllowedError' || err?.name === 'AbortError') return;
        notify({ status: 'danger', title: err?.message || login.dataset.failed });
      }
    }
  });

  document.addEventListener('DOMContentLoaded', () => flushFlash(document));

  // ----- page loads: a progress bar as soon as a link or form starts one -----
  const loading = (on) => document.documentElement.classList.toggle('nav-loading', on);
  document.addEventListener('click', (e) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const a = e.target.closest('a[href]');
    if (!a || a.target === '_blank' || a.hasAttribute('download') || a.hasAttribute('hx-get')) return;
    const url = new URL(a.href, location.href);
    if (url.origin !== location.origin || (url.pathname === location.pathname && url.search === location.search)) return;
    loading(true);
  });
  document.addEventListener('submit', (e) => {
    const f = e.target;
    if (!e.defaultPrevented && !f.matches('[hx-post],[hx-get],[hx-put],[hx-delete],[target=_blank]')) loading(true);
  });
  window.addEventListener('pageshow', () => loading(false));
})();

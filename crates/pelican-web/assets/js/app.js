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
  try { applyTheme(localStorage.getItem('theme')); } catch (_) { /* storage blocked */ }
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
    if (!(f instanceof HTMLFormElement) || !f.dataset.confirm || f.dataset.confirmed || f.hasAttribute('hx-post')) return;
    e.preventDefault();
    confirmDialog(f.dataset.confirm, { title: f.dataset.confirmTitle, ok: f.dataset.confirmOk, color: f.dataset.confirmColor }).then((ok) => {
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
    root.querySelectorAll('[data-tab]').forEach((x) => x.classList.toggle('active', x === b));
    root.querySelectorAll('[data-tab-panel]').forEach((p) => { p.hidden = p.dataset.tabPanel !== b.dataset.tab; });
    root.querySelectorAll('input[name="_tab"]').forEach((i) => { i.value = b.dataset.tab; });
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

  // Reveal / hide password inputs.
  document.addEventListener('click', (e) => {
    const b = e.target.closest('[data-reveal]');
    if (!b) return;
    const input = document.getElementById(b.dataset.reveal);
    if (input) input.type = input.type === 'password' ? 'text' : 'password';
  });

  document.addEventListener('DOMContentLoaded', () => flushFlash(document));
})();

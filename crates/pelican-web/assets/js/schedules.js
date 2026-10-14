// Server › Schedules glue: dialog opening (deep links + validation re-renders),
// the cron preset buttons with their active-state highlighting and a live
// next-run preview, task-dialog payload switching, and the multipart import
// submit (CSRF via header like VerifyCsrfToken allows).
(function () {
  'use strict';

  // ----- dialogs -----
  function open(d) {
    if (d && !d.open && typeof d.showModal === 'function') d.showModal();
  }
  document.addEventListener('DOMContentLoaded', () => {
    open(document.querySelector('dialog[data-dialog-open]'));
    const create = document.getElementById('create-task');
    if (create?.querySelector('.has-error')) open(create);
  });

  // ----- cron fields -----------------------------------------------------
  const FIELDS = ['minute', 'hour', 'dom', 'month', 'dow'];
  const NAMES = { minute: 'cron_minute', hour: 'cron_hour', dom: 'cron_day_of_month', month: 'cron_month', dow: 'cron_day_of_week' };

  function cronInputs(root) {
    return root.querySelectorAll('[data-cron-field]');
  }

  function setValue(form, name, value) {
    form.querySelectorAll('[name="' + name + '"]').forEach((i) => { i.value = value; });
  }

  function values(form) {
    const v = {};
    for (const key of FIELDS) v[key] = form.querySelector('[name="' + NAMES[key] + '"]')?.value ?? '';
    return v;
  }

  // CronPresetAction::setUp(): fixed presets get `success` while they match.
  // The every-x ones highlight per ScheduleResource's color closures.
  function applyColors(presets, v) {
    const starts = (s, p) => typeof s === 'string' && s.startsWith(p);
    for (const btn of presets) {
      let ok;
      if (btn.dataset.presetEvery) {
        const kind = btn.dataset.presetEvery;
        if (kind === 'minutes') ok = starts(v.minute, '*/') && ['*', v.hour, v.dom, v.month, v.dow].every((x) => x === '*');
        else if (kind === 'hours') ok = v.minute === '0' && starts(v.hour, '*/') && ['*', v.dom, v.month, v.dow].every((x) => x === '*');
        else if (kind === 'days') ok = v.minute === '0' && v.hour === '0' && starts(v.dom, '*/') && v.month === '*' && v.dow === '*';
        else if (kind === 'months') ok = v.minute === '0' && v.hour === '0' && v.dom === '1' && starts(v.month, '*/') && v.dow === '*';
        else ok = v.minute === '0' && v.hour === '0' && v.dom === '*' && v.month === '*' && v.dow !== '*';
      } else {
        ok = v.minute === btn.dataset.minute && v.hour === btn.dataset.hour && v.dom === btn.dataset.dom
          && v.month === btn.dataset.month && v.dow === btn.dataset.dow;
      }
      btn.classList.toggle('btn-success', !!ok);
    }
  }

  // Next-run preview mirroring Utilities::getScheduleNextRunDate for the
  // common expression forms (*, lists, ranges, steps) in the browser timezone.
  function parseField(raw, min, max) {
    const set = new Set();
    if (!raw || !raw.trim()) return null;
    for (const part of raw.trim().split(',')) {
      let body = part, step = 1;
      const slash = body.indexOf('/');
      if (slash >= 0) {
        step = parseInt(body.slice(slash + 1), 10);
        if (!(step > 0)) return null;
        body = body.slice(0, slash);
      }
      let lo = min, hi = max;
      if (body !== '*') {
        const dash = body.indexOf('-');
        lo = parseInt(dash >= 0 ? body.slice(0, dash) : body, 10);
        hi = dash >= 0 ? parseInt(body.slice(dash + 1), 10) : step > 1 && slash < 0 ? null : lo;
        if (Number.isNaN(lo)) return null;
        if (hi === null) hi = max; // "n/step"
        if (Number.isNaN(hi) || hi > max) return null;
      } else if (slash < 0) {
        continue; // full range handled implicitly by the loop over everything
      }
      for (let n = lo; n <= hi; n += step) set.add(n);
    }
    return set;
  }

  // Standard-cron day/month/day-of-week combination check.
  function matches(sets, d) {
    const [ms, hs, ds, mos, dows] = sets;
    if (ms && !ms.has(d.getMinutes())) return false;
    if (hs && !hs.has(d.getHours())) return false;
    if (mos && !mos.has(d.getMonth() + 1)) return false;
    const dd = ds ? ds.has(d.getDate()) : true;
    const dw = dows ? dows.has(d.getDay() === 0 ? 0 : d.getDay()) : true;
    // Standard cron: both restricted → OR, otherwise AND.
    if (ds && dows) return dd || dw;
    return dd && dw;
  }

  function computeNext(v) {
    const sets = [parseField(v.minute, 0, 59), parseField(v.hour, 0, 23), parseField(v.dom, 1, 31),
      parseField(v.month, 1, 12), parseField(v.dow.replace(/^7$/, '0'), 0, 6)];
    if (sets.some((s) => s === null)) return null;
    const now = new Date();
    const t = new Date(now.getTime());
    t.setSeconds(0, 0);
    t.setMinutes(t.getMinutes() + 1);
    const limit = new Date(now.getFullYear() + 5, now.getMonth(), now.getDate()).getTime();
    while (t.getTime() <= limit) {
      if (matches(sets, t)) return t;
      t.setMinutes(t.getMinutes() + 1);
    }
    return null;
  }

  function pad(n) { return String(n).padStart(2, '0'); }

  function refreshPreview(root) {
    const out = root.querySelector('#cron-next-run');
    const form = root.closest('form') || root.querySelector('form') || document.querySelector('[data-cron-fields]');
    if (!out || !form) return;
    const v = values(form);
    const next = computeNext(v);
    out.textContent = next
      ? `${next.getFullYear()}-${pad(next.getMonth() + 1)}-${pad(next.getDate())} ${pad(next.getHours())}:${pad(next.getMinutes())}:00`
      : out.dataset.invalid || '—';
  }

  document.addEventListener('input', (e) => {
    const field = e.target.closest?.('[data-cron-field]');
    if (!field) return;
    const root = field.closest('[data-cron-root]');
    applyColors(root.querySelectorAll('[data-preset],[data-preset-every]'), values(field.form));
    refreshPreview(root);
  });

  document.addEventListener('click', (e) => {
    const preset = e.target.closest?.('[data-preset]');
    const every = e.target.closest?.('[data-preset-every]');
    if (!preset && !every) return;
    const root = (preset || every).closest('[data-cron-root]');
    const form = document.querySelector('[data-cron-fields]');
    if (!root || !form) return;

    if (preset) {
      setValue(form, NAMES.minute, preset.dataset.minute);
      setValue(form, NAMES.hour, preset.dataset.hour);
      setValue(form, NAMES.dom, preset.dataset.dom);
      setValue(form, NAMES.month, preset.dataset.month);
      setValue(form, NAMES.dow, preset.dataset.dow);
    } else {
      const name = { minutes: '_x_minutes', hours: '_x_hours', days: '_x_days', months: '_x_months', day_of_week: '_x_dow' }[every.dataset.presetEvery];
      const raw = root.querySelector('[name="' + name + '"]')?.value.trim();
      const n = parseInt(raw, 10);
      if (Number.isNaN(n) || n < 1) return;
      const star = (k, val) => setValue(form, NAMES[k], val);
      if (every.dataset.presetEvery === 'minutes') {
        star('minute', '*/' + n); star('hour', '*'); star('dom', '*'); star('month', '*'); star('dow', '*');
      } else if (every.dataset.presetEvery === 'hours') {
        star('minute', '0'); star('hour', '*/' + n); star('dom', '*'); star('month', '*'); star('dow', '*');
      } else if (every.dataset.presetEvery === 'days') {
        star('minute', '0'); star('hour', '0'); star('dom', '*/' + n); star('month', '*'); star('dow', '*');
      } else if (every.dataset.presetEvery === 'months') {
        star('minute', '0'); star('hour', '0'); star('dom', '1'); star('month', '*/' + n); star('dow', '*');
      } else {
        const dow = root.querySelector('[name="_x_dow"]')?.value ?? '1';
        star('minute', '0'); star('hour', '0'); star('dom', '*'); star('month', '*'); star('dow', dow);
      }
    }
    applyColors(root.querySelectorAll('[data-preset],[data-preset-every]'), values(form));
    refreshPreview(root);
  });

  document.addEventListener('DOMContentLoaded', () => {
    const root = document.querySelector('[data-cron-root]');
    if (!root) return;
    const form = document.querySelector('[data-cron-fields]');
    const invalidNode = root.querySelector('#cron-next-run');
    if (invalidNode) invalidNode.dataset.invalid = invalidNode.textContent.trim().length && invalidNode.textContent.trim() !== '—'
      ? '' : '';
    applyColors(root.querySelectorAll('[data-preset],[data-preset-every]'), values(form));
    // Only run the live preview when the current expression already evaluates.
    if (document.getElementById('cron-next-run')) refreshPreview(root);
  });

  // ----- task dialogs: one enabled payload control per action -------------
  const PAYLOAD_KIND = { command: 'command', power: 'power', backup: 'backup', delete_files: 'delete_files' };
  document.addEventListener('change', (e) => {
    const sel = e.target.closest?.('[data-task-action]');
    if (!sel) return;
    const dlg = sel.closest('[data-task-dialog]');
    const kind = PAYLOAD_KIND[sel.value];
    dlg?.querySelectorAll('[data-payload-kind]').forEach((g) => {
      const show = g.dataset.payloadKind === kind;
      g.hidden = !show;
      g.querySelectorAll('input,textarea,select').forEach((i) => {
        i.disabled = !show && i.type !== 'hidden';
        if (show && sel.value !== dlg.dataset.actionBefore) {
          if (i.tagName === 'SELECT') i.value = i.options[1]?.value ?? i.value; // default payload (restart)
          else i.value = '';
        }
      });
    });
    if (dlg) dlg.dataset.actionBefore = sel.value;
  });

  // ----- import modal: multipart fetch with CSRF header -------------------
  document.addEventListener('click', (e) => {
    const add = e.target.closest?.('[data-url-add]');
    if (add) {
      const list = add.closest('.modal-body')?.querySelector('#import-urls');
      const last = list?.querySelector('[data-url-row]:last-child');
      if (!list || !last) return;
      const row = last.cloneNode(true);
      row.querySelector('input').value = '';
      row.querySelector('[name$="[url]"]').name = 'urls[' + list.children.length + '][url]';
      row.querySelector('[data-url-remove]').hidden = false;
      list.appendChild(row);
      add.hidden = false;
      return;
    }
    const rm = e.target.closest?.('[data-url-remove]');
    if (rm) {
      const list = rm.closest('#import-urls');
      const rows = list?.querySelectorAll('[data-url-row]');
      if (rows && rows.length > 1) rm.closest('[data-url-row]').remove();
    }
  });

  document.addEventListener('submit', (e) => {
    const f = e.target.closest?.('form[data-import-fetch]');
    if (!f || e.defaultPrevented) return;
    e.preventDefault();
    fetch(f.action, {
      method: 'POST',
      headers: { 'X-CSRF-TOKEN': f.dataset.csrf || '' },
      body: new FormData(f),
    })
      .then(() => window.location.reload())
      .catch(() => window.location.reload());
  });
})();

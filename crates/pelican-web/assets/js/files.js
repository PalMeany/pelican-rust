// File manager: selection + bulk actions, row actions in shared modals,
// uploads straight to Wings, CodeMirror editor (ListFiles / EditFiles).
(function () {
  'use strict';

  function list() {
    const fm = document.getElementById('fm');
    if (!fm) return;
    const strings = JSON.parse(document.getElementById('fm-strings')?.textContent || '{}');
    const checks = () => Array.from(fm.querySelectorAll('.fm-check'));
    const selected = () => checks().filter((c) => c.checked).map((c) => c.value);
    const bulk = fm.querySelector('.fm-bulk');

    function refreshBulk() {
      const n = selected().length;
      if (bulk) bulk.hidden = n === 0;
      const c = fm.querySelector('[data-selected-count]');
      if (c) c.textContent = n + ' selected';
    }
    fm.querySelector('[data-select-all]')?.addEventListener('change', (e) => {
      checks().forEach((c) => { c.checked = e.target.checked; });
      refreshBulk();
    });
    fm.addEventListener('change', (e) => { if (e.target.classList.contains('fm-check')) refreshBulk(); });

    // Bulk modals receive the selection as files[] fields.
    function fillFiles(form) {
      const holder = form.querySelector('[data-bulk-files]');
      if (!holder) return;
      holder.innerHTML = '';
      selected().forEach((name) => {
        const i = document.createElement('input');
        i.type = 'hidden'; i.name = 'files[]'; i.value = name;
        holder.appendChild(i);
      });
    }
    document.querySelectorAll('[data-modal-open="fm-bulk-move"],[data-modal-open="fm-bulk-archive"]').forEach((b) => {
      b.addEventListener('click', () => fillFiles(document.querySelector('#' + b.dataset.modalOpen + ' form')));
    });

    const quick = document.getElementById('fm-quick');
    function submitQuick(op, name, files) {
      quick.querySelector('[name=op]').value = op;
      quick.querySelector('[name=name]').value = name || '';
      if (files) { quick.querySelector('[data-bulk-files]').innerHTML = ''; selected(); fillFiles(quick); }
      quick.submit();
    }
    fm.querySelector('[data-bulk-delete]')?.addEventListener('click', () => {
      if (window.confirm(strings.confirm || 'Are you sure?')) submitQuick('bulk-delete', '', true);
    });

    // Row actions: fill `name` (and the permission checkboxes) then open the modal.
    fm.addEventListener('click', (e) => {
      const b = e.target.closest('[data-row-action]');
      if (!b) return;
      e.preventDefault();
      const row = b.closest('tr');
      const name = row.dataset.file;
      b.closest('details')?.removeAttribute('open');
      const act = b.dataset.rowAction;
      if (act === 'copy' || act === 'unarchive') return submitQuick(act, name);
      if (act === 'delete') {
        if (window.confirm((b.dataset.confirmTitle || strings.delete) + '\n\n' + (strings.confirm || ''))) submitQuick('delete', name);
        return;
      }
      const dialog = document.getElementById(act);
      if (!dialog) return;
      dialog.querySelector('[name=name]').value = name;
      if (act === 'fm-rename') dialog.querySelector('[name=to]').value = name;
      if (act === 'fm-chmod') {
        const mode = String(row.dataset.mode || '000').padStart(3, '0');
        ['owner', 'group', 'public'].forEach((g, i) => {
          const bits = parseInt(mode[i] || '0', 10);
          dialog.querySelectorAll('[name="' + g + '[]"]').forEach((c) => {
            c.checked = (c.value === 'read' && bits & 4) || (c.value === 'write' && bits & 2) || (c.value === 'execute' && bits & 1);
          });
        });
      }
      dialog.showModal();
    });

    // ----- uploads (FileUploadManager): POST multipart `files` to the signed Wings URL -----
    const input = document.getElementById('fm-upload-input');
    document.querySelector('[data-upload-trigger]')?.addEventListener('click', () => input?.click());
    input?.addEventListener('change', () => upload(Array.from(input.files || [])));
    fm.addEventListener('dragover', (e) => { e.preventDefault(); fm.classList.add('drop'); });
    fm.addEventListener('dragleave', () => fm.classList.remove('drop'));
    fm.addEventListener('drop', (e) => {
      e.preventDefault();
      fm.classList.remove('drop');
      const files = Array.from(e.dataTransfer?.files || []);
      if (files.length) upload(files);
    });

    async function upload(files) {
      if (!files.length) return;
      const box = document.getElementById('fm-uploads');
      const listEl = box.querySelector('[data-upload-list]');
      const progress = box.querySelector('[data-upload-progress]');
      box.hidden = false;
      listEl.innerHTML = '';
      let done = 0, failed = 0;
      const uploaded = [];
      const fmt = () => (strings.progress || ':done of :total').replace(':done', done).replace(':total', files.length);
      progress.textContent = fmt();
      for (const file of files) {
        const row = document.createElement('div');
        row.innerHTML = '<div class="small truncate"></div><div class="progress"><span style="width:0%"></span></div>';
        row.firstChild.textContent = file.name;
        listEl.appendChild(row);
        let url;
        try {
          const r = await fetch(fm.dataset.upload, { headers: { Accept: 'application/json' }, credentials: 'same-origin' });
          if (!r.ok) throw new Error(String(r.status));
          url = (await r.json()).url;
        } catch (_) {
          window.notify?.({ status: 'danger', title: strings.token_failed || 'Failed to get an upload token' });
          failed++;
          continue;
        }
        const ok = await new Promise((resolve) => {
          const xhr = new XMLHttpRequest();
          xhr.open('POST', url);
          xhr.upload.onprogress = (ev) => {
            if (ev.lengthComputable) row.querySelector('span').style.width = (ev.loaded / ev.total * 100).toFixed(1) + '%';
          };
          xhr.onload = () => resolve(xhr.status >= 200 && xhr.status < 300);
          xhr.onerror = () => resolve(false);
          const data = new FormData();
          data.append('files', file, file.name);
          xhr.send(data);
        });
        if (ok) { done++; uploaded.push(file.name); } else { failed++; row.querySelector('.progress').classList.add('danger'); }
        progress.textContent = fmt();
      }
      if (uploaded.length) {
        const body = new URLSearchParams();
        body.append('directory', fm.dataset.dir);
        uploaded.forEach((n) => body.append('files[]', n));
        await fetch(fm.dataset.base + '/files/uploaded', {
          method: 'POST', credentials: 'same-origin',
          headers: { 'X-CSRF-TOKEN': document.querySelector('meta[name="csrf-token"]')?.content || '' }, body,
        }).catch(() => {});
      }
      window.notify?.({ status: failed ? 'danger' : 'success', title: failed ? (strings.failed || 'Failed') : (strings.uploaded || 'Uploaded') });
      setTimeout(() => window.location.reload(), 800);
    }
  }

  // ----- editor (EditFiles) -----
  const MODES = {
    plaintext: null, json: { name: 'javascript', json: true }, javascript: 'javascript', typescript: { name: 'javascript', typescript: true },
    yaml: 'yaml', ini: 'properties', properties: 'properties', toml: 'toml', shell: 'shell', python: 'python',
    java: 'text/x-java', kotlin: 'text/x-kotlin', c: 'text/x-csrc', cpp: 'text/x-c++src', csharp: 'text/x-csharp',
    go: 'text/x-go', rust: 'text/x-rustsrc', lua: 'lua', html: 'htmlmixed', xml: 'xml', css: 'css', markdown: 'markdown',
    sql: 'text/x-sql', mysql: 'text/x-mysql', pgsql: 'text/x-pgsql', dockerfile: 'dockerfile', php: 'text/x-php', ruby: 'text/x-ruby',
  };

  function editor() {
    const form = document.getElementById('fm-editor');
    const ta = document.getElementById('fm-content');
    if (!form || !ta || !window.CodeMirror) return;
    const dark = () => document.documentElement.dataset.theme === 'dark'
      || (!document.documentElement.dataset.theme && window.matchMedia('(prefers-color-scheme: dark)').matches);
    const lang = document.getElementById('fm-lang');
    const cm = window.CodeMirror.fromTextArea(ta, {
      lineNumbers: true, matchBrackets: true, styleActiveLine: true, indentUnit: 4, tabSize: 4,
      mode: MODES[lang?.value] || null, theme: dark() ? 'material-darker' : 'default', viewportMargin: Infinity,
    });
    cm.setSize(null, '65vh');
    lang?.addEventListener('change', () => cm.setOption('mode', MODES[lang.value] || null));
    const save = async (close) => {
      cm.save();
      if (close) { form.querySelector('[name=close][value="1"]')?.click(); return; }
      const body = new URLSearchParams(new FormData(form));
      body.set('close', '0');
      const r = await fetch(form.action, {
        method: 'POST', credentials: 'same-origin', body,
        headers: { 'HX-Request': 'true', 'X-CSRF-TOKEN': document.querySelector('meta[name="csrf-token"]')?.content || '' },
      });
      window.notify?.({ status: r.ok ? 'success' : 'danger', title: r.ok ? (form.dataset.saved || 'Saved') : 'Error ' + r.status });
    };
    form.querySelector('[data-save]')?.addEventListener('click', (e) => { e.preventDefault(); save(false); });
    document.addEventListener('keydown', (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
        e.preventDefault();
        save(e.shiftKey);
      }
    });
  }

  function start() { list(); editor(); }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start); else start();
})();

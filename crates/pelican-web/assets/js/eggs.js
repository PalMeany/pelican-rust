// Admin › Eggs: import modal (files read into hidden fields), variables repeater,
// "copy settings from" selects and the install script editor.
(() => {
  // Import: each selected file becomes contents[] + names[] hidden fields.
  document.addEventListener('change', (e) => {
    const input = e.target.closest('[data-egg-files]');
    if (!input) return;
    const box = input.closest('form').querySelector('[data-egg-file-contents]');
    box.innerHTML = '';
    for (const file of input.files) {
      const reader = new FileReader();
      reader.onload = () => {
        const name = document.createElement('input');
        name.type = 'hidden'; name.name = 'names[]'; name.value = file.name;
        const content = document.createElement('textarea');
        content.name = 'contents[]'; content.value = reader.result;
        box.append(name, content);
      };
      reader.readAsText(file);
    }
  });

  // Variables repeater.
  const snakeUpper = (s) => s.trim()
    .replace(/([a-z\d])([A-Z])/g, '$1_$2')
    .replace(/[\s\-]+/g, '_')
    .replace(/_+/g, '_')
    .toUpperCase();
  document.addEventListener('click', (e) => {
    const add = e.target.closest('[data-var-add]');
    if (add) {
      const tpl = document.getElementById('egg-var-template');
      const list = document.querySelector('[data-var-list]');
      const node = tpl.content.firstElementChild.cloneNode(true);
      node.open = true;
      list.appendChild(node);
      node.querySelector('[data-var-name]')?.focus();
      return;
    }
    const remove = e.target.closest('[data-var-remove]');
    if (remove) { e.preventDefault(); remove.closest('[data-var-item]').remove(); return; }
    const move = e.target.closest('[data-var-move]');
    if (move) {
      e.preventDefault();
      const item = move.closest('[data-var-item]');
      if (move.dataset.varMove === '-1' && item.previousElementSibling) item.parentNode.insertBefore(item, item.previousElementSibling);
      if (move.dataset.varMove === '1' && item.nextElementSibling) item.parentNode.insertBefore(item.nextElementSibling, item);
    }
  });
  let nameTimer;
  document.addEventListener('input', (e) => {
    const name = e.target.closest('[data-var-name]');
    if (!name) return;
    const item = name.closest('[data-var-item]');
    const title = item.querySelector('[data-var-title]');
    if (title) title.textContent = name.value || JSON.parse(document.getElementById('egg-strings')?.textContent || '{}').new_variable || '';
    // Name → env_variable (afterStateUpdated, debounced 750 ms).
    clearTimeout(nameTimer);
    nameTimer = setTimeout(() => {
      const env = item.querySelector('[data-var-env]');
      if (env) env.value = snakeUpper(name.value);
    }, 750);
  });
  document.addEventListener('change', (e) => {
    const cb = e.target.closest('input[type=checkbox][data-bool]');
    if (cb && cb.nextElementSibling) cb.nextElementSibling.value = cb.checked ? '1' : '0';
  });

  // Install script editor.
  let editor = null;
  document.addEventListener('DOMContentLoaded', () => {
    const ta = document.getElementById('egg-script');
    if (!ta || !window.CodeMirror) return;
    const dark = () => document.documentElement.dataset.theme === 'dark'
      || (!document.documentElement.dataset.theme && window.matchMedia('(prefers-color-scheme: dark)').matches);
    editor = window.CodeMirror.fromTextArea(ta, { mode: 'shell', lineNumbers: true, readOnly: ta.readOnly, theme: dark() ? 'material-darker' : 'default', viewportMargin: Infinity });
    editor.setSize(null, '60vh');
    document.addEventListener('click', (ev) => { if (ev.target.closest('[data-tab="install_script"]')) setTimeout(() => editor.refresh(), 0); });
    ta.form?.addEventListener('submit', () => editor.save());
  });

  // CopyFrom: process copies config_*, script copies container/entry/script.
  const setField = (name, value) => {
    const el = document.querySelector('[data-egg-form] [name="' + name + '"]');
    if (el) el.value = value ?? '';
  };
  document.addEventListener('change', async (e) => {
    const sel = e.target.closest('[data-copy-process],[data-copy-script]');
    if (!sel) return;
    const process = sel.hasAttribute('data-copy-process');
    let data = null;
    if (sel.value) {
      try { data = await (await fetch('/admin/eggs/' + encodeURIComponent(sel.value) + '/copy-data', { headers: { Accept: 'application/json' } })).json(); } catch (_) { return; }
    }
    if (process) {
      setField('copy_script_from', sel.value);
      setField('config_stop', data ? data.config_stop : '');
      setField('config_startup', data ? data.config_startup : '{}');
      setField('config_files', data ? data.config_files : '{}');
      setField('config_logs', data ? data.config_logs : '{}');
    } else {
      setField('script_container', data ? data.script_container : 'ghcr.io/pelican-eggs/installers:debian');
      setField('script_entry', data ? data.script_entry : 'bash');
      const script = data ? data.script_install || '' : '';
      if (editor) editor.setValue(script); else setField('script_install', script);
    }
  });
})();

// Admin › Plugins: README slide-over and import-from-file multipart submit
// (multipart needs the X-CSRF-TOKEN header; the CSRF middleware only buffers
// urlencoded bodies), plus drag-and-drop load-order reordering.
(function () {
  'use strict';

  const csrf = () => document.querySelector('meta[name="csrf-token"]')?.content || '';

  // View action of a plugin with a README fetches the slide-over content.
  document.addEventListener('click', (e) => {
    const a = e.target.closest?.('a[href$="/readme"]');
    if (!a || e.defaultPrevented || a.target) return;
    e.preventDefault();
    const dialog = document.getElementById('plugin-readme');
    if (!dialog) return;
    dialog.querySelector('[data-readme-body]').innerHTML =
      '<div class="stack"><span class="muted">…</span></div>';
    dialog.querySelector('[data-readme-url]').hidden = true;
    dialog.showModal();
    fetch(a.getAttribute('href'), {
      headers: { Accept: 'application/json', 'X-CSRF-TOKEN': csrf() },
    })
      .then((r) => r.json())
      .then((d) => {
        dialog.querySelector('[data-readme-title]').textContent = d.title || '';
        dialog.querySelector('[data-readme-body]').innerHTML = d.html;
        const url = dialog.querySelector('[data-readme-url]');
        if (d.url) {
          url.href = d.url;
          url.hidden = false;
        }
      })
      .catch(() => dialog.close());
  });

  // Import from file posts multipart with the session token in a header.
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

  // Load-order reordering: toggle makes the rows draggable; dropping applies
  // the new sequence through the reorder endpoint.
  let reordering = false;

  document.addEventListener('click', (e) => {
    const btn = e.target.closest?.('[data-reorder]');
    if (!btn || e.defaultPrevented) return;
    reordering = !reordering;
    btn.querySelector('[data-reorder-off]').hidden = reordering;
    btn.querySelector('[data-reorder-on]').hidden = !reordering;
    btn.classList.toggle('btn-success', reordering);
    document.querySelectorAll('#plugins tbody tr').forEach((tr) => {
      tr.draggable = reordering;
      tr.style.opacity = '';
    });
  });

  function applyReorderMode() {
    document.querySelectorAll('#plugins tbody tr').forEach((tr) => {
      tr.draggable = reordering;
      tr.style.opacity = '';
    });
  }

  let dragged = null;

  document.addEventListener('dragstart', (e) => {
    const tr = e.target.closest?.('#plugins tbody tr[draggable="true"]');
    if (!tr) return;
    dragged = tr;
    tr.style.opacity = '0.5';
    e.dataTransfer.effectAllowed = 'move';
    try { e.dataTransfer.setData('text/plain', ''); } catch (_) {}
  });

  document.addEventListener('dragover', (e) => {
    const tr = e.target.closest?.('#plugins tbody tr');
    if (!tr || !dragged || tr === dragged) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    const rows = [...tr.parentElement.children];
    const rect = tr.getBoundingClientRect();
    const before = (e.clientY - rect.top) < rect.height / 2;
    if (before) tr.before(dragged); else tr.after(dragged);
    void rows;
  });

  document.addEventListener('dragend', () => {
    if (!dragged) return;
    dragged.style.opacity = '';
    dragged = null;
    if (!reordering) return;
    const ids = [...document.querySelectorAll('#plugins tbody [data-plugin-row]')]
      .map((el) => el.getAttribute('data-plugin-row'))
      .filter(Boolean);
    if (!ids.length) return;
    const body = new URLSearchParams();
    ids.forEach((id) => body.append('ids[]', id));
    body.append('_token', csrf());
    body.append('_back', window.location.pathname + window.location.search);
    fetch('/admin/plugins/reorder', {
      method: 'POST',
      headers: { 'X-CSRF-TOKEN': csrf() },
      body,
    })
      .then(() => window.location.reload())
      .catch(() => window.location.reload());
  });
})();

// Server › Network: inline-editable notes column (Filament TextInputColumn).
// The notes <td> of each row becomes an input when the row carries a
// make-primary action form (that form is rendered exactly for users with
// `allocation.update`); the allocation id is taken from the action URL, so the
// binding survives htmx boost swaps. Changes POST to /network/{id}/notes and
// revert + notify on errors, like Filament's inline editing.
(function () {
  'use strict';

  // NOTES_COLUMN is the position of the notes cell in each row
  // (address, port, notes, primary, locked[, actions]).
  var NOTES_COLUMN = 2;

  function init() {
    var table = document.getElementById('network');
    if (!table) return;
    var meta = document.querySelector('meta[name="csrf-token"]');
    var token = meta ? meta.getAttribute('content') : '';
    table.querySelectorAll('table.table tbody tr').forEach(function (tr) {
      var td = tr.children[NOTES_COLUMN];
      if (!td || td.dataset.notesBound) return;
      // Row identity and permission: only rows with a /primary (or /delete)
      // action form carry their allocation id.
      var id = null;
      var base = null;
      tr.querySelectorAll('form[action]').forEach(function (f) {
        var url = f.getAttribute('action');
        var m = url.match(/^(.*\/network\/)(\d+)\//);
        if (m && id === null) {
          base = m[1];
          id = m[2];
        }
      });
      if (id === null) return;
      td.dataset.notesBound = '1';
      // Empty cells render the "No Notes" placeholder as their title.
      var empty = td.textContent.trim() === '';
      var placeholder = '';
      var wrap = td.querySelector('[title]');
      if (wrap) placeholder = wrap.getAttribute('title') || '';
      var value = td.textContent.trim();
      var input = document.createElement('input');
      input.type = 'text';
      input.className = 'input';
      input.style.width = '100%';
      input.value = value;
      input.placeholder = placeholder;
      input.setAttribute('aria-label', placeholder || 'Notes');
      input.dataset.original = value;
      input.addEventListener('change', function () { save(base + id + '/notes', token, input); });
      td.textContent = '';
      td.appendChild(input);
      if (empty) td.title = placeholder;
    });
  }

  async function save(url, token, input) {
    var original = input.dataset.original;
    var value = input.value;
    if (value === original) return;
    try {
      var res = await fetch(url, {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'X-CSRF-TOKEN': token },
        body: new URLSearchParams({ notes: value }),
      });
      if (res.ok) { input.dataset.original = value; return; }
      var message = (window.PELICAN_STRINGS || {}).error || 'Error';
      try { message = (await res.json()).message || message; } catch (_) { /* no body */ }
      window.notify({ status: 'danger', title: message });
      input.value = original;
    } catch (_) {
      window.notify({ status: 'danger', title: (window.PELICAN_STRINGS || {}).error || 'Error' });
      input.value = original;
    }
  }

  init();
  document.addEventListener('DOMContentLoaded', init);
  document.body.addEventListener('htmx:afterSwap', init);
})();

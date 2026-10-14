// Server › Databases glue: reopen the create-database dialog when the page
// was re-rendered after a failed submit (validation errors inside the dialog).
(function () {
  'use strict';
  document.addEventListener('DOMContentLoaded', () => {
    const d = document.getElementById('create-database');
    if (d?.querySelector('.has-error') && typeof d.showModal === 'function' && !d.open) d.showModal();
  });
})();

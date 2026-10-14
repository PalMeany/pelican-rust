// Server › Backups glue: opens the per-row action dialog requested through
// GET (`data-dialog-open`) and reopens dialogs after a validation-failure
// re-render, like users.js.
(function () {
  'use strict';

  function open(d) {
    if (d && !d.open && typeof d.showModal === 'function') d.showModal();
  }

  document.addEventListener('DOMContentLoaded', () => {
    open(document.querySelector('dialog[data-dialog-open]'));
    // Reopen the create dialog when its validation failed.
    const create = document.getElementById('create-backup');
    if (create?.querySelector('.has-error')) open(create);
  });
})();

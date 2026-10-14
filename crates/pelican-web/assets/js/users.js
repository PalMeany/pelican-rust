// Server › Users glue: "Assign all" in a dialog selects every permission
// checkbox; dialogs re-opened after navigation or a failed invite submit.
(function () {
  'use strict';
  document.addEventListener('click', (e) => {
    const b = e.target.closest('[data-assign-all]');
    if (!b) return;
    e.preventDefault();
    b.closest('form')?.querySelectorAll('input[type="checkbox"][name^="perm_"]').forEach((c) => {
      c.checked = true;
    });
  });
  document.addEventListener('DOMContentLoaded', () => {
    // EditAction entry point (`GET /users/{id}` marks its dialog).
    const edit = document.querySelector('dialog[data-dialog-open]');
    if (edit && !edit.open && typeof edit.showModal === 'function') edit.showModal();
    // Reopen the invite dialog after a validation-failure re-render.
    const invite = document.getElementById('invite-user');
    if (invite?.querySelector('.has-error') && !invite.open && typeof invite.showModal === 'function') {
      invite.showModal();
    }
  });
})();

// Server › Settings glue: live fields submit like Filament's
// live(onBlur)/live() (name/description on blur, toggles on change), and the
// icon file upload posts its multipart form with an X-CSRF-TOKEN header
// (the CSRF middleware only buffers urlencoded bodies).
(function () {
  'use strict';

  document.addEventListener('change', function (e) {
    var form = e.target.closest('form[data-live]');
    if (!form || !e.target.matches('input[type="checkbox"], select')) return;
    if (!e.target.disabled) form.requestSubmit();
  });

  // live(onBlur: true): post on every blur of an enabled field.
  document.addEventListener('focusout', function (e) {
    var form = e.target.closest('form[data-live]');
    if (!form || !e.target.matches('input[type="text"], input[type="url"], textarea')) return;
    if (e.target.disabled || e.target.readOnly) return;
    form.requestSubmit();
  });

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
})();

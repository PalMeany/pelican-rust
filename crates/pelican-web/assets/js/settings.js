// Server › Settings glue: live fields submit like Filament's
// live(onBlur)/live() (name/description on blur, toggles on change).
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

})();

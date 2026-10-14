// Server › Startup glue: live fields submit like Filament's live()/live(onBlur):
// selects/toggles on change, text inputs on every blur (matches wire:model.blur
// semantics); PreviewStartupAction toggles the readonly textarea between the
// raw and the resolved (StartupCommandService::handle($server)) command.
(function () {
  'use strict';

  document.addEventListener('change', function (e) {
    var form = e.target.closest('form[data-live]');
    if (!form) return;
    if (e.target.matches('select') || e.target.matches('input[type="checkbox"]')) {
      if (!e.target.disabled) form.requestSubmit();
    }
  });

  // StartupVariable live(onBlur: true): post on every blur of an enabled field,
  // exactly like wire:model.blur would.
  document.addEventListener('focusout', function (e) {
    var form = e.target.closest('form[data-live]');
    if (!form || !e.target.matches('input[type="text"], input[type="number"]')) return;
    if (e.target.disabled || e.target.readOnly) return;
    form.requestSubmit();
  });

  document.addEventListener('click', function (e) {
    var btn = e.target.closest('[data-preview]');
    if (!btn) return;
    var area = document.getElementById('f-startup-command');
    if (!area) return;
    var previewing = btn.getAttribute('aria-pressed') === 'true';
    if (previewing) {
      area.value = area.dataset.raw;
      area.removeAttribute('data-previewing');
      btn.querySelector('[data-preview-label]').textContent = btn.dataset.offLabel;
      btn.setAttribute('aria-pressed', 'false');
    } else {
      area.value = area.dataset.previewValue;
      area.setAttribute('data-previewing', '');
      btn.querySelector('[data-preview-label]').textContent = btn.dataset.onLabel;
      btn.setAttribute('aria-pressed', 'true');
    }
  });
})();

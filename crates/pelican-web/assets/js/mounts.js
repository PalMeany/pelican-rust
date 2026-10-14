// Server › Mounts glue: the CheckboxList is live in Filament — every toggle
// saves immediately (Mounts::afterStateUpdated → save()); bulkToggleable adds
// the select-all / deselect-all pair that also saves.
(function () {
  'use strict';

  document.addEventListener('change', function (e) {
    var form = e.target.closest('form[data-live]');
    if (!form) return;
    if (e.target.matches('input[type="checkbox"]') && !e.target.disabled) {
      form.requestSubmit();
    }
  });

  // BulkToggleable: one button that flips between select-all and deselect-all,
  // then submits the form once (one save like a single real change).
  document.addEventListener('click', function (e) {
    var btn = e.target.closest('[data-toggle-all]');
    if (!btn) return;
    var form = btn.closest('form');
    if (!form) return;
    var boxes = Array.prototype.filter.call(
      form.querySelectorAll('input[type="checkbox"][name="mounts[]"]'),
      function (b) { return !b.disabled; }
    );
    if (!boxes.length) return;
    var allChecked = boxes.every(function (b) { return b.checked; });
    boxes.forEach(function (b) { b.checked = !allChecked; });
    var label = btn.querySelector('[data-toggle-all-label]');
    if (label) label.textContent = allChecked ? btn.dataset.checkLabel : btn.dataset.uncheckLabel;
    form.requestSubmit();
  });
})();

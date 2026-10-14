// Admin › Settings glue: the Filament enable/disable actions toggle hidden
// boolean fields, the test-mail action is hidden for the `log` mailer, and
// the Trusted Proxies hint actions (clear / Cloudflare IPs) rewrite the
// TagsInput widget client-side.
(function () {
  'use strict';

  // [data-set-name]/[data-set-value]: flip one hidden boolean input and re-run
  // visibility rules via the shared change listener (enable/disable actions).
  document.addEventListener('click', function (e) {
    var b = e.target.closest('[data-set-name]');
    if (!b) return;
    e.preventDefault();
    if (b.closest('fieldset') && b.closest('fieldset').disabled) return;
    var inputs = document.querySelectorAll('input[type="hidden"][name="' + b.dataset.setName.replace(/[^\w-]/g, '\\$&') + '"]');
    inputs.forEach(function (i) {
      if (i.disabled) return;
      i.value = b.dataset.setValue;
      i.dispatchEvent(new Event('change', { bubbles: true }));
    });
    // Show only the matching enable/disable button of that group.
    b.parentElement.querySelectorAll('[data-set-name="' + b.dataset.setName + '"]').forEach(function (x) { x.hidden = x !== b; });
  });

  // "Send Test Mail" is visible for every mailer but `log` (hint action).
  function applyMailerTest() {
    var mailer = '';
    document.querySelectorAll('input[name="MAIL_MAILER"]').forEach(function (r) { if (r.checked) mailer = r.value; });
    var btn = document.querySelector('[data-mailer-test]');
    if (btn) btn.hidden = mailer === 'log';
  }
  document.addEventListener('change', function (e) {
    if (e.target.name === 'MAIL_MAILER') applyMailerTest();
  });
  document.addEventListener('DOMContentLoaded', applyMailerTest);

  // Rewrite the TagsInput of a `.field`: replace chips and the hidden JSON value.
  function setTags(field, values) {
    var widget = field.querySelector('[data-tags]');
    if (!widget) return;
    var hidden = widget.querySelector('input[type="hidden"]');
    if (hidden) hidden.value = JSON.stringify(values);
    widget.querySelectorAll('.chip').forEach(function (c) { c.remove(); });
    var entry = widget.querySelector('.tags-entry');
    values.forEach(function (v) {
      var chip = document.createElement('span');
      chip.className = 'chip';
      chip.setAttribute('data-tag', v);
      chip.textContent = v;
      var rm = document.createElement('button');
      rm.type = 'button';
      rm.setAttribute('data-tag-remove', '');
      rm.setAttribute('aria-label', '×');
      rm.textContent = '×';
      chip.appendChild(rm);
      widget.insertBefore(chip, entry);
    });
  }

  // "Clear": confirmation like the hint action, then empty the tags.
  document.addEventListener('click', function (e) {
    var b = e.target.closest('[data-tags-clear]');
    if (!b) return;
    e.preventDefault();
    if (b.closest('fieldset') && b.closest('fieldset').disabled) return;
    setTags(b.closest('.field'), []);
  });

  // "Set to Cloudflare IPs": fill in the current Cloudflare address ranges.
  document.addEventListener('click', function (e) {
    var b = e.target.closest('[data-tags-cloudflare]');
    if (!b) return;
    e.preventDefault();
    if (b.closest('fieldset') && b.closest('fieldset').disabled) return;
    fetch('/admin/settings/cloudflare-ips', { headers: { Accept: 'application/json' } })
      .then(function (r) { return r.ok ? r.json() : []; })
      .then(function (ips) { setTags(b.closest('.field'), ips); })
      .catch(function () { setTags(b.closest('.field'), []); });
  });
})();

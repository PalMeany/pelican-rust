# 0017. Passkeys: собственная проверка WebAuthn в формате webauthn-lib

**Статус:** Accepted

**Контекст.** Оригинал хранит passkeys через `laravel/passkeys` 0.2 поверх `web-auth/webauthn-lib` 5.3:
в `passkeys.credential` лежит JSON `CredentialRecord` (base64url идентификатора/COSE-ключа/user handle,
`trustPath: []`, `aaguid` строкой UUID, флаги backup/UV), user handle — HMAC-SHA256(`users|<id>`, APP_KEY
как строка). Rust-крейты WebAuthn (webauthn-rs) хранят свой формат и не читают эти записи.

**Решение.**
- `pelican_core::webauthn` — минимальная реализация церемоний: разбор clientDataJSON/authData/CBOR,
  ES256 (p256) и RS256 (rsa), только аттестация "none" (как `WebAuthn::attestationStatementSupportManager`),
  шаги `CeremonyStepManagerFactory` в том же порядке и с теми же сообщениями (тип, challenge, origin с
  запретом поддоменов, topOrigin, хеш rpId, UP/UV, биты backup, алгоритм, длина id, подпись, счётчик).
- Конфигурация — как `config/passkeys.php` панели (`PASSKEYS_RELYING_PARTY_ID`, `PASSKEYS_ALLOWED_ORIGINS`,
  `PASSKEYS_STRICT_ORIGIN`, `PASSKEYS_USER_HANDLE_SECRET`) + middleware `AllowPasskeyOrigin`.
- Маршруты пакета (`/passkeys/login{,/options}`, `/user/passkeys{,/options}`, `DELETE /user/passkeys/{id}`),
  опции церемонии в сессии, throttle 6/мин, ошибки — JSON панели с `message` (`AddPasskeyErrorMessage`,
  `Handler::invalidPasskey`). UI (вкладка профиля, кнопка входа) — только при `isSecure()`, как в оригинале.
- Совместимость проверена кросс-тестом `scripts/crosscheck/passkeys.php`: webauthn-lib в контейнере оригинала
  принимает нашу регистрацию и строит тот же `CredentialRecord`, проверяет наш assertion по нашей записи.

**Последствия.** Passkeys, созданные в оригинале, работают в Rust-панели и наоборот. Аттестации кроме
"none" (packed/tpm/…) отклоняются так же, как в оригинале. Маршруты подтверждения паролем через passkey
(`/passkeys/confirm*`) не перенесены: панель их не использует.

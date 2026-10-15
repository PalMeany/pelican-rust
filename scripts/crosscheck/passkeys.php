<?php
// Cross-check of pelican_core::webauthn against laravel/passkeys + webauthn-lib, run in the
// original panel's container with the fixture of `webauthn::tests::fixture` on stdin.
require 'vendor/autoload.php';
$app = require 'bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();

use Laravel\Passkeys\Support\WebAuthn;
use Webauthn\CredentialRecord;
use Webauthn\PublicKeyCredential;
use Webauthn\PublicKeyCredentialCreationOptions;
use Webauthn\PublicKeyCredentialRequestOptions;

$f = json_decode(stream_get_contents(STDIN), true, flags: JSON_THROW_ON_ERROR);
config(['passkeys.relying_party_id' => $f['rp_id'], 'passkeys.allowed_origins' => [$f['origin']]]);

$creation = WebAuthn::fromJson(json_encode($f['creation_options']), PublicKeyCredentialCreationOptions::class);
$cred = WebAuthn::fromJson(json_encode($f['registration']), PublicKeyCredential::class);
$record = WebAuthn::attestationValidator()->check($cred->response, $creation, $f['rp_id']);
$php = json_decode(WebAuthn::toJson($record), true, flags: JSON_THROW_ON_ERROR);
echo 'registration: ', $php === $f['stored'] ? 'same record' : 'DIFFERENT '.json_encode($php), "\n";

$request = WebAuthn::fromJson(json_encode($f['request_options']), PublicKeyCredentialRequestOptions::class);
$assertion = WebAuthn::fromJson(json_encode($f['assertion']), PublicKeyCredential::class);
$stored = WebAuthn::fromJson(json_encode($f['stored']), CredentialRecord::class);
$after = WebAuthn::assertionValidator()->check($stored, $assertion->response, $request, $f['rp_id'], $stored->userHandle);
echo 'assertion: ok, counter ', $after->counter, "\n";

//! Passkeys compatible with `laravel/passkeys` 0.2 on `web-auth/webauthn-lib` 5.3:
//! the stored `passkeys.credential` JSON (`CredentialRecord`), the browser-facing
//! options, registration with "none" attestation and assertions (ES256 / RS256),
//! running the library's ceremony steps in its order (`CeremonyStepManagerFactory`).

use base64::Engine;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const ES256: i64 = -7;
pub const RS256: i64 = -257;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    /// The browser payload could not be read (`Invalid credential format.`).
    #[error("{0}")]
    Format(String),
    /// `AuthenticatorResponseVerificationException` with the library's message.
    #[error("{0}")]
    Verification(String),
}

impl Error {
    /// `Handler::PASSKEY_ORIGIN_FAILURES`: shown as `passkeys.invalid_origin`.
    pub fn is_origin_failure(&self) -> bool {
        matches!(self, Error::Verification(m) if m.starts_with("Invalid origin") || m.starts_with("Invalid scheme"))
    }
}

fn verification(msg: &str) -> Error {
    Error::Verification(msg.into())
}

fn format(msg: &str) -> Error {
    Error::Format(msg.into())
}

pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// `Webauthn\Util\Base64::decode`: url-safe or standard alphabet, padding optional.
pub fn decode_b64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    URL_SAFE_NO_PAD.decode(s).or_else(|_| STANDARD_NO_PAD.decode(s)).ok()
}

/// `PasskeyAuthenticatable::getPasskeyUserHandle`: HMAC-SHA256 of `users|<id>` keyed with
/// `passkeys.user_handle_secret` (`PASSKEYS_USER_HANDLE_SECRET`, else the raw `APP_KEY` string).
pub fn user_handle(user_id: i64, secret: &str) -> Vec<u8> {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else { return Vec::new() };
    mac.update(format!("users|{user_id}").as_bytes());
    mac.finalize().into_bytes().to_vec()
}

// ----- origins (config/passkeys.php, PasskeyOrigin, AllowPasskeyOrigin) -----------------

/// `(scheme, host, port)` of a URL like PHP's `parse_url` would see it.
fn split_url(url: &str) -> Option<(String, String, Option<u16>)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = if let Some(end) = authority.strip_prefix('[').and_then(|a| a.find(']')) {
        let host = &authority[..end + 2];
        let port = authority[end + 2..].strip_prefix(':');
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if host.is_empty() || scheme.is_empty() {
        return None;
    }
    let port = match port {
        Some(p) if !p.is_empty() => Some(p.parse().ok()?),
        _ => None,
    };
    Some((scheme.to_string(), host.to_string(), port))
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

/// `PasskeyOrigin::normalize`: lower-cased `scheme://host[:port]`, bare hosts are https.
pub fn normalize_origin(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let url = if url.contains("://") { url.to_string() } else { format!("https://{url}") };
    let (scheme, host, port) = split_url(&url)?;
    let scheme = scheme.to_lowercase();
    let mut origin = format!("{scheme}://{}", host.to_lowercase());
    if let Some(p) = port.filter(|p| Some(*p) != default_port(&scheme)) {
        origin.push_str(&format!(":{p}"));
    }
    Some(origin)
}

pub fn host_of(origin: &str) -> String {
    split_url(origin).map(|(_, h, _)| h).unwrap_or_default()
}

/// Relying party and origins from `config/passkeys.php`.
#[derive(Clone, Debug)]
pub struct Party {
    pub rp_id: String,
    pub allowed_origins: Vec<String>,
    pub strict_origin: bool,
    pub user_handle_secret: String,
    pub timeout: u64,
}

impl Party {
    pub fn from_env(env: &crate::env::Env) -> Party {
        let app_origin =
            normalize_origin(&env.string_or("APP_URL", "")).unwrap_or_else(|| "http://localhost".to_string());
        let rp =
            normalize_origin(&env.string_or("PASSKEYS_RELYING_PARTY_ID", "")).unwrap_or_else(|| app_origin.clone());
        let mut origins = vec![app_origin];
        for o in env.string_or("PASSKEYS_ALLOWED_ORIGINS", "").split(',') {
            if let Some(o) = normalize_origin(o) {
                if !origins.contains(&o) {
                    origins.push(o);
                }
            }
        }
        let secret = env.string("PASSKEYS_USER_HANDLE_SECRET").unwrap_or_else(|| env.string_or("APP_KEY", ""));
        Party {
            rp_id: host_of(&rp),
            allowed_origins: origins,
            strict_origin: env.bool_or("PASSKEYS_STRICT_ORIGIN", false),
            user_handle_secret: secret,
            timeout: 60000,
        }
    }

    /// `AllowPasskeyOrigin`: also accept the request's own origin when it is the relying
    /// party's host over https (or http on a loopback host).
    pub fn for_request(&self, origin_header: Option<&str>, scheme_and_host: &str, request_host: &str) -> Party {
        let mut party = self.clone();
        if self.strict_origin {
            return party;
        }
        let Some(origin) = normalize_origin(origin_header.unwrap_or(scheme_and_host)) else { return party };
        let Some((scheme, host, _)) = split_url(&origin) else { return party };
        let loopback = ["localhost", "127.0.0.1", "[::1]"].contains(&host.as_str());
        if host == self.rp_id && host == request_host && (scheme == "https" || (scheme == "http" && loopback)) {
            if !party.allowed_origins.contains(&origin) {
                party.allowed_origins.push(origin);
            }
        }
        party
    }
}

/// `CheckAllowedOrigins` with every allowed entry already a full origin.
fn check_origin(origin: &str, allowed: &[String]) -> Result<(), Error> {
    if allowed.iter().any(|a| a == origin) {
        return Ok(());
    }
    let (scheme, host, port) =
        split_url(origin).ok_or_else(|| verification("Invalid origin. Unable to parse the origin."))?;
    let built = match port.filter(|p| Some(*p) != default_port(&scheme)) {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    };
    if allowed.contains(&built) {
        return Ok(());
    }
    let subdomain = allowed
        .iter()
        .filter_map(|a| split_url(a))
        .any(|(s, h, p)| s == scheme && p == port && host.len() > h.len() && host.ends_with(&format!(".{h}")));
    if subdomain {
        return Err(verification("Invalid origin. Subdomains are not allowed."));
    }
    Err(verification("Invalid origin. Not in the list of allowed origins."))
}

// ----- options -------------------------------------------------------------------------

/// `PublicKeyCredentialCreationOptions` of `GenerateRegistrationOptions`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CreationOptions {
    pub rp_id: String,
    pub user_id: Vec<u8>,
    pub user_name: String,
    pub user_display_name: String,
    pub challenge: Vec<u8>,
    pub exclude: Vec<Vec<u8>>,
    pub timeout: u64,
}

impl CreationOptions {
    pub fn new(party: &Party, user_id: i64, email: &str, username: &str, exclude: Vec<Vec<u8>>) -> CreationOptions {
        CreationOptions {
            rp_id: party.rp_id.clone(),
            user_id: user_handle(user_id, &party.user_handle_secret),
            user_name: email.to_string(),
            user_display_name: if username.is_empty() { email.to_string() } else { username.to_string() },
            challenge: random_challenge(),
            exclude,
            timeout: party.timeout,
        }
    }

    /// `WebAuthn::toBrowserArray($options)`.
    pub fn to_browser(&self) -> Value {
        let mut m = IndexMap::new();
        m.insert("challenge", json!(b64url(&self.challenge)));
        m.insert("timeout", json!(self.timeout));
        m.insert("rp", json!({ "id": self.rp_id, "name": self.rp_id }));
        m.insert(
            "user",
            json!({ "id": b64url(&self.user_id), "name": self.user_name, "displayName": self.user_display_name }),
        );
        m.insert(
            "pubKeyCredParams",
            json!([{ "type": "public-key", "alg": ES256 }, { "type": "public-key", "alg": RS256 }]),
        );
        m.insert(
            "authenticatorSelection",
            json!({ "userVerification": "required", "residentKey": "required", "requireResidentKey": true }),
        );
        m.insert("attestation", json!("none"));
        m.insert("excludeCredentials", descriptors(&self.exclude));
        json!(m)
    }
}

/// `PublicKeyCredentialRequestOptions` of `GenerateVerificationOptions`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RequestOptions {
    pub rp_id: String,
    pub challenge: Vec<u8>,
    pub allow: Vec<Vec<u8>>,
    pub timeout: u64,
}

impl RequestOptions {
    pub fn new(party: &Party, allow: Vec<Vec<u8>>) -> RequestOptions {
        RequestOptions { rp_id: party.rp_id.clone(), challenge: random_challenge(), allow, timeout: party.timeout }
    }

    pub fn to_browser(&self) -> Value {
        let mut m = IndexMap::new();
        m.insert("challenge", json!(b64url(&self.challenge)));
        m.insert("timeout", json!(self.timeout));
        m.insert("rpId", json!(self.rp_id));
        m.insert("allowCredentials", descriptors(&self.allow));
        m.insert("userVerification", json!("required"));
        json!(m)
    }
}

fn descriptors(ids: &[Vec<u8>]) -> Value {
    Value::Array(ids.iter().map(|id| json!({ "type": "public-key", "id": b64url(id) })).collect())
}

fn random_challenge() -> Vec<u8> {
    use rand::RngCore;
    let mut c = vec![0u8; 32];
    rand::rng().fill_bytes(&mut c);
    c
}

// ----- stored credential ---------------------------------------------------------------

/// `Webauthn\CredentialRecord` as `CredentialRecordDenormalizer` (de)serializes it.
#[derive(Clone, Debug, PartialEq)]
pub struct CredentialRecord {
    pub public_key_credential_id: Vec<u8>,
    pub kind: String,
    pub transports: Vec<String>,
    pub attestation_type: String,
    /// `[]` (EmptyTrustPath) or `{"x5c": [...]}`, kept as stored.
    pub trust_path: Value,
    pub aaguid: String,
    pub credential_public_key: Vec<u8>,
    pub user_handle: Vec<u8>,
    pub counter: u64,
    pub other_ui: Option<Value>,
    pub backup_eligible: Option<bool>,
    pub backup_status: Option<bool>,
    pub uv_initialized: Option<bool>,
}

impl CredentialRecord {
    pub fn from_json(v: &Value) -> Result<CredentialRecord, Error> {
        let bytes = |k: &str| -> Result<Vec<u8>, Error> {
            v.get(k).and_then(Value::as_str).and_then(decode_b64).ok_or_else(|| format(&format!("Missing {k}")))
        };
        let string = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Ok(CredentialRecord {
            public_key_credential_id: bytes("publicKeyCredentialId")?,
            kind: string("type").unwrap_or_else(|| "public-key".into()),
            transports: v
                .get("transports")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            attestation_type: string("attestationType").unwrap_or_else(|| "none".into()),
            trust_path: v.get("trustPath").cloned().unwrap_or_else(|| json!([])),
            aaguid: string("aaguid").unwrap_or_else(|| "00000000-0000-0000-0000-000000000000".into()),
            credential_public_key: bytes("credentialPublicKey")?,
            user_handle: bytes("userHandle")?,
            counter: v.get("counter").and_then(Value::as_u64).unwrap_or(0),
            other_ui: v.get("otherUI").filter(|o| !o.is_null()).cloned(),
            backup_eligible: v.get("backupEligible").and_then(Value::as_bool),
            backup_status: v.get("backupStatus").and_then(Value::as_bool),
            uv_initialized: v.get("uvInitialized").and_then(Value::as_bool),
        })
    }

    /// `json_decode(WebAuthn::toJson($source), true)`: key order of the normalizer, nulls dropped.
    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("publicKeyCredentialId".into(), json!(b64url(&self.public_key_credential_id)));
        m.insert("type".into(), json!(self.kind));
        m.insert("transports".into(), json!(self.transports));
        m.insert("attestationType".into(), json!(self.attestation_type));
        m.insert("trustPath".into(), self.trust_path.clone());
        m.insert("aaguid".into(), json!(self.aaguid));
        m.insert("credentialPublicKey".into(), json!(b64url(&self.credential_public_key)));
        m.insert("userHandle".into(), json!(b64url(&self.user_handle)));
        m.insert("counter".into(), json!(self.counter));
        if let Some(o) = &self.other_ui {
            m.insert("otherUI".into(), o.clone());
        }
        for (k, v) in [
            ("backupEligible", self.backup_eligible),
            ("backupStatus", self.backup_status),
            ("uvInitialized", self.uv_initialized),
        ] {
            if let Some(b) = v {
                m.insert(k.into(), json!(b));
            }
        }
        Value::Object(m)
    }
}

// ----- browser responses ---------------------------------------------------------------

struct ClientData {
    raw: Vec<u8>,
    kind: String,
    challenge: Vec<u8>,
    origin: String,
    cross_origin: bool,
    top_origin: Option<String>,
}

fn client_data(b64: Option<&Value>) -> Result<ClientData, Error> {
    let raw = b64.and_then(Value::as_str).and_then(decode_b64).ok_or_else(|| format("Invalid clientDataJSON"))?;
    let v: Value = serde_json::from_slice(&raw).map_err(|_| format("Invalid clientDataJSON"))?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(ClientData {
        kind: s("type").ok_or_else(|| format("Invalid clientDataJSON"))?,
        challenge: s("challenge").as_deref().and_then(decode_b64).ok_or_else(|| format("Invalid clientDataJSON"))?,
        origin: s("origin").ok_or_else(|| format("Invalid clientDataJSON"))?,
        cross_origin: v.get("crossOrigin").and_then(Value::as_bool).unwrap_or(false),
        top_origin: s("topOrigin"),
        raw,
    })
}

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_BE: u8 = 0x08;
const FLAG_BS: u8 = 0x10;
const FLAG_AT: u8 = 0x40;
const FLAG_ED: u8 = 0x80;

struct AuthData {
    raw: Vec<u8>,
    rp_id_hash: [u8; 32],
    flags: u8,
    sign_count: u64,
    /// `(aaguid, credential id, COSE key bytes)`.
    attested: Option<([u8; 16], Vec<u8>, Vec<u8>)>,
}

fn auth_data(raw: &[u8]) -> Result<AuthData, Error> {
    let bad = || format("Invalid authenticator data");
    if raw.len() < 37 {
        return Err(bad());
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&raw[..32]);
    let flags = raw[32];
    let sign_count = u32::from_be_bytes([raw[33], raw[34], raw[35], raw[36]]) as u64;
    let mut rest = &raw[37..];
    let mut attested = None;
    if flags & FLAG_AT != 0 {
        if rest.len() < 18 {
            return Err(bad());
        }
        let mut aaguid = [0u8; 16];
        aaguid.copy_from_slice(&rest[..16]);
        let len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
        rest = &rest[18..];
        if rest.len() < len {
            return Err(bad());
        }
        let id = rest[..len].to_vec();
        rest = &rest[len..];
        let before = rest.len();
        let mut reader = rest;
        let _: ciborium::Value = ciborium::de::from_reader(&mut reader).map_err(|_| bad())?;
        let used = before - reader.len();
        attested = Some((aaguid, id, rest[..used].to_vec()));
        rest = reader;
    }
    if flags & FLAG_ED != 0 {
        let mut reader = rest;
        let _: ciborium::Value = ciborium::de::from_reader(&mut reader).map_err(|_| bad())?;
        rest = reader;
    }
    if !rest.is_empty() {
        return Err(bad());
    }
    Ok(AuthData { raw: raw.to_vec(), rp_id_hash, flags, sign_count, attested })
}

fn uuid_string(b: &[u8; 16]) -> String {
    uuid::Uuid::from_bytes(*b).hyphenated().to_string()
}

/// Credential id and response object of a `PublicKeyCredential` JSON (`id` must equal `rawId`).
fn credential_parts(cred: &Value) -> Result<(Vec<u8>, &serde_json::Map<String, Value>), Error> {
    let invalid = || format("Invalid credential format.");
    if cred.get("type").and_then(Value::as_str) != Some("public-key") {
        return Err(invalid());
    }
    let raw_id = cred.get("rawId").and_then(Value::as_str).and_then(decode_b64).ok_or_else(invalid)?;
    let id = cred.get("id").and_then(Value::as_str).and_then(decode_b64).ok_or_else(invalid)?;
    if id != raw_id {
        return Err(invalid());
    }
    let response = cred.get("response").and_then(Value::as_object).ok_or_else(invalid)?;
    Ok((raw_id, response))
}

/// The raw credential id of an assertion (`Base64UrlSafe::encodeUnpadded($credential->rawId)` looks it up).
pub fn assertion_credential_id(cred: &Value) -> Result<Vec<u8>, Error> {
    credential_parts(cred).map(|(id, _)| id)
}

// ----- shared ceremony steps -----------------------------------------------------------

fn common_steps(
    cd: &ClientData,
    ad: &AuthData,
    expected_type: &str,
    challenge: &[u8],
    rp_id: &str,
    allowed_origins: &[String],
) -> Result<(), Error> {
    // CheckClientDataCollectorType
    if cd.kind != expected_type {
        return Err(verification(&format!("The client data type is not \"{expected_type}\".")));
    }
    // CheckChallenge
    if challenge.is_empty() || !constant_eq(challenge, &cd.challenge) {
        return Err(verification("Invalid challenge."));
    }
    // CheckAllowedOrigins
    check_origin(&cd.origin, allowed_origins)?;
    // CheckTopOrigin (no TopOriginValidator configured)
    if cd.top_origin.is_some() {
        if !cd.cross_origin {
            return Err(verification("The response is not cross-origin."));
        }
        return Err(verification("A cross-origin response was received but no TopOriginValidator is configured."));
    }
    // CheckRelyingPartyIdIdHash
    let expected: [u8; 32] = Sha256::digest(rp_id.as_bytes()).into();
    if !constant_eq(&expected, &ad.rp_id_hash) {
        return Err(verification("rpId hash mismatch."));
    }
    // CheckUserWasPresent
    if ad.flags & FLAG_UP == 0 {
        return Err(verification("User was not present"));
    }
    // CheckUserVerification (both option sets require it)
    if ad.flags & FLAG_UV == 0 {
        return Err(verification("User authentication required."));
    }
    // CheckBackupBitsAreConsistent
    if ad.flags & FLAG_BE == 0 && ad.flags & FLAG_BS != 0 {
        return Err(verification("Backup up bit is set but the backup is not eligible."));
    }
    Ok(())
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

// ----- COSE keys -----------------------------------------------------------------------

enum CoseKey {
    Es256(p256::ecdsa::VerifyingKey),
    Rs256(rsa::RsaPublicKey),
}

fn cose_key(bytes: &[u8]) -> Result<(i64, CoseKey), Error> {
    let bad = || verification("Invalid key.");
    let mut reader = bytes;
    let v: ciborium::Value = ciborium::de::from_reader(&mut reader).map_err(|_| bad())?;
    if !reader.is_empty() {
        return Err(verification("Invalid key. Presence of extra bytes."));
    }
    let map = v.as_map().ok_or_else(bad)?;
    let get = |label: i64| {
        map.iter().find(|(k, _)| k.as_integer().and_then(|i| i64::try_from(i).ok()) == Some(label)).map(|(_, v)| v)
    };
    let int = |label: i64| get(label).and_then(|v| v.as_integer()).and_then(|i| i64::try_from(i).ok());
    let bytes_of = |label: i64| get(label).and_then(|v| v.as_bytes()).cloned();
    let alg = int(3).ok_or_else(bad)?;
    match int(1) {
        // EC2 / P-256
        Some(2) => {
            let (x, y) = (bytes_of(-2).ok_or_else(bad)?, bytes_of(-3).ok_or_else(bad)?);
            if int(-1) != Some(1) || x.len() != 32 || y.len() != 32 {
                return Err(bad());
            }
            let sec1 = [&[0x04][..], &x, &y].concat();
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| bad())?;
            Ok((alg, CoseKey::Es256(key)))
        }
        // RSA
        Some(3) => {
            let n = rsa::BigUint::from_bytes_be(&bytes_of(-1).ok_or_else(bad)?);
            let e = rsa::BigUint::from_bytes_be(&bytes_of(-2).ok_or_else(bad)?);
            let key = rsa::RsaPublicKey::new(n, e).map_err(|_| bad())?;
            Ok((alg, CoseKey::Rs256(key)))
        }
        _ => Err(bad()),
    }
}

fn verify_signature(key_bytes: &[u8], data: &[u8], signature: &[u8]) -> Result<(), Error> {
    use p256::ecdsa::signature::Verifier;
    let (alg, key) = cose_key(key_bytes)?;
    let ok = match (alg, key) {
        (ES256, CoseKey::Es256(k)) => p256::ecdsa::Signature::from_der(signature)
            .map(|s| s.normalize_s().unwrap_or(s))
            .is_ok_and(|s| k.verify(data, &s).is_ok()),
        (RS256, CoseKey::Rs256(k)) => {
            let vk = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(k);
            rsa::pkcs1v15::Signature::try_from(signature).is_ok_and(|s| vk.verify(data, &s).is_ok())
        }
        _ => return Err(verification("Invalid algorithm identifier. Should refer to a signature algorithm")),
    };
    if ok { Ok(()) } else { Err(verification("Invalid signature.")) }
}

// ----- ceremonies ----------------------------------------------------------------------

/// `StorePasskey::validate`: `AuthenticatorAttestationResponseValidator::check` with only
/// the "none" attestation statement supported.
pub fn verify_registration(
    cred: &Value,
    options: &CreationOptions,
    allowed_origins: &[String],
) -> Result<CredentialRecord, Error> {
    let (_, response) = credential_parts(cred)?;
    let invalid = || format("Invalid credential format.");
    if !response.contains_key("attestationObject") {
        return Err(verification("Unable to register passkey. Please try again."));
    }
    let cd = client_data(response.get("clientDataJSON"))?;
    let att = response.get("attestationObject").and_then(Value::as_str).and_then(decode_b64).ok_or_else(invalid)?;
    let att: ciborium::Value = ciborium::de::from_reader(att.as_slice()).map_err(|_| invalid())?;
    let att = att.as_map().ok_or_else(invalid)?;
    let field = |k: &str| att.iter().find(|(key, _)| key.as_text() == Some(k)).map(|(_, v)| v);
    let fmt = field("fmt").and_then(|v| v.as_text()).ok_or_else(invalid)?.to_string();
    let att_stmt = field("attStmt").and_then(|v| v.as_map()).ok_or_else(invalid)?;
    let ad = auth_data(field("authData").and_then(|v| v.as_bytes()).ok_or_else(invalid)?)?;
    // AttestationStatementSupportManager knows only "none".
    if fmt != "none" {
        return Err(format("Invalid credential format."));
    }
    let transports = response
        .get("transports")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    // createCredentialRecord
    let (aaguid, credential_id, public_key) =
        ad.attested.clone().ok_or_else(|| verification("Not attested credential data"))?;

    common_steps(&cd, &ad, "webauthn.create", &options.challenge, &options.rp_id, allowed_origins)?;
    // CheckAlgorithm
    let (alg, _) = cose_key(&public_key)?;
    if alg != ES256 && alg != RS256 {
        return Err(verification(&format!("Invalid algorithm. Expected one of {ES256}, {RS256} but got {alg}")));
    }
    // CheckAttestationFormatIsKnownAndValid: NoneAttestationStatementSupport::isValid
    if !att_stmt.is_empty() {
        return Err(verification("Invalid attestation object"));
    }
    // CheckCredentialId
    if credential_id.len() > 1023 {
        return Err(verification("Credential ID too long."));
    }
    Ok(CredentialRecord {
        public_key_credential_id: credential_id,
        kind: "public-key".into(),
        transports,
        attestation_type: "none".into(),
        trust_path: json!([]),
        aaguid: uuid_string(&aaguid),
        credential_public_key: public_key,
        user_handle: options.user_id.clone(),
        counter: ad.sign_count,
        other_ui: None,
        backup_eligible: Some(ad.flags & FLAG_BE != 0),
        backup_status: Some(ad.flags & FLAG_BS != 0),
        uv_initialized: Some(ad.flags & FLAG_UV != 0),
    })
}

/// `VerifyPasskey::validate`: `AuthenticatorAssertionResponseValidator::check` against the
/// stored record (updated in place: counter, backup flags, UV).
pub fn verify_assertion(
    cred: &Value,
    options: &RequestOptions,
    record: &mut CredentialRecord,
    allowed_origins: &[String],
) -> Result<(), Error> {
    let (_, response) = credential_parts(cred)?;
    let invalid = || format("Invalid credential format.");
    if response.contains_key("attestationObject") || !response.contains_key("signature") {
        return Err(verification("Unable to verify passkey. Please try again."));
    }
    let cd = client_data(response.get("clientDataJSON"))?;
    let ad = auth_data(
        &response.get("authenticatorData").and_then(Value::as_str).and_then(decode_b64).ok_or_else(invalid)?,
    )?;
    let signature = response.get("signature").and_then(Value::as_str).and_then(decode_b64).ok_or_else(invalid)?;
    let response_handle = match response.get("userHandle").and_then(Value::as_str) {
        Some(h) if !h.is_empty() => Some(decode_b64(h).ok_or_else(invalid)?),
        _ => None,
    };

    // CheckAllowedCredentialList
    if !options.allow.is_empty() && !options.allow.iter().any(|id| constant_eq(id, &record.public_key_credential_id)) {
        return Err(verification("The credential ID is not allowed."));
    }
    // CheckUserHandle (the stored handle is passed as the expected one)
    if let Some(h) = &response_handle {
        if !constant_eq(&record.user_handle, h) {
            return Err(verification("Invalid user handle"));
        }
    }
    common_steps(&cd, &ad, "webauthn.get", &options.challenge, &options.rp_id, allowed_origins)?;
    // CheckSignature
    let mut data = ad.raw.clone();
    data.extend_from_slice(&Sha256::digest(&cd.raw));
    verify_signature(&record.credential_public_key, &data, &signature)?;
    // CheckCounter (ThrowExceptionIfInvalid)
    if (ad.sign_count != 0 || record.counter != 0) && ad.sign_count <= record.counter {
        return Err(verification("Invalid counter."));
    }
    record.counter = ad.sign_count;
    record.backup_eligible = Some(ad.flags & FLAG_BE != 0);
    record.backup_status = Some(ad.flags & FLAG_BS != 0);
    if record.uv_initialized == Some(false) {
        record.uv_initialized = Some(ad.flags & FLAG_UV != 0);
    }
    Ok(())
}

#[cfg(test)]
pub mod testing {
    //! A software authenticator producing what browsers send.
    use super::*;
    use p256::ecdsa::{SigningKey, signature::Signer};

    pub struct Authenticator {
        pub key: SigningKey,
        pub credential_id: Vec<u8>,
        pub counter: u32,
        pub flags: u8,
    }

    impl Authenticator {
        pub fn new() -> Authenticator {
            let mut seed = [7u8; 32];
            seed[0] = 1;
            Authenticator {
                key: SigningKey::from_slice(&seed).expect("valid scalar"),
                credential_id: vec![0xAB; 20],
                counter: 0,
                flags: FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS,
            }
        }

        pub fn cose(&self) -> Vec<u8> {
            let point = self.key.verifying_key().to_encoded_point(false);
            let map = ciborium::Value::Map(vec![
                (1.into(), 2.into()),
                (3.into(), (-7).into()),
                ((-1).into(), 1.into()),
                ((-2).into(), ciborium::Value::Bytes(point.x().expect("x").to_vec())),
                ((-3).into(), ciborium::Value::Bytes(point.y().expect("y").to_vec())),
            ]);
            let mut out = Vec::new();
            ciborium::ser::into_writer(&map, &mut out).expect("cbor");
            out
        }

        fn auth_data(&self, rp_id: &str, attested: bool) -> Vec<u8> {
            let mut d = Sha256::digest(rp_id.as_bytes()).to_vec();
            d.push(self.flags | if attested { FLAG_AT } else { 0 });
            d.extend_from_slice(&self.counter.to_be_bytes());
            if attested {
                d.extend_from_slice(&[0u8; 16]);
                d.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
                d.extend_from_slice(&self.credential_id);
                d.extend_from_slice(&self.cose());
            }
            d
        }

        fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
            serde_json::to_vec(
                &json!({ "type": kind, "challenge": b64url(challenge), "origin": origin, "crossOrigin": false }),
            )
            .expect("json")
        }

        pub fn register(&self, rp_id: &str, challenge: &[u8], origin: &str) -> Value {
            let att = ciborium::Value::Map(vec![
                ("fmt".into(), "none".into()),
                ("attStmt".into(), ciborium::Value::Map(vec![])),
                ("authData".into(), ciborium::Value::Bytes(self.auth_data(rp_id, true))),
            ]);
            let mut att_bytes = Vec::new();
            ciborium::ser::into_writer(&att, &mut att_bytes).expect("cbor");
            json!({
                "id": b64url(&self.credential_id), "rawId": b64url(&self.credential_id), "type": "public-key",
                "response": {
                    "clientDataJSON": b64url(&Self::client_data("webauthn.create", challenge, origin)),
                    "attestationObject": b64url(&att_bytes),
                    "transports": ["internal", "hybrid"],
                },
            })
        }

        pub fn assert(&mut self, rp_id: &str, challenge: &[u8], origin: &str, user_handle: &[u8]) -> Value {
            self.counter += 1;
            let ad = self.auth_data(rp_id, false);
            let cd = Self::client_data("webauthn.get", challenge, origin);
            let mut data = ad.clone();
            data.extend_from_slice(&Sha256::digest(&cd));
            let sig: p256::ecdsa::Signature = self.key.sign(&data);
            json!({
                "id": b64url(&self.credential_id), "rawId": b64url(&self.credential_id), "type": "public-key",
                "response": {
                    "clientDataJSON": b64url(&cd),
                    "authenticatorData": b64url(&ad),
                    "signature": b64url(sig.to_der().as_bytes()),
                    "userHandle": b64url(user_handle),
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Authenticator;
    use super::*;

    fn party() -> Party {
        Party {
            rp_id: "panel.example.com".into(),
            allowed_origins: vec!["https://panel.example.com".into()],
            strict_origin: false,
            user_handle_secret: "base64:secret".into(),
            timeout: 60000,
        }
    }

    /// `scripts/cargo test -p pelican-core webauthn::tests::fixture -- --ignored --nocapture`
    /// prints a ceremony for the PHP cross-check (scripts/crosscheck/passkeys.php).
    #[test]
    #[ignore]
    fn fixture() {
        let p = party();
        let origin = "https://panel.example.com";
        let mut a = Authenticator::new();
        let opts = CreationOptions::new(&p, 5, "admin@example.com", "admin", vec![]);
        let cred = a.register(&p.rp_id, &opts.challenge, origin);
        let rec = verify_registration(&cred, &opts, &p.allowed_origins).expect("registered");
        let req = RequestOptions::new(&p, vec![]);
        let login = a.assert(&p.rp_id, &req.challenge, origin, &rec.user_handle.clone());
        let out = json!({
            "rp_id": p.rp_id, "origin": origin,
            "creation_options": opts.to_browser(), "registration": cred, "stored": rec.to_json(),
            "request_options": req.to_browser(), "assertion": login,
        });
        println!("FIXTURE{}", out);
    }

    #[test]
    fn origin_normalization_matches_passkey_origin() {
        assert_eq!(normalize_origin("Panel.Example.com").as_deref(), Some("https://panel.example.com"));
        assert_eq!(normalize_origin("https://x.io:443/path").as_deref(), Some("https://x.io"));
        assert_eq!(normalize_origin("http://localhost:8080").as_deref(), Some("http://localhost:8080"));
        assert_eq!(normalize_origin(" "), None);
        assert_eq!(host_of("http://[::1]:80"), "[::1]");
    }

    #[test]
    fn request_origin_added_only_for_rp_host() {
        let mut p = party();
        p.rp_id = "localhost".into();
        let r = p.for_request(Some("http://localhost:8000"), "http://localhost:8000", "localhost");
        assert!(r.allowed_origins.contains(&"http://localhost:8000".to_string()));
        let r = p.for_request(Some("http://evil.test"), "http://localhost:8000", "localhost");
        assert_eq!(r.allowed_origins, p.allowed_origins);
    }

    #[test]
    fn register_then_assert() {
        let p = party();
        let mut a = Authenticator::new();
        let opts = CreationOptions::new(&p, 5, "admin@example.com", "admin", vec![]);
        let cred = a.register(&p.rp_id, &opts.challenge, "https://panel.example.com");
        let mut rec = verify_registration(&cred, &opts, &p.allowed_origins).expect("registered");
        assert_eq!(rec.public_key_credential_id, a.credential_id);
        assert_eq!(rec.user_handle, user_handle(5, "base64:secret"));
        let stored = rec.to_json();
        assert_eq!(
            stored.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
            Some(
                [
                    "publicKeyCredentialId",
                    "type",
                    "transports",
                    "attestationType",
                    "trustPath",
                    "aaguid",
                    "credentialPublicKey",
                    "userHandle",
                    "counter",
                    "backupEligible",
                    "backupStatus",
                    "uvInitialized"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        assert_eq!(stored["trustPath"], json!([]));
        assert_eq!(stored["aaguid"], json!("00000000-0000-0000-0000-000000000000"));
        assert_eq!(CredentialRecord::from_json(&stored).expect("reload"), rec);

        let req = RequestOptions::new(&p, vec![]);
        let handle = rec.user_handle.clone();
        let login = a.assert(&p.rp_id, &req.challenge, "https://panel.example.com", &handle);
        verify_assertion(&login, &req, &mut rec, &p.allowed_origins).expect("verified");
        assert_eq!(rec.counter, 1);
        // Replaying the same assertion fails on the counter.
        assert_eq!(verify_assertion(&login, &req, &mut rec, &p.allowed_origins), Err(verification("Invalid counter.")));
    }

    #[test]
    fn rejects_wrong_origin_challenge_and_signature() {
        let p = party();
        let mut a = Authenticator::new();
        let opts = CreationOptions::new(&p, 1, "a@b.c", "a", vec![]);
        let bad_origin = a.register(&p.rp_id, &opts.challenge, "https://evil.example.org");
        let err = verify_registration(&bad_origin, &opts, &p.allowed_origins).unwrap_err();
        assert!(err.is_origin_failure());
        let sub = a.register(&p.rp_id, &opts.challenge, "https://x.panel.example.com");
        assert_eq!(
            verify_registration(&sub, &opts, &p.allowed_origins),
            Err(verification("Invalid origin. Subdomains are not allowed."))
        );
        let wrong_challenge = a.register(&p.rp_id, b"other", "https://panel.example.com");
        assert_eq!(
            verify_registration(&wrong_challenge, &opts, &p.allowed_origins),
            Err(verification("Invalid challenge."))
        );
        let wrong_rp = a.register("other.com", &opts.challenge, "https://panel.example.com");
        assert_eq!(verify_registration(&wrong_rp, &opts, &p.allowed_origins), Err(verification("rpId hash mismatch.")));

        let cred = a.register(&p.rp_id, &opts.challenge, "https://panel.example.com");
        let mut rec = verify_registration(&cred, &opts, &p.allowed_origins).expect("registered");
        let req = RequestOptions::new(&p, vec![]);
        let mut login = a.assert(&p.rp_id, &req.challenge, "https://panel.example.com", &rec.user_handle.clone());
        login["response"]["signature"] = json!(b64url(&[0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01]));
        assert_eq!(
            verify_assertion(&login, &req, &mut rec, &p.allowed_origins),
            Err(verification("Invalid signature."))
        );
        let mut no_uv = Authenticator::new();
        no_uv.flags = FLAG_UP;
        let cred = no_uv.register(&p.rp_id, &opts.challenge, "https://panel.example.com");
        assert_eq!(
            verify_registration(&cred, &opts, &p.allowed_origins),
            Err(verification("User authentication required."))
        );
    }
}

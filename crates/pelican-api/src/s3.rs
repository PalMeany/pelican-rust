//! The S3 backup schema (`S3BackupSchema`): AWS Signature V4 for the handful of calls the
//! panel makes — presigned GET / UploadPart URLs for Wings, CreateMultipartUpload,
//! CompleteMultipartUpload, AbortMultipartUpload, ListParts and DeleteObject.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{ApiError, ApiResult};

/// `BackupRemoteUploadController` part sizes.
pub const DEFAULT_MAX_PART_SIZE: i64 = 128 * 1024 * 1024;
pub const MIN_MAX_PART_SIZE: i64 = 5 * 1024 * 1024;
pub const MAX_MAX_PART_SIZE: i64 = 5 * 1024 * 1024 * 1024;
pub const MAX_CONCURRENT_UPLOADS: i64 = 10;

/// A backup host's `configuration` (`region`, `key`, `secret`, `bucket`, `endpoint`,
/// `use_path_style_endpoint`, optional `token` and `storage_class`).
#[derive(Clone, Debug)]
pub struct Bucket {
    pub region: String,
    pub key: String,
    pub secret: String,
    pub token: Option<String>,
    pub bucket: String,
    pub endpoint: String,
    pub path_style: bool,
    pub storage_class: Option<String>,
}

fn s3_error(detail: impl std::fmt::Display) -> ApiError {
    tracing::warn!(error = %detail, "S3 request failed");
    ApiError::internal("S3Exception")
}

impl Bucket {
    pub fn from_host(host: &pelican_db::entities::backup_hosts::Model) -> Bucket {
        Bucket::from_configuration(host.configuration.as_ref().map(|j| &j.0).unwrap_or(&Value::Null))
    }

    pub fn from_configuration(c: &Value) -> Bucket {
        let s = |k: &str| c.get(k).map(pelican_core::php::string).unwrap_or_default();
        let opt = |k: &str| Some(s(k)).filter(|v| !v.is_empty());
        let endpoint = opt("endpoint").unwrap_or_else(|| {
            let region = s("region");
            if region.is_empty() || region == "us-east-1" {
                "https://s3.amazonaws.com".into()
            } else {
                format!("https://s3.{region}.amazonaws.com")
            }
        });
        Bucket {
            region: opt("region").unwrap_or_else(|| "us-east-1".into()),
            key: s("key"),
            secret: s("secret"),
            token: opt("token"),
            bucket: s("bucket"),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            path_style: c.get("use_path_style_endpoint").is_some_and(|v| crate::input::php_boolean(Some(v))),
            storage_class: opt("storage_class"),
        }
    }

    /// `(scheme://host[:port], canonical path)` of an object key.
    fn locate(&self, key: &str) -> (String, String, String) {
        let (scheme, rest) = self.endpoint.split_once("://").unwrap_or(("https", self.endpoint.as_str()));
        let (host, base) = match rest.split_once('/') {
            Some((h, p)) => (h.to_string(), format!("/{}", p.trim_matches('/'))),
            None => (rest.to_string(), String::new()),
        };
        let base = if base == "/" { String::new() } else { base };
        let key_path = key.split('/').map(uri_encode).collect::<Vec<_>>().join("/");
        if self.path_style {
            (scheme.to_string(), host, format!("{base}/{}/{key_path}", uri_encode(&self.bucket)))
        } else {
            (scheme.to_string(), format!("{}.{host}", self.bucket), format!("{base}/{key_path}"))
        }
    }

    fn signing_key(&self, date: &str) -> Vec<u8> {
        let k = hmac(format!("AWS4{}", self.secret).as_bytes(), date.as_bytes());
        let k = hmac(&k, self.region.as_bytes());
        let k = hmac(&k, b"s3");
        hmac(&k, b"aws4_request")
    }

    /// A presigned URL (`createPresignedRequest`): only `host` is signed.
    pub fn presign(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, String)],
        expires: i64,
        now: DateTime<Utc>,
    ) -> String {
        let (scheme, host, path) = self.locate(key);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let mut params: Vec<(String, String)> = query.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        params.push(("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()));
        params.push(("X-Amz-Credential".into(), format!("{}/{scope}", self.key)));
        params.push(("X-Amz-Date".into(), amz_date.clone()));
        params.push(("X-Amz-Expires".into(), expires.to_string()));
        params.push(("X-Amz-SignedHeaders".into(), "host".into()));
        if let Some(t) = &self.token {
            params.push(("X-Amz-Security-Token".into(), t.clone()));
        }
        let qs = canonical_query(&params);
        let canonical = format!("{method}\n{path}\n{qs}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD");
        let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex::encode(Sha256::digest(canonical)));
        let sig = hex::encode(hmac(&self.signing_key(&date), to_sign.as_bytes()));
        format!("{scheme}://{host}{path}?{qs}&X-Amz-Signature={sig}")
    }

    /// A request signed in the `Authorization` header.
    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        query: &[(&str, String)],
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) -> ApiResult<String> {
        let now = Utc::now();
        let (scheme, host, path) = self.locate(key);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let payload = hex::encode(Sha256::digest(&body));
        let mut hs: Vec<(String, String)> = vec![
            ("host".into(), host.clone()),
            ("x-amz-content-sha256".into(), payload.clone()),
            ("x-amz-date".into(), amz_date.clone()),
        ];
        if let Some(t) = &self.token {
            hs.push(("x-amz-security-token".into(), t.clone()));
        }
        hs.extend(headers.iter().map(|(k, v)| (k.to_lowercase(), v.trim().to_string())));
        hs.sort();
        let signed = hs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let canonical_headers: String = hs.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let params: Vec<(String, String)> = query.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let qs = canonical_query(&params);
        let canonical = format!("{method}\n{path}\n{qs}\n{canonical_headers}\n{signed}\n{payload}");
        let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex::encode(Sha256::digest(canonical)));
        let sig = hex::encode(hmac(&self.signing_key(&date), to_sign.as_bytes()));
        let auth = format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={sig}", self.key);
        let url =
            if qs.is_empty() { format!("{scheme}://{host}{path}") } else { format!("{scheme}://{host}{path}?{qs}") };
        let client =
            reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(s3_error)?;
        let mut req = client.request(method, &url).header("authorization", auth);
        for (k, v) in &hs {
            if k != "host" {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        let resp = req.body(body).send().await.map_err(s3_error)?;
        let status = resp.status();
        let text = resp.text().await.map_err(s3_error)?;
        if !status.is_success() {
            return Err(s3_error(format!("{status}: {text}")));
        }
        Ok(text)
    }

    /// `CreateMultipartUpload` → the upload id.
    pub async fn create_multipart_upload(&self, key: &str) -> ApiResult<String> {
        let mut headers = vec![("content-type", "application/x-gzip".to_string())];
        if let Some(c) = &self.storage_class {
            headers.push(("x-amz-storage-class", c.clone()));
        }
        let xml = self.send(reqwest::Method::POST, key, &[("uploads", String::new())], &headers, Vec::new()).await?;
        xml_values(&xml, "UploadId").into_iter().next().ok_or_else(|| s3_error("no UploadId in response"))
    }

    /// `ListParts` → `(part number, etag)`.
    pub async fn list_parts(&self, key: &str, upload_id: &str) -> ApiResult<Vec<(i64, String)>> {
        let xml = self.send(reqwest::Method::GET, key, &[("uploadId", upload_id.to_string())], &[], Vec::new()).await?;
        let numbers = xml_values(&xml, "PartNumber");
        let etags = xml_values(&xml, "ETag");
        Ok(numbers.iter().zip(etags).filter_map(|(n, e)| n.parse().ok().map(|n| (n, e))).collect())
    }

    pub async fn complete_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[(i64, String)],
    ) -> ApiResult<()> {
        let mut body = String::from("<CompleteMultipartUpload>");
        for (n, etag) in parts {
            body.push_str(&format!("<Part><ETag>{}</ETag><PartNumber>{n}</PartNumber></Part>", xml_escape(etag)));
        }
        body.push_str("</CompleteMultipartUpload>");
        self.send(
            reqwest::Method::POST,
            key,
            &[("uploadId", upload_id.to_string())],
            &[("content-type", "application/xml".into())],
            body.into_bytes(),
        )
        .await
        .map(|_| ())
    }

    pub async fn abort_multipart_upload(&self, key: &str, upload_id: &str) -> ApiResult<()> {
        self.send(reqwest::Method::DELETE, key, &[("uploadId", upload_id.to_string())], &[], Vec::new())
            .await
            .map(|_| ())
    }

    pub async fn delete_object(&self, key: &str) -> ApiResult<()> {
        self.send(reqwest::Method::DELETE, key, &[], &[], Vec::new()).await.map(|_| ())
    }
}

/// `"{server uuid}/{backup uuid}.tar.gz"`.
pub fn backup_key(server_uuid: &str, backup_uuid: &str) -> String {
    format!("{server_uuid}/{backup_uuid}.tar.gz")
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let Ok(mut m) = Hmac::<Sha256>::new_from_slice(key) else { return Vec::new() };
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// RFC 3986 encoding with SigV4's unreserved set.
fn uri_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn canonical_query(params: &[(String, String)]) -> String {
    let mut enc: Vec<(String, String)> = params.iter().map(|(k, v)| (uri_encode(k), uri_encode(v))).collect();
    enc.sort();
    enc.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

fn xml_values(xml: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(xml_unescape(&after[..j]));
        rest = &after[j + close.len()..];
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The presigned GET example of the AWS SigV4 documentation
    /// ("Authenticating Requests: Using Query Parameters").
    #[test]
    fn presign_matches_aws_example() {
        let b = Bucket {
            region: "us-east-1".into(),
            key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            token: None,
            bucket: "examplebucket".into(),
            endpoint: "https://s3.amazonaws.com".into(),
            path_style: false,
            storage_class: None,
        };
        let now = DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z").map(|d| d.with_timezone(&Utc)).expect("date");
        let url = b.presign("GET", "test.txt", &[], 86400, now);
        assert!(url.starts_with("https://examplebucket.s3.amazonaws.com/test.txt?"));
        assert!(
            url.ends_with("X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"),
            "{url}"
        );
    }

    #[test]
    fn path_style_and_xml() {
        let b = Bucket::from_configuration(&serde_json::json!({
            "region": "us-east-1", "key": "k", "secret": "s", "bucket": "backups",
            "endpoint": "http://minio:9000", "use_path_style_endpoint": true,
        }));
        let (scheme, host, path) = b.locate("abc/def.tar.gz");
        assert_eq!((scheme.as_str(), host.as_str(), path.as_str()), ("http", "minio:9000", "/backups/abc/def.tar.gz"));
        let xml = "<InitiateMultipartUploadResult><UploadId>a&amp;b</UploadId></InitiateMultipartUploadResult>";
        assert_eq!(xml_values(xml, "UploadId"), vec!["a&b".to_string()]);
    }
}

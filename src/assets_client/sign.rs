//! Request signatures as the Frequenz API services check them: every call
//! carries `key`, `ts`, `nonce` and `sig` metadata, where `sig` is an
//! HMAC-SHA256 over the other three and the RPC name.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use tonic::GrpcMethod;
use tonic::service::Interceptor;

/// An API key and its signing secret. `Debug` shows neither.
#[derive(Clone)]
pub struct Credentials {
    key: String,
    secret: String,
}

impl Credentials {
    /// The key must be non-empty visible ASCII with no spaces, because it
    /// travels as a gRPC header value. The secret must be non-empty.
    pub fn new(key: impl Into<String>, secret: impl Into<String>) -> Result<Self, String> {
        let (key, secret) = (key.into(), secret.into());
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("the API key is empty or holds characters other than visible ASCII".into());
        }
        if secret.is_empty() {
            return Err("the signing secret is empty".into());
        }
        Ok(Self { key, secret })
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { <redacted> }")
    }
}

/// HMAC-SHA256 over `key ‖ ts ‖ nonce ‖ method`, URL-safe base64 without
/// padding. `method` is the RPC name alone, not the full gRPC path.
fn signature(secret: &str, key: &str, ts: &str, nonce: &str, method: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    for part in [key, ts, nonce, method] {
        mac.update(part.as_bytes());
    }
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// A nonce as it is both signed and sent: URL-safe base64, padding kept.
fn encode_nonce(nonce: &[u8; 16]) -> String {
    URL_SAFE.encode(nonce)
}

/// Signs every call of the client it intercepts.
#[derive(Clone)]
pub struct Signer {
    creds: Credentials,
}

impl Signer {
    pub fn new(creds: Credentials) -> Self {
        Self { creds }
    }
}

impl Interceptor for Signer {
    /// Adds the `key`, `ts`, `nonce` and `sig` metadata, signing the RPC name
    /// (such as `GetMicrogrid`) the generated client puts on the request.
    fn call(&mut self, mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        let method = req
            .extensions()
            .get::<GrpcMethod>()
            .map(|m| m.method().to_owned())
            .ok_or_else(|| tonic::Status::internal("the request names no RPC to sign"))?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            .to_string();
        let nonce = encode_nonce(&rand::random());
        let sig = signature(&self.creds.secret, &self.creds.key, &ts, &nonce, &method);
        let metadata = req.metadata_mut();
        for (name, value) in [
            ("key", self.creds.key.as_str()),
            ("ts", &ts),
            ("nonce", &nonce),
            ("sig", &sig),
        ] {
            // Credentials::new only admits visible ASCII keys; the rest is
            // digits and base64.
            metadata.insert(name, value.parse().expect("visible ASCII"));
        }
        Ok(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vector from the services' own server-side verifier.
    #[test]
    fn matches_server_vector() {
        assert_eq!(
            signature("my_secret", "my_key", "1634567890", "123456789", "my_rpc"),
            "NJDvrkRZhOPekn5AvPiaJsYTJYCgnLzA-LQFC2D7GNE"
        );
    }

    // A second vector, with a padded base64 nonce.
    #[test]
    fn matches_a_padded_nonce_vector() {
        assert_eq!(
            signature(
                "test-secret",
                "test-key",
                "1700000000",
                "AAECAwQFBgcICQoLDA0ODw==",
                "GetMicrogrid"
            ),
            "J9BmvyTi_lSBZFfgHY8OG6q-njqIPz_i3X9uv-MMHXs"
        );
    }

    #[test]
    fn nonce_keeps_padding() {
        let nonce: [u8; 16] = std::array::from_fn(|i| i as u8);
        assert_eq!(encode_nonce(&nonce), "AAECAwQFBgcICQoLDA0ODw==");
    }

    #[test]
    fn credentials_refuse_empty_or_spaced_parts_and_hide_in_debug() {
        assert!(Credentials::new("", "s").is_err());
        assert!(Credentials::new("a b", "s").is_err());
        assert!(Credentials::new("k", "").is_err());
        let shown = format!("{:?}", Credentials::new("k3y", "s3cret").unwrap());
        assert!(
            !shown.contains("k3y") && !shown.contains("s3cret"),
            "{shown}"
        );
    }

    fn get_microgrid() -> tonic::Request<()> {
        let mut req = tonic::Request::new(());
        req.extensions_mut().insert(GrpcMethod::new(
            "frequenz.api.platformassets.v1alpha1.PlatformAssetsService",
            "GetMicrogrid",
        ));
        req
    }

    #[test]
    fn adds_a_verifiable_signature() {
        let mut signer = Signer::new(Credentials::new("test-key", "test-secret").unwrap());
        let req = signer.call(get_microgrid()).unwrap();
        let get = |name: &str| {
            req.metadata()
                .get(name)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(get("key"), "test-key");
        assert_eq!(get("nonce").len(), 24);
        // In seconds since the epoch.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            now.abs_diff(get("ts").parse().unwrap()) <= 5,
            "{}",
            get("ts")
        );
        let expected = signature(
            "test-secret",
            "test-key",
            &get("ts"),
            &get("nonce"),
            "GetMicrogrid",
        );
        assert_eq!(get("sig"), expected);
    }

    #[test]
    fn refuses_a_request_without_an_rpc_name() {
        let mut signer = Signer::new(Credentials::new("k", "s").unwrap());
        let err = signer.call(tonic::Request::new(())).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
    }
}

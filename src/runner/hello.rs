//! The hello a script child sends once its script has loaded, and the
//! engine's answer: `POST /api/runner/hello`.

use serde::{Deserialize, Serialize};

use crate::engine::topology::TopologyError;

/// `{"token": ..., "client_version": ..., "topology": {...}}`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloRequest {
    /// The token the engine gave this child.
    pub token: String,
    /// The child's package version, in PEP 440 form.
    pub client_version: String,
    /// A topology document of the script's microgrids. Kept as JSON
    /// here, so a bad one is refused as an invalid spec (422).
    pub topology: serde_json::Value,
}

/// The answer once the microgrids are in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HelloAnswer {
    pub microgrids: Vec<HelloMicrogrid>,
}

/// One microgrid of the answer: its id and where its gRPC server
/// listens (`null` when its bind failed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HelloMicrogrid {
    pub id: u64,
    pub grpc_addr: Option<String>,
}

/// Why a hello was refused. The world and the old child are as they
/// were.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelloError {
    /// The token is not the current child's (403).
    Token,
    /// `client_version` is not the binary's (400).
    Version { client: String },
    /// The microgrids are refused as a structural change is: an
    /// invalid spec or a microgrid without an id (422), an id or a
    /// port that is not the script's to take (409); also a child that
    /// exited before its microgrids were applied (409).
    Topology(TopologyError),
}

impl HelloRequest {
    /// Read a hello. After the token, which the supervisor checks,
    /// the version is compared before anything else is read, so a
    /// client of another version is told so, even when it sends
    /// fields this engine does not know.
    pub fn parse(body: serde_json::Value) -> Result<Self, HelloError> {
        if let Some(client) = body.get("client_version").and_then(|v| v.as_str())
            && client != crate::version::VERSION.as_str()
        {
            return Err(HelloError::Version {
                client: client.to_string(),
            });
        }
        serde_json::from_value(body)
            .map_err(|e| TopologyError::Invalid(format!("the hello does not parse: {e}")).into())
    }
}

impl std::fmt::Display for HelloError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token => f.write_str("the token is not the current script child's"),
            Self::Version { client } => f.write_str(&crate::version::mismatch_message(client)),
            Self::Topology(e) => e.fmt(f),
        }
    }
}

impl From<TopologyError> for HelloError {
    fn from(e: TopologyError) -> Self {
        Self::Topology(e)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The version is read first, so a newer client with a field this
    /// engine does not know is told about the version.
    #[test]
    fn a_hello_of_another_version_is_refused_for_its_version() {
        let err = HelloRequest::parse(json!({
            "token": "t", "client_version": "9.9.9", "topology": {}, "newer": 1,
        }))
        .unwrap_err();
        assert_eq!(
            err,
            HelloError::Version {
                client: "9.9.9".into()
            }
        );
        let v = crate::version::VERSION.as_str();
        let err = HelloRequest::parse(json!({
            "token": "t", "client_version": v, "topology": {}, "newer": 1,
        }))
        .unwrap_err();
        assert!(
            matches!(&err, HelloError::Topology(TopologyError::Invalid(m)) if m.contains("newer")),
            "{err}"
        );
        let ok = HelloRequest::parse(json!({"token": "t", "client_version": v, "topology": {}}));
        assert_eq!(ok.unwrap().token, "t");
    }
}

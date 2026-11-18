//! `grpc://host[:port][?ssl=BOOL]` URLs. The port defaults to 443 and ssl to
//! true; BOOL is `true`/`on`/`1` or `false`/`off`/`0` in any case, and no other
//! query parameter is taken. An IPv6 host goes in brackets:
//! `grpc://[::1]:9900?ssl=false`.

use std::net::Ipv6Addr;
use std::time::Duration;

use tonic::codegen::http::Uri;
use tonic::transport::{ClientTlsConfig, Endpoint};

/// How long the TCP connect, and then the TLS handshake, may each take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The endpoint `url` names: TLS unless the URL turns ssl off.
pub fn endpoint(url: &str) -> Result<Endpoint, String> {
    let bad = || format!("{url}: expected grpc://host[:port][?ssl=true|false]");
    let uri: Uri = url.parse().map_err(|_| bad())?;
    let authority = uri.authority().ok_or_else(bad)?;
    if uri.scheme_str() != Some("grpc")
        || authority.as_str().contains('@')
        || !matches!(uri.path(), "" | "/")
    {
        return Err(bad());
    }
    // With brackets for an IPv6 address, as the URI needs it.
    let host = authority.host();
    let name = match host.strip_prefix('[') {
        Some(bracketed) => {
            let name = bracketed.strip_suffix(']').ok_or_else(bad)?;
            name.parse::<Ipv6Addr>().map_err(|_| bad())?;
            name
        }
        None => host,
    };
    // The text after the host: empty, or a colon and the port.
    let port = match &authority.as_str()[host.len()..] {
        "" => 443,
        _ => authority.port_u16().filter(|&p| p > 0).ok_or_else(bad)?,
    };
    let ssl = match uri.query().map(|q| q.split_once('=')) {
        None => true,
        Some(Some(("ssl", value))) => match value.to_ascii_lowercase().as_str() {
            "true" | "on" | "1" => true,
            "false" | "off" | "0" => false,
            _ => return Err(bad()),
        },
        Some(_) => return Err(bad()),
    };
    let scheme = if ssl { "https" } else { "http" };
    let failed = |e: tonic::transport::Error| format!("{url}: {e}");
    let endpoint = Endpoint::from_shared(format!("{scheme}://{host}:{port}"))
        .map_err(failed)?
        .connect_timeout(CONNECT_TIMEOUT);
    if !ssl {
        return Ok(endpoint);
    }
    endpoint
        .tls_config(
            ClientTlsConfig::new()
                .with_native_roots()
                .domain_name(name)
                .timeout(CONNECT_TIMEOUT),
        )
        .map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(url: &str) -> String {
        endpoint(url).unwrap().uri().to_string()
    }

    #[test]
    fn port_defaults_to_443_and_ssl_to_true() {
        assert_eq!(
            uri("grpc://assets.example.com"),
            "https://assets.example.com:443/"
        );
    }

    #[test]
    fn ssl_takes_on_off_and_digits() {
        for (q, scheme) in [
            ("ssl=true", "https"),
            ("ssl=On", "https"),
            ("ssl=1", "https"),
            ("ssl=FALSE", "http"),
            ("ssl=off", "http"),
            ("ssl=0", "http"),
        ] {
            assert_eq!(
                uri(&format!("grpc://h:1?{q}")),
                format!("{scheme}://h:1/"),
                "{q}"
            );
        }
    }

    #[test]
    fn ipv6_literals() {
        assert_eq!(uri("grpc://[::1]:9900?ssl=false"), "http://[::1]:9900/");
    }

    #[test]
    fn refuses_malformed_urls() {
        for bad in [
            "http://h:1",
            "grpc://",
            "grpc://h:port",
            "grpc://h:0",
            "grpc://h/x",
            "grpc://u@h:1",
            "grpc://a:b:1",
            "grpc://[::1",
            "grpc://[example.com]:443",
            "grpc://h?ssl=maybe",
            "grpc://h?tls=1",
            "grpc://h?ssl=1&x=2",
            "grpc://h:",
            "grpc://h%",
            "grpc://h :1",
            "grpc://[::1]x",
        ] {
            assert!(endpoint(bad).is_err(), "{bad} parsed");
        }
    }
}

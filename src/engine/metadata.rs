//! Enterprise-wide settings: the enterprise id, the assets and
//! dispatch socket addresses, and the request and augment lifetime
//! fallbacks.

use std::time::Duration;

/// The ports of the enterprise's assets and dispatch sockets. An
/// address that does not parse contributes nothing; the binary
/// validates it when it binds.
pub(crate) fn reserved_ports_of(m: &Metadata) -> Vec<u16> {
    [&m.assets_socket_addr, &m.dispatch_socket_addr]
        .into_iter()
        .filter_map(|a| a.parse::<std::net::SocketAddr>().ok())
        .map(|a| a.port())
        .collect()
}

/// Enterprise-level gateway settings the binary's enterprise flags
/// set. Per-microgrid identity (id, name, grpc_port, TSO) lives in
/// the `sim::microgrids` registry, one entry per microgrid. Metadata
/// here only carries enterprise-wide knobs: the enterprise id
/// surfaced on every gRPC `MicrogridInfo`, the assets server's bind
/// address, and the default request-lifetime fallback.
#[derive(Debug, Clone)]
pub struct Metadata {
    pub enterprise_id: u64,
    /// Address the PlatformAssets gRPC service binds to. Independent
    /// of any microgrid's `grpc_port` so a sibling service (assets /
    /// reporting / future API surfaces) doesn't fight a microgrid for
    /// its socket. Set by `--assets-addr`.
    pub assets_socket_addr: String,
    /// Address the single (enterprise-wide)
    /// `MicrogridDispatchService` gRPC service binds to. One service
    /// fronts every microgrid, keyed by `microgrid_id` in each
    /// request, so it gets its own socket — distinct from any
    /// microgrid's `grpc_port` and from the assets server. Set by
    /// `--dispatch-addr`.
    pub dispatch_socket_addr: String,
    /// Fallback request lifetime when a `SetElectricalComponentPower`
    /// caller doesn't supply `request_lifetime`. Mirrors microsim's
    /// `retain-requests-duration-ms`. Set by `--request-lifetime`.
    /// The gRPC handler's per-request validation in
    /// `server::resolve_lifetime` clamps to
    /// `[REQUEST_LIFETIME_MIN_S, REQUEST_LIFETIME_MAX_S]`; this
    /// default isn't clamped (a config that wants short / long
    /// fallbacks is responsible for picking values that align with
    /// its operational expectations).
    pub default_request_lifetime: Duration,
    /// Fallback lifetime for an Augment request that carries none.
    /// The proto documents 5 s for Augment where SetPower gets 60 s,
    /// so the two fallbacks are separate knobs. Set by
    /// `--augment-lifetime`. Like `default_request_lifetime`, this
    /// default isn't clamped to the RPC's
    /// `[AUGMENT_LIFETIME_MIN_S, REQUEST_LIFETIME_MAX_S]` (5 s,
    /// 15 min) range; a config that picks a value outside it is
    /// responsible for the consequences.
    pub default_augment_lifetime: Duration,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            enterprise_id: 0,
            assets_socket_addr: "[::1]:9900".to_string(),
            dispatch_socket_addr: "[::1]:8900".to_string(),
            default_request_lifetime: Duration::from_secs(60),
            default_augment_lifetime: Duration::from_secs(5),
        }
    }
}

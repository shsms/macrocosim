//! A client of a Frequenz Assets API (`frequenz.api.platformassets.v1alpha1`):
//! it reads one microgrid's components and connections and makes a
//! [`MicrogridSpec`] of them with the site import
//! ([`crate::sim::site_import`]).
//!
//! The spec keeps the API's microgrid and component ids. Components outside
//! their operational lifetime are left out with their connections, and so are
//! connections outside their own. A module has exactly one root, so a microgrid
//! with more than one grid connection point is refused, and a component not
//! under the grid connection point is left out.
//!
//! Components the simulator has no model for are left out too, each parent of
//! one connected to each of its children ([`Unsupported::Skip`]). A hybrid
//! inverter, and an inverter of no type over a battery, go with the
//! components only such inverters lead to, into [`Fetched::inverter_trees`].
//! Every component left out is named in [`Fetched::left_out`].

pub mod endpoint;
pub mod sign;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use crate::proto::assets::platform_assets_service_client::PlatformAssetsServiceClient;
use crate::proto::assets::{
    GetMicrogridRequest, ListMicrogridElectricalComponentConnectionsRequest,
    ListMicrogridElectricalComponentsRequest,
};
use crate::proto::common::microgrid::Lifetime;
use crate::proto::common::microgrid::Microgrid;
use crate::proto::common::microgrid::electrical_components::{
    ElectricalComponent, ElectricalComponentCategory, ElectricalComponentConnection,
};
use crate::sim::site_import::{
    self, ComponentsFile, ConnectionsFile, InverterKind, InverterTree, Unsupported,
};
use crate::spec::{ComponentSpec, MicrogridSpec, SCHEMA_VERSION, TopologyDoc};
use sign::{Credentials, Signer};

/// How long a whole fetch may take, connecting included.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// A microgrid's spec, and what was left out of it.
#[derive(Debug)]
pub struct Fetched {
    pub spec: MicrogridSpec,
    /// One line per component left out, with why, including one not under
    /// the grid connection point. One line per connection outside its own
    /// operational lifetime, or naming a component the microgrid does not list.
    pub left_out: Vec<String>,
    /// The inverters left out with the components under them: hybrid
    /// inverters, and inverters of no type with a battery under them.
    pub inverter_trees: Vec<InverterTree>,
}

impl Fetched {
    /// What `python -m macrocosim.export` ([`crate::python::write_module`])
    /// reads for this microgrid, and one line per inverter tree saying where
    /// the module has it, or that it does not. The input is the topology
    /// document, with each tree commented out under the first of its inverter's
    /// parents, in connection order, that is in the spec and takes successors
    /// ([`ComponentSpec::takes_successors`]). One with no such parent is not
    /// written.
    pub fn module_input(&self) -> (serde_json::Value, Vec<String>) {
        let doc = TopologyDoc {
            schema_version: SCHEMA_VERSION,
            microgrids: vec![self.spec.clone()],
        };
        let mut input = serde_json::to_value(doc).expect("a topology document serializes");
        let with_successors: HashSet<u64> = self
            .spec
            .components
            .iter()
            .filter(|c| c.takes_successors())
            .filter_map(ComponentSpec::id)
            .collect();
        let mut commented = Vec::new();
        let mut notes = Vec::new();
        for tree in &self.inverter_trees {
            let label = tree.kind.label();
            let Some(parent) = tree.parents.iter().find(|p| with_successors.contains(p)) else {
                notes.push(format!(
                    "{label} {} is not written: none of its parents that are left can \
                     have components under it",
                    tree.inverter
                ));
                continue;
            };
            commented.push(serde_json::json!({
                "parent": parent,
                "kind": match tree.kind {
                    InverterKind::Hybrid => "hybrid_inverter",
                    InverterKind::Unspecified => "inverter_of_no_type",
                },
                "components": tree.spec.components,
                "connections": tree.spec.connections,
            }));
            notes.push(format!(
                "{label} {} and the components that go with it are written commented out \
                 under component {parent}",
                tree.inverter
            ));
        }
        if !commented.is_empty() {
            input["commented"] = commented.into();
        }
        (input, notes)
    }
}

/// Fetches microgrid `microgrid_id` from the Assets API at `url`, every call
/// signed with `creds`, and makes its spec with [`spec_of`]. Fails when the
/// answers take longer than a minute, connecting included.
pub async fn fetch(url: &str, creds: Credentials, microgrid_id: u64) -> Result<Fetched, String> {
    fetch_within(url, creds, microgrid_id, FETCH_TIMEOUT).await
}

/// [`fetch`], failing when the whole fetch, connecting included, takes longer
/// than `timeout`.
pub async fn fetch_within(
    url: &str,
    creds: Credentials,
    microgrid_id: u64,
    timeout: Duration,
) -> Result<Fetched, String> {
    let endpoint = endpoint::endpoint(url)?;
    let client =
        PlatformAssetsServiceClient::with_interceptor(endpoint.connect_lazy(), Signer::new(creds));
    let (mut a, mut b, mut c) = (client.clone(), client.clone(), client);
    let fail =
        |rpc: &'static str| move |s: tonic::Status| format!("{url}: {rpc}: {}", status_text(&s));
    let answers = async {
        tokio::try_join!(
            async {
                a.get_microgrid(GetMicrogridRequest { microgrid_id })
                    .await
                    .map_err(fail("GetMicrogrid"))
            },
            async {
                b.list_microgrid_electrical_components(ListMicrogridElectricalComponentsRequest {
                    microgrid_id,
                    filter: None,
                })
                .await
                .map_err(fail("ListMicrogridElectricalComponents"))
            },
            async {
                c.list_microgrid_electrical_component_connections(
                    ListMicrogridElectricalComponentConnectionsRequest {
                        microgrid_id,
                        filter: None,
                    },
                )
                .await
                .map_err(fail("ListMicrogridElectricalComponentConnections"))
            },
        )
    };
    let (microgrid, components, connections) = tokio::time::timeout(timeout, answers)
        .await
        .map_err(|_| format!("{url}: no complete answer within {timeout:?}"))??;
    let microgrid = microgrid
        .into_inner()
        .microgrid
        .ok_or_else(|| format!("{url}: the answer holds no microgrid {microgrid_id}"))?;
    spec_of(
        &microgrid,
        &components.into_inner().components,
        &connections.into_inner().connections,
        SystemTime::now(),
    )
}

/// `status`'s code and message, and the causes under it that the message does
/// not already say: a failure to connect keeps the operating system's reason
/// several sources down.
fn status_text(status: &tonic::Status) -> String {
    let mut text = format!("{:?}: {}", status.code(), escape_controls(status.message()));
    for cause in std::iter::successors(std::error::Error::source(status), |e| e.source()) {
        let cause = escape_controls(&cause.to_string());
        // tonic's own wrapper says no more than "transport error".
        if cause != "transport error" && !text.contains(&cause) {
            text = format!("{text} ({cause})");
        }
    }
    text
}

/// The spec of `microgrid` from the Assets API's answers, and what was left out
/// of it, with operational lifetimes judged at `now`.
pub fn spec_of(
    microgrid: &Microgrid,
    components: &[ElectricalComponent],
    connections: &[ElectricalComponentConnection],
    now: SystemTime,
) -> Result<Fetched, String> {
    let fail = |e: String, left_out: &[String]| {
        let why = if left_out.is_empty() {
            String::new()
        } else {
            format!(" ({})", left_out.join("; "))
        };
        format!("microgrid {}: {e}{why}", microgrid.id)
    };
    if components.is_empty() {
        return Err(fail("the Assets API lists no components".into(), &[]));
    }
    let mut left_out = Vec::new();
    let mut active = Vec::new();
    let mut names = HashMap::new();
    for c in components {
        names.insert(c.id, c.name.as_str());
        match outside_lifetime(c.operational_lifetime.as_ref(), now) {
            None => active.push(c),
            Some(why) => {
                left_out.push(format!("{} {why}; left out", component_label(&names, c.id)))
            }
        }
    }
    let active_ids: HashSet<u64> = active.iter().map(|c| c.id).collect();
    let mut edges = Vec::new();
    for c in connections {
        let (src, dst) = (
            c.source_electrical_component_id,
            c.destination_electrical_component_id,
        );
        if !(names.contains_key(&src) && names.contains_key(&dst)) {
            left_out.push(format!(
                "connection {src} → {dst} names a component the microgrid does not list; \
                 left out"
            ));
        } else if let Some(why) = outside_lifetime(c.operational_lifetime.as_ref(), now) {
            left_out.push(format!("connection {src} → {dst} {why}; left out"));
        } else if active_ids.contains(&src) && active_ids.contains(&dst) {
            edges.push(*c);
        }
    }
    if active.is_empty() {
        return Err(fail(
            "no component is in its operational lifetime".into(),
            &left_out,
        ));
    }
    // A module has one root: keep what hangs from the one grid connection
    // point, over the connections that are left.
    let roots: Vec<u64> = active
        .iter()
        .filter(|c| c.category() == ElectricalComponentCategory::GridConnectionPoint)
        .map(|c| c.id)
        .collect();
    let root = match roots[..] {
        [] => return Err(fail("no grid connection point is left".into(), &left_out)),
        [root] => root,
        _ => {
            let labels: Vec<String> = roots
                .iter()
                .map(|&id| component_label(&names, id))
                .collect();
            return Err(fail(
                format!(
                    "it has {} grid connection points, {}; a module has one",
                    roots.len(),
                    labels.join(" and ")
                ),
                &[],
            ));
        }
    };
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for c in &edges {
        children
            .entry(c.source_electrical_component_id)
            .or_default()
            .push(c.destination_electrical_component_id);
    }
    let mut reached = HashSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if reached.insert(id) {
            stack.extend(children.get(&id).into_iter().flatten());
        }
    }
    for c in &active {
        if reached.contains(&c.id) {
            continue;
        }
        // A listed parent the grid does not reach either; none when only
        // a connection was left out.
        let by = connections
            .iter()
            .filter(|e| e.destination_electrical_component_id == c.id)
            .map(|e| e.source_electrical_component_id)
            .find(|p| names.contains_key(p) && !reached.contains(p));
        left_out.push(match by {
            Some(p) => format!(
                "{} is cut off from the grid by {}; left out",
                component_label(&names, c.id),
                component_label(&names, p)
            ),
            None => format!(
                "{} is cut off from the grid; left out",
                component_label(&names, c.id)
            ),
        });
    }
    active.retain(|c| reached.contains(&c.id));
    edges.retain(|e| {
        reached.contains(&e.source_electrical_component_id)
            && reached.contains(&e.destination_electrical_component_id)
    });
    let active: Vec<ElectricalComponent> = active.into_iter().cloned().collect();
    let import = site_import::parse_with(
        ComponentsFile::from_protos(&active),
        Some(ConnectionsFile::from_protos(&edges)),
        Unsupported::Skip,
    )
    .map_err(|e| fail(e, &left_out))?;
    for (id, why) in &import.skipped {
        left_out.push(format!("{}: {why}; left out", component_label(&names, *id)));
    }
    let mut spec = import.spec()?;
    spec.id = Some(microgrid.id);
    spec.name = Some(microgrid.name.clone()).filter(|n| !n.is_empty());
    Ok(Fetched {
        spec,
        left_out,
        inverter_trees: import.inverter_trees,
    })
}

/// `component 3`, or `component 3 (main meter)` when it has a name.
fn component_label(names: &HashMap<u64, &str>, id: u64) -> String {
    match names.get(&id).filter(|n| !n.is_empty()) {
        Some(n) => format!("component {id} ({})", escape_controls(n)),
        None => format!("component {id}"),
    }
}

/// `text` from the API with its control characters escaped, so a name or a
/// message cannot drive the terminal it is printed to.
fn escape_controls(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Why `now` is outside an operational lifetime, or `None` when it is inside or
/// there is none. Both ends are inclusive, and an unset end means open.
fn outside_lifetime(lifetime: Option<&Lifetime>, now: SystemTime) -> Option<&'static str> {
    // A time SystemTime cannot hold counts as unset.
    let at = |t: Option<prost_types::Timestamp>| t.and_then(|t| SystemTime::try_from(t).ok());
    let start = at(lifetime.and_then(|l| l.start_timestamp));
    let end = at(lifetime.and_then(|l| l.end_timestamp));
    if start.is_some_and(|s| s > now) {
        Some("is not yet active")
    } else if end.is_some_and(|e| e < now) {
        Some("is retired")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use prost_types::Timestamp;

    use super::*;
    use crate::proto::common::microgrid::electrical_components::ElectricalComponentCategory as C;

    fn component(id: u64, category: C, name: &str) -> ElectricalComponent {
        ElectricalComponent {
            id,
            name: name.into(),
            category: category as i32,
            ..Default::default()
        }
    }

    fn hybrid(id: u64) -> ElectricalComponent {
        use crate::proto::common::microgrid::electrical_components::{
            ElectricalComponentCategorySpecificInfo, Inverter, InverterType,
            electrical_component_category_specific_info::Kind,
        };
        ElectricalComponent {
            category_specific_info: Some(ElectricalComponentCategorySpecificInfo {
                kind: Some(Kind::Inverter(Inverter {
                    r#type: InverterType::Hybrid as i32,
                })),
            }),
            ..component(id, C::Inverter, "")
        }
    }

    fn edge(src: u64, dst: u64) -> ElectricalComponentConnection {
        ElectricalComponentConnection {
            source_electrical_component_id: src,
            destination_electrical_component_id: dst,
            ..Default::default()
        }
    }

    fn life(start: Option<i64>, end: Option<i64>) -> Option<Lifetime> {
        let at = |seconds| Timestamp { seconds, nanos: 0 };
        Some(Lifetime {
            start_timestamp: start.map(at),
            end_timestamp: end.map(at),
        })
    }

    /// A made-up site: grid 1 → meter 2, which has a retired meter 3, a meter 4
    /// not yet active, and an HVAC 5 over meter 6.
    #[test]
    fn the_spec_leaves_out_inactive_and_unsupported_components_with_notes() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let microgrid = Microgrid {
            id: 42,
            name: "test site".into(),
            ..Default::default()
        };
        let mut retired = component(3, C::Meter, "old meter");
        retired.operational_lifetime = life(None, Some(999));
        let mut later = component(4, C::Meter, "");
        later.operational_lifetime = life(Some(1_001), None);
        let components = [
            component(1, C::GridConnectionPoint, "grid"),
            component(2, C::Meter, "main"),
            retired,
            later,
            component(5, C::Hvac, "air"),
            component(6, C::Meter, "hvac meter"),
        ];
        let mut ended = edge(1, 6);
        ended.operational_lifetime = life(None, Some(500));
        let connections = [
            edge(1, 2),
            edge(2, 3),
            edge(2, 4),
            edge(2, 5),
            edge(5, 6),
            ended,
            edge(2, 77),
        ];
        let fetched = spec_of(&microgrid, &components, &connections, now).unwrap();
        assert_eq!(fetched.spec.id, Some(42));
        assert_eq!(fetched.spec.name.as_deref(), Some("test site"));
        let ids: Vec<u64> = fetched
            .spec
            .components
            .iter()
            .map(|c| c.id().unwrap())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(sorted, vec![1, 2, 6]);
        let mut edges = fetched.spec.connections.clone();
        edges.sort();
        assert_eq!(edges, vec![(1, 2), (2, 6)]);
        assert_eq!(
            fetched.left_out,
            vec![
                "component 3 (old meter) is retired; left out",
                "component 4 is not yet active; left out",
                "connection 1 → 6 is retired; left out",
                "connection 2 → 77 names a component the microgrid does not list; left out",
                "component 5 (air): cannot simulate category HVAC; left out",
            ]
        );
    }

    /// A component whose only parent is retired is left out, with what it
    /// leads to, as cut off from the grid; so is one whose only connection is
    /// retired, with no component to blame.
    #[test]
    fn a_component_cut_off_from_the_grid_is_left_out() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut retired = component(2, C::Meter, "old meter");
        retired.operational_lifetime = life(None, Some(999));
        let components = [
            component(1, C::GridConnectionPoint, "grid"),
            retired,
            component(3, C::Meter, "sub meter"),
            component(4, C::Meter, ""),
            component(5, C::Meter, ""),
        ];
        let retired_edge = ElectricalComponentConnection {
            operational_lifetime: life(None, Some(999)),
            ..edge(1, 5)
        };
        let fetched = spec_of(
            &Microgrid::default(),
            &components,
            &[
                edge(1, 2),
                edge(99, 3),
                edge(2, 3),
                edge(3, 4),
                retired_edge,
            ],
            now,
        )
        .unwrap();
        let ids: Vec<u64> = fetched
            .spec
            .components
            .iter()
            .filter_map(|c| c.id())
            .collect();
        assert_eq!(ids, vec![1]);
        assert!(fetched.spec.connections.is_empty());
        let mut left_out = fetched.left_out;
        left_out.sort();
        assert_eq!(
            left_out,
            vec![
                "component 2 (old meter) is retired; left out",
                "component 3 (sub meter) is cut off from the grid by component 2 (old meter); \
                 left out",
                "component 4 is cut off from the grid by component 3 (sub meter); left out",
                "component 5 is cut off from the grid; left out",
                "connection 1 → 5 is retired; left out",
                "connection 99 → 3 names a component the microgrid does not list; left out",
            ]
        );
    }

    #[test]
    fn a_microgrid_with_no_active_component_says_why() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut retired = component(1, C::GridConnectionPoint, "");
        retired.operational_lifetime = life(None, Some(999));
        let err = spec_of(&Microgrid::default(), &[retired], &[], now).unwrap_err();
        assert!(
            err.contains("no component is in its operational lifetime (component 1 is retired"),
            "{err}"
        );
    }

    /// A hybrid inverter under a meter that is cut off from the grid is cut off
    /// too, and one under a CHP is not written; one under a kept meter is, and
    /// so is one under a CHP and a kept meter.
    #[test]
    fn a_hybrid_inverter_goes_under_a_kept_parent_that_takes_successors() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut retired = component(3, C::Meter, "");
        retired.operational_lifetime = life(None, Some(999));
        let components = [
            component(1, C::GridConnectionPoint, "grid"),
            component(2, C::Meter, "main"),
            retired,
            component(4, C::Meter, "cut off"),
            hybrid(5),
            component(6, C::Chp, "chp"),
            hybrid(7),
            hybrid(8),
            component(9, C::Battery, ""),
            hybrid(10),
        ];
        let edges = [
            edge(1, 2),
            edge(1, 3),
            edge(3, 4),
            edge(4, 5),
            edge(2, 6),
            edge(6, 7),
            edge(2, 8),
            edge(8, 9),
            edge(6, 10),
            edge(2, 10),
        ];
        let fetched = spec_of(&Microgrid::default(), &components, &edges, now).unwrap();
        let (input, notes) = fetched.module_input();
        assert_eq!(
            input["commented"],
            serde_json::json!([{
                "parent": 2,
                "kind": "hybrid_inverter",
                "components": [{"type": "battery", "id": 9},
                               {"type": "battery_inverter", "id": 8}],
                "connections": [[8, 9]],
            }, {
                "parent": 2,
                "kind": "hybrid_inverter",
                "components": [{"type": "battery_inverter", "id": 10}],
                "connections": [],
            }])
        );
        assert_eq!(
            notes,
            vec![
                "hybrid inverter 7 is not written: none of its parents that are left can \
                 have components under it",
                "hybrid inverter 8 and the components that go with it are written commented \
                 out under component 2",
                "hybrid inverter 10 and the components that go with it are written commented \
                 out under component 2",
            ]
        );
    }

    /// Meter 4 is under hybrid inverter 3 and under meter 6, whose parent is
    /// retired: 6 is cut off first, so 4 goes with the hybrid inverter. An
    /// inverter of no type over a battery goes into a tree of its own kind.
    #[test]
    fn what_only_an_inverter_tree_leads_to_goes_with_it() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut retired = component(7, C::Meter, "");
        retired.operational_lifetime = life(None, Some(999));
        let components = [
            component(1, C::GridConnectionPoint, ""),
            component(2, C::Meter, ""),
            hybrid(3),
            component(4, C::Meter, ""),
            component(6, C::Meter, ""),
            retired,
            component(8, C::Inverter, ""),
            component(9, C::Battery, ""),
        ];
        let edges = [
            edge(1, 2),
            edge(2, 3),
            edge(3, 4),
            edge(1, 7),
            edge(7, 6),
            edge(6, 4),
            edge(2, 8),
            edge(8, 9),
        ];
        let fetched = spec_of(&Microgrid::default(), &components, &edges, now).unwrap();
        let ids: Vec<u64> = fetched
            .spec
            .components
            .iter()
            .filter_map(|c| c.id())
            .collect();
        let mut sorted = ids;
        sorted.sort();
        assert_eq!(sorted, vec![1, 2]);
        assert!(
            fetched
                .left_out
                .contains(&"component 6 is cut off from the grid by component 7; left out".into()),
            "{:?}",
            fetched.left_out
        );
        let (input, _) = fetched.module_input();
        assert_eq!(input["commented"][0]["kind"], "hybrid_inverter");
        assert_eq!(
            input["commented"][0]["connections"],
            serde_json::json!([[3, 4]])
        );
        assert_eq!(input["commented"][1]["kind"], "inverter_of_no_type");
        assert_eq!(
            input["commented"][1]["connections"],
            serde_json::json!([[8, 9]])
        );
    }

    #[test]
    fn a_microgrid_needs_one_grid_connection_point() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let err = spec_of(
            &Microgrid::default(),
            &[component(2, C::Meter, "")],
            &[],
            now,
        )
        .unwrap_err();
        assert_eq!(err, "microgrid 0: no grid connection point is left");
        let grids = [
            component(1, C::GridConnectionPoint, ""),
            component(2, C::GridConnectionPoint, ""),
        ];
        let err = spec_of(&Microgrid::default(), &grids, &[], now).unwrap_err();
        assert_eq!(
            err,
            "microgrid 0: it has 2 grid connection points, component 1 and component 2; a \
             module has one"
        );
        let err = spec_of(&Microgrid::default(), &[], &[], now).unwrap_err();
        assert_eq!(err, "microgrid 0: the Assets API lists no components");
    }

    /// A label or a status drives no terminal: control characters are escaped.
    #[test]
    fn labels_escape_control_characters() {
        let names = HashMap::from([(3, "a\u{1b}]52;c;x\u{7}b")]);
        assert_eq!(
            component_label(&names, 3),
            "component 3 (a\\u{1b}]52;c;x\\u{7}b)"
        );
        let status = tonic::Status::unavailable("refused\u{1b}[2J");
        assert_eq!(status_text(&status), "Unavailable: refused\\u{1b}[2J");
    }

    #[test]
    fn lifetime_ends_are_inclusive() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(outside_lifetime(None, now), None);
        assert_eq!(
            outside_lifetime(life(Some(1_000), Some(1_000)).as_ref(), now),
            None
        );
        assert_eq!(
            outside_lifetime(life(Some(1_001), None).as_ref(), now),
            Some("is not yet active")
        );
        assert_eq!(
            outside_lifetime(life(None, Some(999)).as_ref(), now),
            Some("is retired")
        );
    }
}

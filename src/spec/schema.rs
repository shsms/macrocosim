//! The JSON Schema of the topology document, derived from the spec
//! types. It is built at run time, never checked in.

use super::TopologyDoc;

/// The JSON Schema of [`TopologyDoc`].
pub fn topology_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(TopologyDoc))
        .expect("a JSON Schema serializes to JSON")
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    /// Every object schema anywhere in `schema`: each one with
    /// `properties`.
    fn objects(schema: &Value) -> Vec<&Value> {
        let mut out = Vec::new();
        let mut stack = vec![schema];
        while let Some(v) = stack.pop() {
            match v {
                Value::Object(map) => {
                    if map.contains_key("properties") {
                        out.push(v);
                    }
                    stack.extend(map.values());
                }
                Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
        out
    }

    /// The object schema of the component type tagged `tag`.
    fn component<'a>(schema: &'a Value, tag: &str) -> &'a Value {
        objects(schema)
            .into_iter()
            .find(|o| o["properties"]["type"]["const"] == tag)
            .unwrap_or_else(|| panic!("no component type {tag}"))
    }

    #[test]
    fn the_schema_names_every_type_and_field() {
        let schema = topology_schema();
        for tag in [
            "grid_connection_point",
            "meter",
            "battery",
            "battery_inverter",
            "solar_inverter",
            "chp",
            "ev_charger",
            "steam_boiler",
            "wind_turbine",
            "power_transformer",
            "breaker",
        ] {
            let properties = &component(&schema, tag)["properties"];
            for field in ["id", "name", "stream_jitter_pct", "health"] {
                assert!(properties[field].is_object(), "{tag}.{field}");
            }
        }
        let battery = &component(&schema, "battery")["properties"];
        for field in ["capacity_wh", "initial_soc_pct", "soc_protect_margin_pct"] {
            assert!(battery[field].is_object(), "battery.{field}");
        }
        assert!(schema["properties"]["schema_version"].is_object());
        assert!(
            schema.to_string().contains("\"telemetry_only\""),
            "modes in snake case"
        );
    }

    /// Unknown fields are refused, so the schema says so for every
    /// object.
    #[test]
    fn every_object_refuses_other_fields() {
        let schema = topology_schema();
        let objects = objects(&schema);
        // The document, the microgrid and the eleven component types.
        assert_eq!(objects.len(), 13);
        for o in objects {
            assert_eq!(o["additionalProperties"], false, "{o}");
        }
    }
}

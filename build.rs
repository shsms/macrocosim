// The proto APIs are vendored under macrocosim's own `submodules/`.
// The override env var lets a downstream packager point at a private
// mirror of the microgrid proto without editing build.rs; frequenz-api-common
// still comes from the assets submodule.
use std::path::PathBuf;

fn main() -> Result<(), std::io::Error> {
    let microgrid_root = std::env::var("MACROCOSIM_PROTO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("submodules/frequenz-api-microgrid"));
    let assets_root = PathBuf::from("submodules/frequenz-api-assets");
    let dispatch_root = PathBuf::from("submodules/frequenz-api-dispatch");

    let microgrid_proto =
        microgrid_root.join("proto/frequenz/api/microgrid/v1alpha18/microgrid.proto");
    let assets_proto =
        assets_root.join("proto/frequenz/api/platformassets/v1alpha1/platformassets.proto");
    // dispatch v1 imports frequenz-api-common v1alpha8 (electrical_components,
    // pagination, streaming/event, types/interval) — the common root below
    // covers it, so it needs no vendored common of its own.
    let dispatch_proto = dispatch_root.join("proto/frequenz/api/dispatch/v1/dispatch.proto");
    // Microgrid v1alpha18 pins frequenz-api-common v0.8.3 and platformassets
    // v1alpha1 pins v0.8.11. v0.8.11 keeps v0.8.3's wire format: it adds
    // messages, some of which platformassets imports, and renames
    // ELECTRICAL_COMPONENT_OPERATIONAL_MODE_TELEMTRY_ONLY to
    // ..._TELEMETRY_ONLY. So common is generated once, from the assets
    // submodule's copy.
    let common_proto_root = assets_root.join("submodules/frequenz-api-common/proto");
    let google_proto_root = microgrid_root.join("submodules/api-common-protos");

    println!("cargo:rerun-if-env-changed=MACROCOSIM_PROTO_ROOT");
    // Watch the whole proto trees (cargo accepts directories), not
    // just the three root files: a submodule bump that only touches
    // an imported proto (e.g. frequenz-api-common) must regenerate
    // too, or the crate keeps compiling stale generated code.
    println!("cargo:rerun-if-changed={}", microgrid_root.display());
    println!("cargo:rerun-if-changed={}", assets_root.display());
    println!("cargo:rerun-if-changed={}", dispatch_root.display());
    println!("cargo:rerun-if-changed={}", google_proto_root.display());

    tonic_prost_build::configure()
        .disable_comments(["."])
        .include_file("proto_v1_alpha18.rs")
        .compile_well_known_types(false)
        .compile_protos(
            &[
                microgrid_proto.as_path(),
                assets_proto.as_path(),
                dispatch_proto.as_path(),
            ],
            &[
                microgrid_root.join("proto").as_path(),
                assets_root.join("proto").as_path(),
                dispatch_root.join("proto").as_path(),
                common_proto_root.as_path(),
                google_proto_root.as_path(),
            ],
        )
        .inspect_err(|e| {
            eprintln!("Could not compile protobuf files. Error: {:?}", e);
        })
}

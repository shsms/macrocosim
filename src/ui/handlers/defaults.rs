//! `GET /api/defaults` — the per-type defaults the engine fills into
//! a spec before it builds it, read-only: one object per component
//! type, keyed by the topology document's `"type"` tag, with the JSON
//! field names of the fields it fills.

use crate::spec::Defaults;
use crate::ui::api::Json;

pub(in crate::ui) async fn defaults() -> Json<Defaults> {
    Json(Defaults::default())
}

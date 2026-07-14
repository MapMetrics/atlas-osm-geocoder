//! Pass 2: postcode emitter.
//!
//! Streams nodes and ways out of the `.osm.pbf`, collects every distinct
//! `addr:postcode` value (nodes directly; ways via their member-node
//! centroid, same helper as `layers::poi`/`layers::address`), and writes
//! `postcode.geojsonl` via `emit::LayerWriter` — one `Feature` per distinct
//! code at the MEAN coordinate of all its members. Property contract
//! mirrors `extract_country_v3.py`'s `extract_postcode` (see
//! `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! `extract_postcode` ~lines 590-617) exactly, adapted from ClickHouse
//! `postcodes_v3` row inputs (already one row per distinct code, with its
//! own precomputed `(lon, lat)`) to raw OSM `addr:postcode` tag inputs
//! (grouped client-side here instead, one Feature per distinct code emitted
//! at the arithmetic mean of every member's coordinate — the python has no
//! grouping step of its own since its source table is already
//! pre-aggregated upstream):
//!
//! - `carmen:text`: the postcode string itself, cleaned via
//!   [`clean_alias`] (mirrors the python's `clean_alias(pc)` — strip commas,
//!   collapse whitespace; `carmen:text` is comma-split downstream so a raw
//!   code containing a comma would corrupt the property).
//! - `carmen:center`: `[lon, lat]`, the MEAN of every member's own
//!   coordinate (node's own location, or a way's member-node centroid) —
//!   the python's `postcodes_v3` rows already carry one precomputed
//!   `(lon, lat)` per code (server-side aggregation this crate has no
//!   equivalent table for), so this crate's closest-available signal is the
//!   plain mean over every raw-tag occurrence of that code.
//! - No `carmen:score` property (the python's `extract_postcode` does not
//!   emit one either — see its function body, unlike `extract_place`/
//!   `extract_street`/`extract_region` which all set one).
//! - Feature id: `hid("postcode|" + code)` — namespaced (unlike the python's
//!   bare `hid(r["id"])`, which has no OSM-tag equivalent id source here)
//!   so it can never collide with another layer's `hid()` input space, and
//!   is deterministic across runs for the same code.

use std::path::Path;

use osmpbf::{Element, ElementReader};

use crate::emit::LayerWriter;
use crate::error::ExtractError;
use crate::ids::hid;
use crate::layers::common::{tags_to_map, way_centroid};
use crate::nodes::NodeTable;
use crate::taxonomy::TagMap;

/// Mirrors `extract_country_v3.py`'s `clean_alias`: strip commas
/// (`carmen:text` is comma-split downstream) and collapse whitespace.
fn clean_alias(s: &str) -> String {
    s.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn postcode_from_tags(tags: &TagMap) -> Option<String> {
    let pc = tags.get("addr:postcode")?;
    if pc.is_empty() {
        return None;
    }
    Some(pc.clone())
}

/// One group's accumulated members for a distinct postcode: running sum of
/// coordinates + count, so the mean can be computed once at emit time
/// without retaining every individual coordinate.
#[derive(Default)]
struct Group {
    sum_lon: f64,
    sum_lat: f64,
    n: u64,
}

/// Pass 2: extract distinct `addr:postcode` values from `pbf` (nodes + ways)
/// and write `{out_dir}/postcode.geojsonl` via `LayerWriter` — one `Feature`
/// per distinct code at the mean coordinate of its members. Returns the
/// number of features written.
pub fn extract(pbf: &Path, nodes: &NodeTable, out_dir: &Path) -> Result<u64, ExtractError> {
    // Insertion-ordered grouping (mirrors `layers::address`'s pattern) so
    // output is deterministic regardless of the underlying HashMap's
    // iteration order.
    let mut groups: Vec<(String, Group)> = Vec::new();
    let mut group_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut way_skips: u64 = 0;

    let mut record = |code: String, lon: f64, lat: f64| {
        let idx = *group_index.entry(code.clone()).or_insert_with(|| {
            groups.push((code, Group::default()));
            groups.len() - 1
        });
        let g = &mut groups[idx].1;
        g.sum_lon += lon;
        g.sum_lat += lat;
        g.n += 1;
    };

    let reader = ElementReader::from_path(pbf)?;
    reader.for_each(|element| match element {
        Element::Node(n) => {
            let tags = tags_to_map(n.tags());
            if let Some(code) = postcode_from_tags(&tags) {
                record(code, n.lon(), n.lat());
            }
        }
        Element::DenseNode(n) => {
            let tags = tags_to_map(n.tags());
            if let Some(code) = postcode_from_tags(&tags) {
                record(code, n.lon(), n.lat());
            }
        }
        Element::Way(w) => {
            let tags = tags_to_map(w.tags());
            if let Some(code) = postcode_from_tags(&tags) {
                let refs: Vec<i64> = w.refs().collect();
                match way_centroid(&refs, nodes) {
                    Some((lon, lat)) => record(code, lon, lat),
                    None => way_skips += 1,
                }
            }
        }
        _ => {}
    })?;

    if way_skips > 0 {
        eprintln!("layers::postcode: skipped {way_skips} way(s) with zero resolvable member nodes");
    }

    let path = out_dir.join("postcode.geojsonl");
    let mut writer = LayerWriter::new(&path)?;

    for (code, g) in &groups {
        if g.n == 0 {
            continue;
        }
        let text = clean_alias(code);
        if text.is_empty() {
            continue;
        }
        let lon = g.sum_lon / g.n as f64;
        let lat = g.sum_lat / g.n as f64;

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.into());
        props.insert("carmen:center".into(), serde_json::json!([lon, lat]));

        let feature_id = hid(&format!("postcode|{code}"));
        let geometry = serde_json::json!({
            "type": "Point",
            "coordinates": [lon, lat],
        });
        writer.feature(feature_id, &props, geometry)?;
    }

    Ok(writer.count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_alias_strips_commas_and_collapses_whitespace() {
        assert_eq!(clean_alias("98,000"), "98 000");
        assert_eq!(clean_alias("  98000  "), "98000");
    }

    #[test]
    fn postcode_from_tags_none_when_missing_or_empty() {
        let mut t = TagMap::default();
        assert!(postcode_from_tags(&t).is_none());
        t.insert("addr:postcode".into(), "".into());
        assert!(postcode_from_tags(&t).is_none());
        t.insert("addr:postcode".into(), "98000".into());
        assert_eq!(postcode_from_tags(&t), Some("98000".to_string()));
    }
}

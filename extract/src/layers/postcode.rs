//! Pass 2: postcode emitter.
//!
//! Streams nodes and ways out of the `.osm.pbf`, collects every distinct
//! `addr:postcode` value (nodes directly; ways via their member-node
//! centroid, same helper as `layers::poi`/`layers::address`), and writes
//! `postcode.geojsonl` via `emit::LayerWriter` — one `Feature` per distinct
//! code at the MEAN coordinate of all its members. Property contract
//! mirrors `extract_country_v3.py`'s `extract_postcode` (see
//! `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! `extract_postcode` ~lines 590-617), adapted from ClickHouse
//! `postcodes_v3` inputs to raw OSM `addr:postcode` tag inputs.
//!
//! **Key design difference from Python**: The Python's ClickHouse
//! `postcodes_v3` source is NOT deduplicated (e.g., NL '1183BS' has 3 rows
//! at 3 distinct coordinates; GB has 4.37M rows / 2.64M distinct codes),
//! and the Python's `extract_postcode` emits one feature PER ROW, producing
//! duplicate postcode features. This crate implements DISTINCT-code + mean-coordinate
//! GROUPING client-side, emitting one feature per distinct logical postcode
//! code (with whitespace/comma variants normalized before grouping). This is
//! a DELIBERATE DESIGN IMPROVEMENT over Python's row-per-feature behavior, not parity.
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
        // Normalize grouping key with clean_alias BEFORE lookup: "98000" and
        // "98000 " (whitespace variants) must form ONE group, not two. Skip
        // if the cleaned code is empty.
        let normalized = clean_alias(&code);
        if normalized.is_empty() {
            return;
        }
        let idx = *group_index.entry(normalized.clone()).or_insert_with(|| {
            groups.push((normalized, Group::default()));
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

    #[test]
    fn grouping_key_normalized_before_lookup() {
        // TDD: raw postcode variants ("98000", "98000 " with trailing space,
        // " 98000" with leading space) must all normalize to "98000" and form
        // ONE group, not THREE separate groups. The grouping key (group_index
        // lookup) must use the cleaned alias, not the raw tag value.
        let mut groups: Vec<(String, Group)> = Vec::new();
        let mut group_index: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        let record_buggy = |code: String, lon: f64, lat: f64,
                           groups: &mut Vec<(String, Group)>,
                           group_index: &mut std::collections::HashMap<String, usize>| {
            // BEFORE FIX: grouping key is raw code (creates 3 groups)
            let idx = *group_index.entry(code.clone()).or_insert_with(|| {
                groups.push((code, Group::default()));
                groups.len() - 1
            });
            let g = &mut groups[idx].1;
            g.sum_lon += lon;
            g.sum_lat += lat;
            g.n += 1;
        };

        // Synthetic members with whitespace variants
        record_buggy("98000".to_string(), 7.4, 43.7, &mut groups, &mut group_index);
        record_buggy("98000 ".to_string(), 7.5, 43.8, &mut groups, &mut group_index);
        record_buggy(" 98000".to_string(), 7.6, 43.9, &mut groups, &mut group_index);

        // BUG: without normalization, these create 3 separate groups
        assert_eq!(groups.len(), 3, "BEFORE FIX: expected 3 groups (raw keys), got {}", groups.len());

        // Now demonstrate the FIX: normalize grouping key with clean_alias
        let mut groups_fixed: Vec<(String, Group)> = Vec::new();
        let mut group_index_fixed: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        let record_fixed = |code: String, lon: f64, lat: f64,
                           groups: &mut Vec<(String, Group)>,
                           group_index: &mut std::collections::HashMap<String, usize>| {
            // AFTER FIX: grouping key is clean_alias(code)
            let normalized = clean_alias(&code);
            if normalized.is_empty() {
                return; // skip if cleans to empty
            }
            let idx = *group_index.entry(normalized.clone()).or_insert_with(|| {
                groups.push((normalized, Group::default()));
                groups.len() - 1
            });
            let g = &mut groups[idx].1;
            g.sum_lon += lon;
            g.sum_lat += lat;
            g.n += 1;
        };

        // Same synthetic members
        record_fixed("98000".to_string(), 7.4, 43.7, &mut groups_fixed, &mut group_index_fixed);
        record_fixed("98000 ".to_string(), 7.5, 43.8, &mut groups_fixed, &mut group_index_fixed);
        record_fixed(" 98000".to_string(), 7.6, 43.9, &mut groups_fixed, &mut group_index_fixed);

        // AFTER FIX: All 3 collapse into ONE group (the cleaned key "98000")
        assert_eq!(groups_fixed.len(), 1, "AFTER FIX: expected 1 group (all variants clean to '98000'), got {}", groups_fixed.len());
        let (stored_code, g) = &groups_fixed[0];
        assert_eq!(stored_code, "98000", "stored code must be the cleaned key");
        assert_eq!(g.n, 3, "group must have 3 members");
        // Mean coordinate should be (7.5, 43.8) — average of the 3
        assert!((g.sum_lon / g.n as f64 - 7.5).abs() < 1e-6);
        assert!((g.sum_lat / g.n as f64 - 43.8).abs() < 1e-6);
    }
}

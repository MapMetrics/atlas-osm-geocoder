//! Pass 2: address emitter.
//!
//! Streams nodes and ways out of the `.osm.pbf`, collects every element that
//! carries both `addr:housenumber` and `addr:street`, groups the results by
//! `(addr:street, resolved locality)`, and writes `address.geojsonl` via
//! `emit::LayerWriter`. Property contract mirrors `extract_country_v3.py`'s
//! `extract_address` (see
//! `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! ~lines 327-391) exactly, adapted from ClickHouse `addresses_v3` row
//! inputs (which are pre-grouped server-side via `GROUP BY addr_street,
//! parent_locality`) to raw OSM tag inputs (grouped client-side here
//! instead):
//!
//! - Members: every `Node` and `Way` (way resolved to its member-node
//!   centroid, same helper as `layers::poi`) carrying non-empty
//!   `addr:housenumber` AND `addr:street`. Elements missing either tag are
//!   skipped, matching the python's `WHERE addr_street != ''` filter (an
//!   empty housenumber would also break the parallel-array contract).
//! - Grouping key: `(addr:street, locality)`, where `locality` comes from
//!   `HierarchyIndex::resolve` (the python's `parent_locality` column is the
//!   OSM-tag pipeline's closest equivalent — a hierarchy-derived locality
//!   name rather than a raw tag).
//! - Within a group, members are sorted by housenumber before segmenting so
//!   output is deterministic regardless of PBF scan order (the python has
//!   no such sort — ClickHouse's `groupArray` order is whatever the query
//!   happened to stream rows in — but determinism is a strict improvement
//!   here and does not change the contract). The sort key is the raw
//!   `addr:housenumber` string compared byte-wise (i.e. lexicographic, NOT
//!   numeric): matches the python, which never parses `hns` as numbers
//!   anywhere in `extract_address` (they flow straight from `groupArray` to
//!   `numbers = [(h or "") for h in seg_hn]` as strings).
//! - Segmentation: each group's members are split into consecutive chunks
//!   of at most `CAP` (2000), one Feature per chunk — identical to the
//!   python's `for start in range(0, m, cap)`.
//! - `carmen:text`: `dedup_join([street, "street locality"])` — i.e.
//!   `"street,street locality"` when a locality is present, or just
//!   `"street"` when it is not (dedup_join drops the second alias if it's
//!   identical to the first, which only happens when locality is empty and
//!   the format string degenerates to `street` again — mirroring the
//!   python's `f"{street} {locality}" if locality else street` guard).
//! - `carmen:center`: the segment's coordinate **centroid** (arithmetic mean
//!   of member lon/lats), NOT the first member's coordinate — the python
//!   computes `clon = sum(seg_lon) / len(seg_lon)` (same for lat).
//! - `carmen:addressnumber`: array of `addr:housenumber` strings, in the
//!   same (sorted) member order as the `MultiPoint` coordinates — strictly
//!   parallel, verified by the Monaco integration test on every line.
//! - Feature id: `hid("addr:" + street + ":" + locality + ":" +
//!   segment_index)` (segment_index is 0-based, incrementing once per
//!   emitted segment within a group) — deliberately namespaced with an
//!   `addr:` prefix (unlike the python's bare `f"{street}|{locality}|{start}"`)
//!   so ids can never collide with another layer's `hid()` input space.

use std::path::Path;

use osmpbf::{Element, ElementReader};

use crate::emit::LayerWriter;
use crate::error::ExtractError;
use crate::hierarchy::HierarchyIndex;
use crate::ids::hid;
use crate::layers::common::{tags_to_map, way_centroid};
use crate::nodes::NodeTable;
use crate::taxonomy::TagMap;

/// Maximum members per emitted `MultiPoint` feature (mirrors the python's
/// `cap=2000` default).
const CAP: usize = 2000;

/// One resolved address candidate, gathered during the streaming pass,
/// before grouping/segmenting.
struct Candidate {
    street: String,
    housenumber: String,
    postcode: Option<String>,
    lon: f64,
    lat: f64,
}

/// Mirrors `layers::poi`'s `dedup_join`: name-first, case-insensitive
/// deduped, comma-joined. Local copy rather than a `pub(crate)` import from
/// `poi` because the two modules' alias lists have different shapes/cardinality
/// and `poi`'s version is more naturally read alongside its own `carmen_text`
/// helper; if a third layer needs it, it should move to `layers::common`.
fn dedup_join(parts: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in parts {
        let cleaned = p.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" ");
        if cleaned.is_empty() {
            continue;
        }
        let key = cleaned.to_lowercase();
        if seen.insert(key) {
            out.push(cleaned);
        }
    }
    out.join(",")
}

/// `carmen:text` = `"street,street locality"` (or just `"street"` when
/// `locality` is empty) — mirrors the python's
/// `dedup_join([street, f"{street} {locality}" if locality else street])`.
fn carmen_text(street: &str, locality: &str) -> String {
    let second = if locality.is_empty() {
        street.to_string()
    } else {
        format!("{street} {locality}")
    };
    dedup_join(&[street.to_string(), second])
}

/// One group's collected members, keyed by `(street, locality)`. Grouping
/// itself is a plain `Vec` scan (bounded by extract sizes this crate
/// targets — see `nodes::NodeTable`'s doc comment) rather than a `HashMap`,
/// so member order within a group is preserved for the pre-sort step; the
/// map below is only used to find/create a group's `Vec` by key.
struct Group {
    street: String,
    locality: String,
    members: Vec<(String, f64, f64, Option<String>)>, // (housenumber, lon, lat, postcode)
}

/// Split `members` (already sorted by housenumber) into `<=CAP`-sized
/// consecutive segments and emit one `Feature` per segment via `writer`.
/// Returns the number of segments emitted.
fn emit_group(
    writer: &mut LayerWriter,
    street: &str,
    locality: &str,
    members: &[(String, f64, f64, Option<String>)],
) -> Result<u64, ExtractError> {
    if members.is_empty() {
        return Ok(0);
    }

    let text = carmen_text(street, locality);
    let mut emitted = 0u64;

    for (segment_index, chunk) in members.chunks(CAP).enumerate() {
        let coords: Vec<serde_json::Value> = chunk
            .iter()
            .map(|(_, lon, lat, _)| serde_json::json!([lon, lat]))
            .collect();
        let numbers: Vec<serde_json::Value> = chunk
            .iter()
            .map(|(hn, _, _, _)| serde_json::Value::from(hn.clone()))
            .collect();

        let clon: f64 = chunk.iter().map(|(_, lon, _, _)| lon).sum::<f64>() / chunk.len() as f64;
        let clat: f64 = chunk.iter().map(|(_, _, lat, _)| lat).sum::<f64>() / chunk.len() as f64;

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.clone().into());
        props.insert("carmen:center".into(), serde_json::json!([clon, clat]));
        props.insert("carmen:addressnumber".into(), serde_json::Value::Array(numbers));
        if !locality.is_empty() {
            props.insert("city".into(), locality.into());
        }
        // Emit postcode if any member in this chunk carries it.
        if let Some(postcode) = chunk.iter().find_map(|(_, _, _, pc)| pc.clone()) {
            props.insert("postcode".into(), postcode.into());
        }

        let feature_id = hid(&format!("addr:{street}:{locality}:{segment_index}"));
        let geometry = serde_json::json!({
            "type": "MultiPoint",
            "coordinates": coords,
        });
        writer.feature(feature_id, &props, geometry)?;
        emitted += 1;
    }

    Ok(emitted)
}

/// Pass 2: extract addresses from `pbf` (nodes + ways), resolve each one's
/// hierarchy locality via `hier`, group by `(addr:street, locality)`, and
/// write `{out_dir}/address.geojsonl` via `LayerWriter`. Returns the number
/// of Feature lines written (one per `<=2000`-member segment, not one per
/// address point).
pub fn extract(
    pbf: &Path,
    nodes: &NodeTable,
    hier: &HierarchyIndex,
    out_dir: &Path,
) -> Result<u64, ExtractError> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut way_skips: u64 = 0;

    let reader = ElementReader::from_path(pbf)?;
    reader.for_each(|element| match element {
        Element::Node(n) => {
            let tags = tags_to_map(n.tags());
            if let Some(c) = candidate_from_tags(&tags, n.lon(), n.lat()) {
                candidates.push(c);
            }
        }
        Element::DenseNode(n) => {
            let tags = tags_to_map(n.tags());
            if let Some(c) = candidate_from_tags(&tags, n.lon(), n.lat()) {
                candidates.push(c);
            }
        }
        Element::Way(w) => {
            let tags = tags_to_map(w.tags());
            if has_address_tags(&tags) {
                let refs: Vec<i64> = w.refs().collect();
                match way_centroid(&refs, nodes) {
                    Some((lon, lat)) => {
                        if let Some(c) = candidate_from_tags(&tags, lon, lat) {
                            candidates.push(c);
                        }
                    }
                    None => way_skips += 1,
                }
            }
        }
        _ => {}
    })?;

    if way_skips > 0 {
        eprintln!("layers::address: skipped {way_skips} way(s) with zero resolvable member nodes");
    }

    // Group by (street, locality), preserving first-seen group order for
    // deterministic output ordering (member order within a group is fixed
    // up by the housenumber sort below regardless).
    let mut groups: Vec<Group> = Vec::new();
    let mut group_index: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();

    for c in candidates {
        let parents = hier.resolve(c.lon, c.lat);
        let locality = parents.locality.unwrap_or_default();
        let key = (c.street.clone(), locality.clone());
        let idx = *group_index.entry(key).or_insert_with(|| {
            groups.push(Group {
                street: c.street.clone(),
                locality: locality.clone(),
                members: Vec::new(),
            });
            groups.len() - 1
        });
        groups[idx].members.push((c.housenumber, c.lon, c.lat, c.postcode));
    }

    let path = out_dir.join("address.geojsonl");
    let mut writer = LayerWriter::new(&path)?;

    for group in &mut groups {
        // Stable sort by housenumber (lexicographic string compare — see
        // module doc comment): ensures deterministic output regardless of
        // PBF scan order.
        group.members.sort_by(|a, b| a.0.cmp(&b.0));
        emit_group(&mut writer, &group.street, &group.locality, &group.members)?;
    }

    Ok(writer.count())
}

fn has_address_tags(tags: &TagMap) -> bool {
    tags.get("addr:housenumber").is_some_and(|v| !v.is_empty())
        && tags.get("addr:street").is_some_and(|v| !v.is_empty())
}

fn candidate_from_tags(tags: &TagMap, lon: f64, lat: f64) -> Option<Candidate> {
    if !has_address_tags(tags) {
        return None;
    }
    let street = tags.get("addr:street")?.clone();
    let housenumber = tags.get("addr:housenumber")?.clone();
    let postcode = tags.get("addr:postcode").cloned().filter(|pc| !pc.is_empty());
    Some(Candidate {
        street,
        housenumber,
        postcode,
        lon,
        lat,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carmen_text_with_locality() {
        assert_eq!(carmen_text("Main St", "Monaco"), "Main St,Main St Monaco");
    }

    #[test]
    fn carmen_text_without_locality_is_just_street() {
        assert_eq!(carmen_text("Main St", ""), "Main St");
    }

    #[test]
    fn has_address_tags_requires_both() {
        let mut t = TagMap::default();
        assert!(!has_address_tags(&t));
        t.insert("addr:housenumber".into(), "12".into());
        assert!(!has_address_tags(&t));
        t.insert("addr:street".into(), "Main St".into());
        assert!(has_address_tags(&t));
    }

    #[test]
    fn has_address_tags_rejects_empty_values() {
        let mut t = TagMap::default();
        t.insert("addr:housenumber".into(), "".into());
        t.insert("addr:street".into(), "Main St".into());
        assert!(!has_address_tags(&t));
    }

    #[test]
    fn candidate_from_tags_none_when_missing_street() {
        let mut t = TagMap::default();
        t.insert("addr:housenumber".into(), "12".into());
        assert!(candidate_from_tags(&t, 1.0, 2.0).is_none());
    }

    /// Postcode test: a group where some members carry addr:postcode
    /// and others don't must emit the postcode property on the feature,
    /// and a group with no postcodes must not emit it.
    #[test]
    fn emit_group_emits_postcode_when_any_member_has_it() {
        let dir = std::env::temp_dir().join("ae_address_postcode_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("address_postcode.geojsonl");
        let mut writer = LayerWriter::new(&path).unwrap();

        // Members: some with postcode, some without
        let members = vec![
            ("1".to_string(), 0.0, 0.0, Some("98000".to_string())),
            ("2".to_string(), 1.0, 1.0, None),
            ("3".to_string(), 2.0, 2.0, Some("98000".to_string())),
        ];

        let emitted = emit_group(&mut writer, "Postcode St", "Monaco", &members).unwrap();
        assert_eq!(emitted, 1, "Must emit exactly one feature");
        drop(writer);

        let contents = std::fs::read_to_string(&path).unwrap();
        let line = contents.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let postcode = v["properties"]["postcode"].as_str();
        assert_eq!(postcode, Some("98000"), "postcode property must be present");
    }

    #[test]
    fn emit_group_omits_postcode_when_no_members_have_it() {
        let dir = std::env::temp_dir().join("ae_address_no_postcode_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("address_no_postcode.geojsonl");
        let mut writer = LayerWriter::new(&path).unwrap();

        let members = vec![
            ("1".to_string(), 0.0, 0.0, None),
            ("2".to_string(), 1.0, 1.0, None),
        ];

        let emitted = emit_group(&mut writer, "No Postcode St", "Nowhere", &members).unwrap();
        assert_eq!(emitted, 1);
        drop(writer);

        let contents = std::fs::read_to_string(&path).unwrap();
        let line = contents.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let has_postcode = v["properties"].get("postcode").is_some();
        assert!(!has_postcode, "postcode property must not be present");
    }

    /// Segment-split unit test: 4001 synthetic members in one group must
    /// split into exactly 3 emitted features of sizes 2000/2000/1, with
    /// `carmen:addressnumber` length exactly matching `MultiPoint`
    /// coordinate count on every segment.
    #[test]
    fn emit_group_splits_4001_members_into_2000_2000_1() {
        let dir = std::env::temp_dir().join("ae_address_split_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("address_split.geojsonl");
        let mut writer = LayerWriter::new(&path).unwrap();

        let members: Vec<(String, f64, f64, Option<String>)> = (0..4001)
            .map(|i| (format!("{i}"), i as f64 * 0.0001, i as f64 * 0.0001, None))
            .collect();

        let emitted = emit_group(&mut writer, "Synthetic St", "Testville", &members).unwrap();
        assert_eq!(emitted, 3, "4001 members must split into exactly 3 segments");
        drop(writer);

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 3);

        let sizes: Vec<usize> = lines
            .iter()
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                let coords = v["geometry"]["coordinates"].as_array().unwrap();
                let numbers = v["properties"]["carmen:addressnumber"].as_array().unwrap();
                assert_eq!(
                    coords.len(),
                    numbers.len(),
                    "carmen:addressnumber must stay parallel to MultiPoint coords"
                );
                coords.len()
            })
            .collect();
        assert_eq!(sizes, vec![2000, 2000, 1]);

        // Feature ids must be distinct across segments (segment_index varies).
        let ids: Vec<u64> = lines
            .iter()
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                v["id"].as_u64().unwrap()
            })
            .collect();
        assert_eq!(ids[0], hid("addr:Synthetic St:Testville:0"));
        assert_eq!(ids[1], hid("addr:Synthetic St:Testville:1"));
        assert_eq!(ids[2], hid("addr:Synthetic St:Testville:2"));
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);
    }

    #[test]
    fn emit_group_empty_members_emits_nothing() {
        let dir = std::env::temp_dir().join("ae_address_empty_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("address_empty.geojsonl");
        let mut writer = LayerWriter::new(&path).unwrap();
        let emitted = emit_group(&mut writer, "Empty St", "Nowhere", &[] as &[(String, f64, f64, Option<String>)]).unwrap();
        assert_eq!(emitted, 0);
    }

    #[test]
    fn emit_group_centroid_is_mean_of_members() {
        let dir = std::env::temp_dir().join("ae_address_centroid_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("address_centroid.geojsonl");
        let mut writer = LayerWriter::new(&path).unwrap();

        let members = vec![
            ("1".to_string(), 0.0, 0.0, None),
            ("2".to_string(), 2.0, 4.0, None),
        ];
        emit_group(&mut writer, "Center St", "Town", &members).unwrap();
        drop(writer);

        let contents = std::fs::read_to_string(&path).unwrap();
        let line = contents.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let center = v["properties"]["carmen:center"].as_array().unwrap();
        assert_eq!(center[0].as_f64().unwrap(), 1.0);
        assert_eq!(center[1].as_f64().unwrap(), 2.0);
    }
}

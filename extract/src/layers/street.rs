//! Pass 2: street emitter.
//!
//! Streams ways out of the `.osm.pbf`, gates each through a named-highway
//! filter, resolves its centroid via `way_centroid`, and writes
//! `street.geojsonl` via `emit::LayerWriter`. Property/geometry contract
//! mirrors `extract_country_v3.py`'s `extract_street` (see
//! `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! `extract_street` ~lines 394-447) exactly, adapted from ClickHouse
//! `streets_v3` row inputs (one pre-aggregated row per named way) to raw OSM
//! way inputs:
//!
//! - **No (name, locality) grouping.** Re-reading the python's body (not
//!   just its docstring) shows `extract_street` emits exactly one `Feature`
//!   per `streets_v3` row — and `streets_v3` is *already* one row per named
//!   way (populated upstream by `streets.lua`'s `process_way`, which fires
//!   once per way carrying both `highway` and `name`; see
//!   `/Volumes/T7/osm.pbfconverter/streets.lua`). There is no `GROUP BY` in
//!   `extract_street`'s SQL and no client-side aggregation step, unlike
//!   `extract_address`'s explicit `GROUP BY addr_street, parent_locality`.
//!   This module therefore emits one `Feature` per qualifying OSM way, not
//!   grouped by name — correcting the task brief's "grouped by (name,
//!   locality)" / "MultiPoint of way midpoints per group" sketch, which the
//!   python's actual body contradicts (per the brief's own rule: "the
//!   python wins").
//! - **Geometry**: `Point` at the way's centroid (arithmetic mean of
//!   resolvable member node coordinates, via the same `way_centroid` helper
//!   `layers::poi`/`layers::address` use) — mirrors `streets_v3.lon/lat`,
//!   which is itself a single representative point per way (`extract_street`
//!   emits `{"type": "Point", "coordinates": center}`, python ~line 444).
//! - **Named-highway filter**: ANY way with non-empty `highway` tag AND
//!   non-empty `name` tag qualifies, mirroring `streets.lua`'s upstream gate
//!   (see `/Volumes/T7/osm.pbfconverter/streets.lua`,
//!   `object.tags.highway and object.tags.name`). No allowlist of highway
//!   classes; all classes from `highway=*` are accepted, achieving
//!   production parity with `build_streets_v3.sql` (`p.highway != '' AND
//!   p.name != ''` — no class restriction).
//! - `carmen:text`: `dedup_join([name, "name locality", name_en, *intl.values()])`
//!   — identical alias order to the python (~lines 413-420). This OSM-tag
//!   pipeline has no `name_en`/`names_intl` equivalent yet (no external
//!   name-translation merge at this stage), so those alias slots are simply
//!   absent from the list, not emitted as empty entries (same convention as
//!   `layers::poi`'s missing `ext_name`/`names_intl` slots).
//! - `carmen:score`: `STREET_SCORE.get(highway, 1)` — the python's fixed
//!   highway_class -> score table (~lines 55-58), with the same `1` default
//!   fallback for any class not in the table (any unknown `highway=*` class
//!   scores as 1).
//! - optional `locality` (resolved via `HierarchyIndex`), `highway_class`
//!   (the raw `highway` tag value, mirroring the python's `highway_class`
//!   property name for the same field).

use std::path::Path;

use osmpbf::{Element, ElementReader};

use crate::emit::LayerWriter;
use crate::error::ExtractError;
use crate::hierarchy::HierarchyIndex;
use crate::ids::hid;
use crate::layers::common::{tags_to_map, way_centroid};
use crate::nodes::NodeTable;
use crate::taxonomy::TagMap;

/// highway_class -> street carmen:score, verbatim port of the python's
/// `STREET_SCORE` dict (extract_country_v3.py ~lines 55-58).
fn street_score(highway_class: &str) -> i64 {
    match highway_class {
        "motorway" => 100,
        "trunk" => 70,
        "primary" => 50,
        "secondary" => 30,
        "tertiary" => 15,
        "unclassified" => 5,
        "pedestrian" => 3,
        "residential" => 2,
        _ => 1, // STREET_SCORE.get(hc, 1) default fallback
    }
}

/// Whether `tags` qualifies as a named street per this module's filter:
/// non-empty `highway` tag AND non-empty `name` tag, with no class
/// restrictions (production parity with `build_streets_v3.sql`).
fn is_named_street(tags: &TagMap) -> bool {
    let has_name = tags.get("name").is_some_and(|v| !v.is_empty());
    let has_highway = tags.get("highway").is_some_and(|v| !v.is_empty());
    has_name && has_highway
}

/// Mirrors `extract_country_v3.py`'s `clean_alias`/`dedup_join`: strip
/// commas (carmen:text is comma-split downstream), collapse whitespace,
/// name-first case-insensitive dedupe.
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

/// `carmen:text` = `dedup_join([name, "name locality"])` — mirrors the
/// python's alias order (extract_street ~lines 413-415); `name_en`/
/// `names_intl` alias slots have no OSM-tag equivalent here (see module doc).
fn carmen_text(name: &str, locality: Option<&str>) -> String {
    let mut aliases = vec![name.to_string()];
    if let Some(loc) = locality {
        if !loc.is_empty() {
            aliases.push(format!("{name} {loc}"));
        }
    }
    dedup_join(&aliases)
}

/// One resolved street-way candidate, gathered during the streaming pass.
struct Candidate {
    id: i64,
    name: String,
    highway: String,
    lon: f64,
    lat: f64,
}

/// Pass 2: extract named streets from `pbf` (ways only), resolve each one's
/// hierarchy locality via `hier`, and write `{out_dir}/street.geojsonl` via
/// `LayerWriter`. Returns the number of features written.
pub fn extract(
    pbf: &Path,
    nodes: &NodeTable,
    hier: &HierarchyIndex,
    out_dir: &Path,
) -> Result<u64, ExtractError> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut way_skips: u64 = 0;

    let reader = ElementReader::from_path(pbf)?;
    reader.for_each(|element| {
        if let Element::Way(w) = element {
            let tags = tags_to_map(w.tags());
            if is_named_street(&tags) {
                let refs: Vec<i64> = w.refs().collect();
                match way_centroid(&refs, nodes) {
                    Some((lon, lat)) => {
                        candidates.push(Candidate {
                            id: w.id(),
                            name: tags.get("name").cloned().unwrap_or_default(),
                            highway: tags.get("highway").cloned().unwrap_or_default(),
                            lon,
                            lat,
                        });
                    }
                    None => way_skips += 1,
                }
            }
        }
    })?;

    if way_skips > 0 {
        eprintln!("layers::street: skipped {way_skips} way(s) with zero resolvable member nodes");
    }

    let path = out_dir.join("street.geojsonl");
    let mut writer = LayerWriter::new(&path)?;

    for c in &candidates {
        let parents = hier.resolve(c.lon, c.lat);
        let locality = parents.locality.as_deref();

        let text = carmen_text(&c.name, locality);
        if text.is_empty() {
            // is_named_street already guarantees non-empty name, so this
            // should not happen, but never emit an empty carmen:text.
            continue;
        }

        let score = street_score(&c.highway);

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.into());
        props.insert("carmen:center".into(), serde_json::json!([c.lon, c.lat]));
        props.insert("carmen:score".into(), score.into());
        if let Some(loc) = locality {
            if !loc.is_empty() {
                props.insert("locality".into(), loc.into());
            }
        }
        if !c.highway.is_empty() {
            props.insert("highway_class".into(), c.highway.clone().into());
        }

        let feature_id = hid(&format!("w{}", c.id));
        let geometry = serde_json::json!({
            "type": "Point",
            "coordinates": [c.lon, c.lat],
        });
        writer.feature(feature_id, &props, geometry)?;
    }

    Ok(writer.count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> TagMap {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn street_score_matches_python_table() {
        assert_eq!(street_score("motorway"), 100);
        assert_eq!(street_score("trunk"), 70);
        assert_eq!(street_score("primary"), 50);
        assert_eq!(street_score("secondary"), 30);
        assert_eq!(street_score("tertiary"), 15);
        assert_eq!(street_score("unclassified"), 5);
        assert_eq!(street_score("pedestrian"), 3);
        assert_eq!(street_score("residential"), 2);
    }

    #[test]
    fn street_score_defaults_to_one_for_unlisted_class() {
        assert_eq!(street_score("service"), 1);
        assert_eq!(street_score("living_street"), 1);
        assert_eq!(street_score("track"), 1);
    }

    #[test]
    fn is_named_street_accepts_any_highway_class_when_named() {
        // Production parity: no class allowlist. Any highway=* qualifies.
        for hc in &[
            "motorway", "trunk", "primary", "secondary", "tertiary", "residential",
            "pedestrian", "living_street", "unclassified", "service", "track", "footway",
            "unknown_class", "path", "bridleway"
        ] {
            assert!(
                is_named_street(&tags(&[("highway", hc), ("name", "X")])),
                "expected {hc} with name to qualify"
            );
        }
    }

    #[test]
    fn is_named_street_requires_both_highway_and_name() {
        // Missing highway tag fails even with name.
        assert!(!is_named_street(&tags(&[("name", "Nameless")])));
        // Missing name tag fails even with highway.
        assert!(!is_named_street(&tags(&[("highway", "residential")])));
        // Empty highway tag fails.
        assert!(!is_named_street(&tags(&[("highway", ""), ("name", "X")])));
        // Empty name tag fails.
        assert!(!is_named_street(&tags(&[("highway", "residential"), ("name", "")])));
    }

    #[test]
    fn carmen_text_with_locality() {
        assert_eq!(carmen_text("Main St", Some("Monaco")), "Main St,Main St Monaco");
    }

    #[test]
    fn carmen_text_without_locality_is_just_name() {
        assert_eq!(carmen_text("Main St", None), "Main St");
    }

    #[test]
    fn carmen_text_empty_locality_is_deduped_away() {
        assert_eq!(carmen_text("Main St", Some("")), "Main St");
    }
}

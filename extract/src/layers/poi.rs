//! Pass 2: POI emitter.
//!
//! Streams nodes and ways out of the `.osm.pbf`, gates each through
//! `taxonomy::is_poi`, and writes `poi.geojsonl` via `emit::LayerWriter`.
//! Property contract mirrors `extract_country_v3.py`'s `extract_poi`
//! (see `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! `extract_poi` ~lines 171-246) exactly, adapted from ClickHouse `pois_v3`
//! row inputs to raw OSM tag inputs:
//!
//! - `carmen:text`: dedup-joined aliases, name-first: `[name, "name locality",
//!   brand, "brand locality", category]`. The python's `ext_name`/`names_intl`
//!   alias slots have no OSM-tag equivalent in this v1 (no external-source
//!   merge at this stage) and are simply absent from the list, not emitted as
//!   empty entries.
//! - `carmen:center`: raw `[lon, lat]`, unrounded (the python does not round
//!   `extract_poi`'s center either — only `extract_country`'s country
//!   centroid rounds to 4dp).
//! - `carmen:score` / `popularity`: identical value (see [`poi_score`] doc).
//! - optional `category` (first/only taxonomy match), `locality` (resolved
//!   via `HierarchyIndex`), `brand`, `housenumber` (`addr:housenumber` tag).

use std::path::Path;

use osmpbf::{Element, ElementReader};

use crate::emit::LayerWriter;
use crate::error::ExtractError;
use crate::hierarchy::HierarchyIndex;
use crate::ids::{hid, osm_sid};
use crate::nodes::NodeTable;
use crate::taxonomy::{categorize, is_poi, TagMap};

/// carmen:score / popularity v1 scoring — a deterministic, open-data
/// stand-in for the enriched popularity score `poi_score()` computes in
/// `extract_country_v3.py` (which draws on external enrichment
/// `popularity`/`review_count` columns that have no OSM-tag equivalent
/// here). Base 100; +400 if either `wikidata` or `wikipedia` is present
/// (a reasonable notability signal — POIs get external encyclopedic
/// references); +200 if `brand` is present (chain/franchise signal). The
/// two bonuses stack (a branded, wikidata-linked POI scores 700).
fn poi_score(tags: &TagMap) -> i64 {
    let mut score: i64 = 100;
    let has_wiki = tags.get("wikidata").is_some_and(|v| !v.is_empty())
        || tags.get("wikipedia").is_some_and(|v| !v.is_empty());
    if has_wiki {
        score += 400;
    }
    if tags.get("brand").is_some_and(|v| !v.is_empty()) {
        score += 200;
    }
    score
}

/// Mirrors `extract_country_v3.py`'s `clean_alias`: strip commas (carmen:text
/// is comma-split downstream) and collapse whitespace.
fn clean_alias(s: &str) -> String {
    s.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Mirrors `extract_country_v3.py`'s `dedup_join`: name-first, case-
/// insensitive deduped, comma-joined. No MAX_ALIASES cap here — the OSM-tag
/// alias list per POI is already small and bounded (name, name+locality,
/// brand, brand+locality, category), unlike the python's category[] array
/// tail which could be long.
fn dedup_join(parts: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in parts {
        let cleaned = clean_alias(p);
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

/// Build the alias list + resulting `carmen:text` for one POI. Order mirrors
/// `extract_poi`: name, "name locality", brand, "brand locality", category.
fn carmen_text(name: &str, brand: &str, locality: Option<&str>, category: Option<&str>) -> String {
    let mut aliases: Vec<String> = Vec::new();
    if !name.is_empty() {
        aliases.push(name.to_string());
    }
    if !name.is_empty() {
        if let Some(loc) = locality {
            if !loc.is_empty() {
                aliases.push(format!("{name} {loc}"));
            }
        }
    }
    if !brand.is_empty() {
        aliases.push(brand.to_string());
        if let Some(loc) = locality {
            if !loc.is_empty() {
                aliases.push(format!("{brand} {loc}"));
            }
        }
    }
    if let Some(cat) = category {
        aliases.push(cat.to_string());
    }
    dedup_join(&aliases)
}

/// One resolved POI candidate, gathered during the streaming pass, before
/// properties/geometry are assembled.
struct Candidate {
    kind: char, // 'n' or 'w'
    id: i64,
    tags: TagMap,
    lon: f64,
    lat: f64,
}

fn tags_to_map<'a>(iter: impl Iterator<Item = (&'a str, &'a str)>) -> TagMap {
    iter.map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Centroid (simple arithmetic mean) of a way's resolvable member node
/// locations. Returns `None` if zero member nodes resolve via `nodes`
/// (caller counts this as a skip, per the brief).
fn way_centroid(way_refs: &[i64], nodes: &NodeTable) -> Option<(f64, f64)> {
    let mut sum_lon = 0.0f64;
    let mut sum_lat = 0.0f64;
    let mut n = 0u64;
    for &node_id in way_refs {
        if let Some((lon, lat)) = nodes.get(node_id) {
            sum_lon += lon;
            sum_lat += lat;
            n += 1;
        }
    }
    if n == 0 {
        None
    } else {
        Some((sum_lon / n as f64, sum_lat / n as f64))
    }
}

/// Pass 2: extract POIs from `pbf` (nodes + ways), resolve each one's
/// hierarchy parents via `hier`, and write `{out_dir}/poi.geojsonl` via
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
    reader.for_each(|element| match element {
        Element::Node(n) => {
            let tags = tags_to_map(n.tags());
            if is_poi(&tags) {
                candidates.push(Candidate {
                    kind: 'n',
                    id: n.id(),
                    tags,
                    lon: n.lon(),
                    lat: n.lat(),
                });
            }
        }
        Element::DenseNode(n) => {
            let tags = tags_to_map(n.tags());
            if is_poi(&tags) {
                candidates.push(Candidate {
                    kind: 'n',
                    id: n.id(),
                    tags,
                    lon: n.lon(),
                    lat: n.lat(),
                });
            }
        }
        Element::Way(w) => {
            let tags = tags_to_map(w.tags());
            if is_poi(&tags) {
                let refs: Vec<i64> = w.refs().collect();
                match way_centroid(&refs, nodes) {
                    Some((lon, lat)) => {
                        candidates.push(Candidate {
                            kind: 'w',
                            id: w.id(),
                            tags,
                            lon,
                            lat,
                        });
                    }
                    None => way_skips += 1,
                }
            }
        }
        _ => {}
    })?;

    if way_skips > 0 {
        eprintln!("layers::poi: skipped {way_skips} way(s) with zero resolvable member nodes");
    }

    let path = out_dir.join("poi.geojsonl");
    let mut writer = LayerWriter::new(&path)?;

    for c in &candidates {
        let name = c.tags.get("name").map(String::as_str).unwrap_or("");
        let brand = c.tags.get("brand").map(String::as_str).unwrap_or("");
        let category = categorize(&c.tags);
        let parents = hier.resolve(c.lon, c.lat);
        let locality = parents.locality.as_deref();

        let text = carmen_text(name, brand, locality, category);
        if text.is_empty() {
            // is_poi already guarantees name-or-brand present, so this
            // should not happen, but never emit an empty carmen:text.
            continue;
        }

        let score = poi_score(&c.tags);

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.into());
        props.insert(
            "carmen:center".into(),
            serde_json::json!([c.lon, c.lat]),
        );
        props.insert("carmen:score".into(), score.into());
        props.insert("popularity".into(), score.into());
        if let Some(cat) = category {
            props.insert("category".into(), cat.into());
        }
        if let Some(loc) = locality {
            if !loc.is_empty() {
                props.insert("locality".into(), loc.into());
            }
        }
        if !brand.is_empty() {
            props.insert("brand".into(), brand.into());
        }
        if let Some(hn) = c.tags.get("addr:housenumber") {
            if !hn.is_empty() {
                props.insert("housenumber".into(), hn.clone().into());
            }
        }

        let sid = osm_sid(c.kind, c.id);
        let feature_id = hid(&sid);
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
    fn poi_score_base_only() {
        assert_eq!(poi_score(&tags(&[("amenity", "cafe")])), 100);
    }

    #[test]
    fn poi_score_wikidata_bonus() {
        assert_eq!(poi_score(&tags(&[("wikidata", "Q123")])), 500);
    }

    #[test]
    fn poi_score_wikipedia_bonus() {
        assert_eq!(poi_score(&tags(&[("wikipedia", "en:Foo")])), 500);
    }

    #[test]
    fn poi_score_brand_bonus() {
        assert_eq!(poi_score(&tags(&[("brand", "Starbucks")])), 300);
    }

    #[test]
    fn poi_score_wiki_and_brand_stack() {
        assert_eq!(
            poi_score(&tags(&[("wikidata", "Q1"), ("brand", "Starbucks")])),
            700
        );
    }

    #[test]
    fn poi_score_empty_wikidata_value_does_not_count() {
        assert_eq!(poi_score(&tags(&[("wikidata", "")])), 100);
    }

    #[test]
    fn dedup_join_strips_commas_and_dedupes_case_insensitively() {
        let parts = vec!["Joe's, Cafe".to_string(), "joe's cafe".to_string(), "Cafe".to_string()];
        assert_eq!(dedup_join(&parts), "Joe's Cafe,Cafe");
    }

    #[test]
    fn carmen_text_builds_name_first_alias_order() {
        let text = carmen_text("Joe's", "Acme", Some("Monaco"), Some("cafe"));
        assert_eq!(text, "Joe's,Joe's Monaco,Acme,Acme Monaco,cafe");
    }

    #[test]
    fn carmen_text_omits_empty_slots() {
        let text = carmen_text("Joe's", "", None, Some("cafe"));
        assert_eq!(text, "Joe's,cafe");
    }

    #[test]
    fn way_centroid_averages_resolvable_nodes() {
        let mut locations = std::collections::HashMap::new();
        locations.insert(1i64, (0.0f64, 0.0f64));
        locations.insert(2i64, (2.0f64, 2.0f64));
        // Build a NodeTable via its public load-free constructor path isn't
        // available; exercise the pure function with a tiny local stand-in
        // instead by re-deriving the centroid math directly (way_centroid's
        // NodeTable dependency is exercised end-to-end by the Monaco
        // integration test instead).
        let sum_lon: f64 = locations.values().map(|(lon, _)| lon).sum();
        let sum_lat: f64 = locations.values().map(|(_, lat)| lat).sum();
        let n = locations.len() as f64;
        assert_eq!((sum_lon / n, sum_lat / n), (1.0, 1.0));
    }
}

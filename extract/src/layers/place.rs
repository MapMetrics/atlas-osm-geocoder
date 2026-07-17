//! Pass 2 (no PBF re-read — consumes the already-loaded
//! `boundaries::AdminSet`): place / region / country emitters.
//!
//! Three sibling functions, each writing one layer file, mirroring
//! `extract_country_v3.py`'s `extract_place` / `extract_region` /
//! `extract_country` (see
//! `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! ~lines 450-587) exactly, adapted from ClickHouse `places_v3` row inputs
//! (pre-filtered upstream by `place_type IN (...)`) to raw OSM `PlaceNode`/
//! `AdminArea` inputs collected by `boundaries::AdminSet::load`:
//!
//! - [`extract_places`]: one `Feature` per `PlaceNode` whose `place` tag is
//!   in [`PLACE_TYPES`] (the OSM-tag equivalent of the python's
//!   `PLACE_TYPES` set, ~lines 61-66 — narrowed to the OSM `place=*` values
//!   this crate's `boundaries::PlaceNode` can actually carry, since GeoNames
//!   feature codes like `ppl`/`pplc`/`adm1` have no OSM-tag source here).
//!   Geometry: `Point` at the node's own `(lon, lat)` (python ~line 498).
//!   `carmen:score` via [`place_score_from_pop`] (verbatim port of the
//!   python's `_place_score_from_pop`, ~lines 450-454). `carmen:text` (G8:
//!   cross-language search) is `[name, intl_names...]` dedup-joined via
//!   [`dedup_join`] — `PlaceNode::intl_names` (every `name:<lang>` tag value
//!   plus `int_name` on the place node itself, sorted by tag key, capped at
//!   16 — see `boundaries::place_node_from_tags`) gives e.g. "Den Haag" the
//!   alias "The Hague" (`name:en`). This is this crate's direct equivalent
//!   of production's `names_intl` external-enrichment alias slot for
//!   places. **Follow-up**: `extract_regions`'s `AdminArea`-sourced rows
//!   (admin_level==4 boundary relations) and [`extract_countries`] do NOT
//!   get this treatment — `AdminArea` carries no tags at all (only
//!   `name`/`admin_level`/`rings`, collected from `boundary=administrative`
//!   *relations*, a different OSM object type than the `place=*` *nodes*
//!   `PlaceNode` sources from), so there is no `name:<lang>` source to read
//!   for regions/countries in this v1. `extract_regions`'s OTHER source
//!   (`PlaceNode`s with `place` in `{state, region}`) does carry
//!   `intl_names` but is left unchanged here too, out of this task's
//!   explicit scope ("place feature's aliases") — a natural next step.
//! - [`extract_regions`]: `region.geojsonl` combines TWO sources per the
//!   task brief (the python's `extract_region` only has one — a
//!   `places_v3` GeoNames-feature-code query with no OSM admin-boundary
//!   equivalent available to this crate):
//!   1. `AdminArea`s with `admin_level == 4`, emitted as a `Point` at the
//!      ring centroid (arithmetic mean of the first/outer ring's exterior
//!      coordinates — see module doc "ring centroid math" note).
//!   2. `PlaceNode`s with `place` in `{state, region}`, emitted as a `Point`
//!      at the node's own coordinates (the OSM-tag equivalent of the
//!      python's `place_type IN (REGION_TYPES)` filter, narrowed to the two
//!      OSM `place=*` values that actually mean "region/state").
//!
//!   Both sources share the python's fixed score of 180 (`extract_region`
//!   ~line 536: "regions score high in NL (~170-197); use a fixed high
//!   band").
//! - [`extract_countries`]: one `Feature` per `AdminArea` with
//!   `admin_level == 2`, geometry `Point` at the ring centroid **rounded to
//!   4 decimal places** — the python's `extract_country` rounds its
//!   avg(lon)/avg(lat) centroid to 4dp (~line 552:
//!   `round(avg(lon),4), round(avg(lat),4)`), the ONE center-rounding case
//!   anywhere in the python (every other layer's `carmen:center` is
//!   unrounded) — mirrored exactly here via [`round4`]. `carmen:text` is
//!   just the area's `name` (this crate has no `COUNTRY_ALIASES`/ISO-code
//!   alias table equivalent — the python's alias list degrades to
//!   `[name, cc]`; the OSM-tag pipeline has no `cc` parameter at extract
//!   time, so it degrades further to just `name` itself, cleaned via
//!   [`clean_text`]). No `iso_3166_1`/`bbox` properties
//!   are emitted (both require external inputs — a country-code parameter
//!   and a full-extract POI bbox scan — outside this function's inputs).

use std::path::Path;

use geo::CoordsIter;

use crate::boundaries::AdminSet;
use crate::emit::LayerWriter;
use crate::error::ExtractError;
use crate::ids::hid;

/// Populated-place `place=*` values -> place.geojsonl. OSM-tag-source subset
/// of the python's `PLACE_TYPES` (extract_country_v3.py ~lines 61-66),
/// narrowed to values `boundaries::place_node_from_tags` can actually
/// collect from raw `place=*` node tags.
const PLACE_TYPES: &[&str] = &[
    "city",
    "town",
    "village",
    "hamlet",
    "suburb",
    "quarter",
    "neighbourhood",
];

/// `place=*` values that mean "region/state" -> region.geojsonl place-node
/// source. OSM-tag-source subset of the python's `REGION_TYPES`
/// (extract_country_v3.py ~lines 68-71).
const REGION_PLACE_TYPES: &[&str] = &["state", "region"];

/// Fixed region carmen:score, verbatim port of the python's
/// `extract_region` (~line 536: "regions score high in NL (~170-197); use a
/// fixed high band").
const REGION_SCORE: i64 = 180;

/// carmen:score from population — verbatim port of the python's
/// `_place_score_from_pop` (extract_country_v3.py ~lines 450-454): a
/// log-ish mapping into a ~0..250 band, zero for missing/zero population.
fn place_score_from_pop(pop: u64) -> i64 {
    if pop == 0 {
        return 0;
    }
    let score = 20.0 * (pop as f64 + 1.0).log10();
    score.round().min(250.0) as i64
}

/// Round to 4 decimal places, matching ClickHouse's `round(x, 4)` used by
/// the python's `extract_country` (~line 552) for its centroid — the ONE
/// center-rounding case anywhere in the python's layer emitters.
fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Mirrors `extract_country_v3.py`'s `clean_alias`: strip commas
/// (carmen:text is comma-split downstream) and collapse whitespace. Returns
/// `""` for a name that is empty/all whitespace/all-commas, so callers can
/// gate on emptiness uniformly. Used directly by `extract_regions`/
/// `extract_countries` (whose `carmen:text` is always just the bare name —
/// see module doc) and as the per-alias cleaner inside [`dedup_join`] below
/// (G8: `extract_places`'s multi-alias `carmen:text`).
fn clean_text(name: &str) -> String {
    name.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// G8 (cross-language search): name-first, case-insensitive deduped,
/// comma-joined alias list — mirrors `layers::poi`'s `dedup_join`. Only
/// [`extract_places`] needs this multi-alias form (`[name, intl_names...]`);
/// `extract_regions`/`extract_countries` stay on the single-name
/// [`clean_text`] (see module doc's "Follow-up" note on why regions/
/// countries don't get `intl_names` in this v1).
fn dedup_join(parts: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in parts {
        let cleaned = clean_text(p);
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

/// Ring centroid: arithmetic mean of the polygon's first ring's exterior
/// coordinates (documented v1 approximation — see module doc "ring centroid
/// math" note in the task brief: mean-of-exterior-ring-coords is fine for
/// v1, as opposed to a true area-weighted polygon centroid). Returns `None`
/// for an area with zero rings or an empty exterior ring.
fn ring_centroid(area: &crate::boundaries::AdminArea) -> Option<(f64, f64)> {
    let ring = area.rings.first()?;
    let mut sum_lon = 0.0f64;
    let mut sum_lat = 0.0f64;
    let mut n = 0u64;
    for coord in ring.exterior_coords_iter() {
        sum_lon += coord.x;
        sum_lat += coord.y;
        n += 1;
    }
    if n == 0 {
        None
    } else {
        Some((sum_lon / n as f64, sum_lat / n as f64))
    }
}

/// `place.geojsonl`: one `Feature` per qualifying `PlaceNode` (see
/// [`PLACE_TYPES`]). `carmen:text` = `[name, intl_names...]` dedup-joined
/// (G8: cross-language search — see module doc and [`dedup_join`]).
/// Geometry: `Point` at the node's own coordinates. Returns the number of
/// features written.
pub fn extract_places(
    admin: &AdminSet,
    out_dir: &Path,
) -> Result<u64, ExtractError> {
    let path = out_dir.join("place.geojsonl");
    let mut writer = LayerWriter::new(&path)?;

    for p in admin.place_nodes() {
        if !PLACE_TYPES.contains(&p.place.as_str()) {
            continue;
        }
        if p.name.is_empty() {
            continue;
        }

        let mut aliases: Vec<String> = Vec::with_capacity(1 + p.intl_names.len());
        aliases.push(p.name.clone());
        aliases.extend(p.intl_names.iter().cloned());
        let text = dedup_join(&aliases);
        if text.is_empty() {
            continue;
        }

        let score = place_score_from_pop(p.population);

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.into());
        props.insert("carmen:center".into(), serde_json::json!([p.lon, p.lat]));
        props.insert("carmen:score".into(), score.into());
        if p.population > 0 {
            props.insert("population".into(), p.population.into());
        }

        let feature_id = hid(&format!("n{}", p.id));
        let geometry = serde_json::json!({
            "type": "Point",
            "coordinates": [p.lon, p.lat],
        });
        writer.feature(feature_id, &props, geometry)?;
    }

    Ok(writer.count())
}

/// `region.geojsonl`: `AdminArea`s with `admin_level == 4` (ring centroid)
/// plus `PlaceNode`s with `place` in [`REGION_PLACE_TYPES`] (own
/// coordinates) — see module doc. Returns the number of features written.
pub fn extract_regions(admin: &AdminSet, out_dir: &Path) -> Result<u64, ExtractError> {
    let path = out_dir.join("region.geojsonl");
    let mut writer = LayerWriter::new(&path)?;

    for area in admin.areas() {
        if area.admin_level != 4 || area.name.is_empty() {
            continue;
        }
        let Some((lon, lat)) = ring_centroid(area) else {
            continue;
        };
        let text = clean_text(&area.name);
        if text.is_empty() {
            continue;
        }

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.into());
        props.insert("carmen:center".into(), serde_json::json!([lon, lat]));
        props.insert("carmen:score".into(), REGION_SCORE.into());

        let feature_id = hid(&format!("region-area|{}", area.name));
        let geometry = serde_json::json!({
            "type": "Point",
            "coordinates": [lon, lat],
        });
        writer.feature(feature_id, &props, geometry)?;
    }

    for p in admin.place_nodes() {
        if !REGION_PLACE_TYPES.contains(&p.place.as_str()) || p.name.is_empty() {
            continue;
        }
        let text = clean_text(&p.name);
        if text.is_empty() {
            continue;
        }

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.into());
        props.insert("carmen:center".into(), serde_json::json!([p.lon, p.lat]));
        props.insert("carmen:score".into(), REGION_SCORE.into());

        let feature_id = hid(&format!("n{}", p.id));
        let geometry = serde_json::json!({
            "type": "Point",
            "coordinates": [p.lon, p.lat],
        });
        writer.feature(feature_id, &props, geometry)?;
    }

    Ok(writer.count())
}

/// `country.geojsonl`: one `Feature` per `AdminArea` with `admin_level ==
/// 2`, geometry `Point` at the ring centroid rounded to 4dp (see module
/// doc). Returns the number of features written.
pub fn extract_countries(admin: &AdminSet, out_dir: &Path) -> Result<u64, ExtractError> {
    let path = out_dir.join("country.geojsonl");
    let mut writer = LayerWriter::new(&path)?;

    for area in admin.areas() {
        if area.admin_level != 2 || area.name.is_empty() {
            continue;
        }
        let Some((lon, lat)) = ring_centroid(area) else {
            continue;
        };
        let (lon, lat) = (round4(lon), round4(lat));

        let text = clean_text(&area.name);
        if text.is_empty() {
            continue;
        }

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.into());
        props.insert("carmen:center".into(), serde_json::json!([lon, lat]));

        let feature_id = hid(&format!("country-area|{}", area.name));
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
    use crate::boundaries::{AdminArea, PlaceNode};

    fn square(name: &str, level: u8, x0: f64, y0: f64, x1: f64, y1: f64) -> AdminArea {
        let ring = geo::Polygon::new(
            geo::LineString::from(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]),
            vec![],
        );
        AdminArea {
            name: name.into(),
            admin_level: level,
            rings: vec![ring],
        }
    }

    #[test]
    fn place_score_from_pop_matches_python_formula() {
        assert_eq!(place_score_from_pop(0), 0);
        // 20 * log10(38300+1) ≈ 91.66 -> 92
        assert_eq!(place_score_from_pop(38_300), 92);
    }

    #[test]
    fn place_score_from_pop_caps_at_250() {
        // An enormous population must still clamp to 250.
        assert_eq!(place_score_from_pop(10_000_000_000_000), 250);
    }

    #[test]
    fn round4_matches_clickhouse_round() {
        assert_eq!(round4(7.42469999), 7.4247);
        assert_eq!(round4(43.73939999), 43.7394);
    }

    #[test]
    fn ring_centroid_is_mean_of_exterior_coords() {
        // A unit square's exterior ring (with closing point repeated) has
        // 5 coords: (0,0),(1,0),(1,1),(0,1),(0,0) -> mean (0.4, 0.4).
        let area = square("Test", 4, 0.0, 0.0, 1.0, 1.0);
        let (lon, lat) = ring_centroid(&area).unwrap();
        assert!((lon - 0.4).abs() < 1e-9, "lon={lon}");
        assert!((lat - 0.4).abs() < 1e-9, "lat={lat}");
    }

    #[test]
    fn ring_centroid_none_for_area_with_no_rings() {
        let area = AdminArea { name: "Empty".into(), admin_level: 4, rings: vec![] };
        assert!(ring_centroid(&area).is_none());
    }

    // --- G8: dedup_join (place multi-alias carmen:text) ---

    #[test]
    fn dedup_join_places_name_first_then_intl_names() {
        let parts = vec!["Den Haag".to_string(), "The Hague".to_string()];
        assert_eq!(dedup_join(&parts), "Den Haag,The Hague");
    }

    #[test]
    fn dedup_join_places_dedupes_case_insensitively() {
        let parts = vec!["Monaco".to_string(), "monaco".to_string(), "Monaco City".to_string()];
        assert_eq!(dedup_join(&parts), "Monaco,Monaco City");
    }

    #[test]
    fn dedup_join_places_skips_empty_entries() {
        let parts = vec!["Name".to_string(), "".to_string(), "Alias".to_string()];
        assert_eq!(dedup_join(&parts), "Name,Alias");
    }

    /// G8 acceptance case from the task brief: "Den Haag" gains "The Hague"
    /// via `intl_names` (`name:en` on the place node).
    #[test]
    fn extract_places_appends_intl_names_to_carmen_text() {
        let dir = std::env::temp_dir().join("ae_place_intl_names_unit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let admin = AdminSet::for_test(
            vec![],
            vec![PlaceNode {
                name: "Den Haag".into(),
                place: "city".into(),
                population: 545_838,
                lon: 4.3007,
                lat: 52.0705,
                id: 1,
                intl_names: vec!["The Hague".into()],
            }],
        );
        let count = extract_places(&admin, &dir).unwrap();
        assert_eq!(count, 1);

        let contents = std::fs::read_to_string(dir.join("place.geojsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(v["properties"]["carmen:text"], "Den Haag,The Hague");
    }

    #[test]
    fn extract_places_carmen_text_is_just_name_when_no_intl_names() {
        let dir = std::env::temp_dir().join("ae_place_no_intl_names_unit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let admin = AdminSet::for_test(
            vec![],
            vec![PlaceNode {
                name: "Plainville".into(),
                place: "village".into(),
                population: 0,
                lon: 0.0,
                lat: 0.0,
                id: 1,
                intl_names: vec![],
            }],
        );
        extract_places(&admin, &dir).unwrap();

        let contents = std::fs::read_to_string(dir.join("place.geojsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(v["properties"]["carmen:text"], "Plainville");
    }

    #[test]
    fn extract_places_filters_by_place_type() {
        let dir = std::env::temp_dir().join("ae_place_unit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let admin = AdminSet::for_test(
            vec![],
            vec![
                PlaceNode {
                    name: "Townsville".into(),
                    place: "town".into(),
                    population: 1000,
                    lon: 1.0,
                    lat: 2.0,
                    id: 1,
                    intl_names: vec![],
                },
                PlaceNode {
                    name: "NotAPlaceType".into(),
                    place: "suburb_but_typo".into(),
                    population: 0,
                    lon: 3.0,
                    lat: 4.0,
                    id: 2,
                    intl_names: vec![],
                },
            ],
        );
        let count = extract_places(&admin, &dir).unwrap();
        assert_eq!(count, 1, "only the qualifying place_type should be emitted");
    }

    #[test]
    fn extract_regions_combines_admin_level_4_and_place_nodes() {
        let dir = std::env::temp_dir().join("ae_region_unit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let admin = AdminSet::for_test(
            vec![square("RegionArea", 4, 0.0, 0.0, 1.0, 1.0), square("NotRegion", 8, 0.0, 0.0, 1.0, 1.0)],
            vec![
                PlaceNode {
                    name: "StateNode".into(),
                    place: "state".into(),
                    population: 0,
                    lon: 5.0,
                    lat: 5.0,
                    id: 1,
                    intl_names: vec![],
                },
                PlaceNode {
                    name: "NotRegionPlace".into(),
                    place: "city".into(),
                    population: 0,
                    lon: 6.0,
                    lat: 6.0,
                    id: 2,
                    intl_names: vec![],
                },
            ],
        );
        let count = extract_regions(&admin, &dir).unwrap();
        assert_eq!(count, 2, "one admin_level=4 area + one region place node");
    }

    #[test]
    fn extract_countries_only_admin_level_2() {
        let dir = std::env::temp_dir().join("ae_country_unit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let admin = AdminSet::for_test(
            vec![square("Countryland", 2, 0.0, 0.0, 1.0, 1.0), square("NotCountry", 4, 0.0, 0.0, 1.0, 1.0)],
            vec![],
        );
        let count = extract_countries(&admin, &dir).unwrap();
        assert_eq!(count, 1);

        let contents = std::fs::read_to_string(dir.join("country.geojsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(v["properties"]["carmen:text"], "Countryland");
    }
}

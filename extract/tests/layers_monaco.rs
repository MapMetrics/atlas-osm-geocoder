//! Integration tests for `layers::street` and `layers::place` against the
//! Monaco fixture. Pins the geometry/property conventions of
//! `extract_country_v3.py`'s `extract_street`/`extract_place`/
//! `extract_region`/`extract_country` (see
//! `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! ~lines 316-587), adapted from ClickHouse `*_v3` row inputs to raw OSM
//! tag inputs. See `layers::street`/`layers::place` module doc comments for
//! the full per-convention pin references.

use std::collections::HashMap;
use std::fs;

use atlas_extract::boundaries::AdminSet;
use atlas_extract::hierarchy::HierarchyIndex;
use atlas_extract::layers::{place, street};
use atlas_extract::nodes::NodeTable;

const MONACO: &str = "tests/fixtures/monaco.osm.pbf";

fn read_lines(path: &std::path::Path) -> Vec<String> {
    let contents = fs::read_to_string(path).unwrap();
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn monaco_street_extraction_meets_contract() {
    let out_dir = std::env::temp_dir().join("ae_street_monaco_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load(MONACO.as_ref(), &nodes).unwrap();
    let hier = HierarchyIndex::build(&admin);

    let count = street::extract(MONACO.as_ref(), &nodes, &hier, &out_dir).unwrap();

    let path = out_dir.join("street.geojsonl");
    let lines = read_lines(&path);
    assert_eq!(lines.len() as u64, count, "line count must match returned count");

    // Monaco's dense named-highway network (motorway..residential,
    // pedestrian, living_street, unclassified, plus named service roads)
    // must yield at least 20 distinct named streets.
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_ids: HashMap<u64, ()> = HashMap::new();

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("failed to parse line as JSON: {e}\nline: {line}"));

        assert_eq!(v["type"], "Feature");

        let id = v["id"].as_u64().expect("id must be a u64");
        assert!(id < (1u64 << 56), "id {id} must be < 2^56");
        assert!(seen_ids.insert(id, ()).is_none(), "duplicate feature id {id}");

        // Street geometry: Point at the way's centroid — the python's
        // extract_street (extract_country_v3.py ~line 444) emits ONE
        // Feature per streets_v3 row (already one row per named way) as a
        // Point, NOT a grouped MultiPoint. No (name, locality) grouping
        // happens in extract_street itself (see layers::street module doc
        // for the full pin — this corrects the brief's grouped-MultiPoint
        // sketch, which the python's actual body contradicts).
        assert_eq!(v["geometry"]["type"], "Point", "geometry must be Point (python extract_street ~line 444)");
        let coords = v["geometry"]["coordinates"].as_array().expect("coordinates must be an array");
        assert_eq!(coords.len(), 2, "Point coordinates must be [lon,lat]");

        let text = v["properties"]["carmen:text"].as_str().expect("carmen:text must be a string");
        assert!(!text.is_empty(), "carmen:text must be non-empty");
        // carmen:text = "name,name locality" (python extract_street, ~line 420):
        // first alias segment before the first comma is always the bare name.
        let name = text.split(',').next().unwrap();
        assert!(!name.is_empty());
        seen_names.insert(name.to_string());

        // carmen:score present (STREET_SCORE lookup, python ~line 428).
        assert!(v["properties"]["carmen:score"].is_number(), "carmen:score must be present");
    }

    assert!(
        lines.len() > 900,
        "expected > 900 named highways in Monaco fixture (fixture has 1051 ways with highway+name, ~960 with resolvable centroid), got {}",
        lines.len()
    );
    assert!(
        seen_names.len() >= 20,
        "expected >= 20 distinct named streets in Monaco, got {}",
        seen_names.len()
    );
}

#[test]
fn monaco_place_extraction_contains_monte_carlo() {
    let out_dir = std::env::temp_dir().join("ae_place_monaco_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load(MONACO.as_ref(), &nodes).unwrap();

    let count = place::extract_places(&admin, &out_dir).unwrap();
    assert!(count > 0, "expected > 0 place features, got {count}");

    let path = out_dir.join("place.geojsonl");
    let lines = read_lines(&path);
    assert_eq!(lines.len() as u64, count, "line count must match returned count");

    let mut found_monte_carlo = false;
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("failed to parse line as JSON: {e}\nline: {line}"));

        assert_eq!(v["type"], "Feature");
        assert_eq!(v["geometry"]["type"], "Point", "place geometry must be Point");

        let text = v["properties"]["carmen:text"].as_str().expect("carmen:text must be a string");
        assert!(!text.is_empty());
        assert!(v["properties"]["carmen:score"].is_number(), "carmen:score must be present");

        if text.split(',').next() == Some("Monte-Carlo") {
            found_monte_carlo = true;
            // Monte-Carlo (place=suburb, population=15507 in the fixture)
            // must carry a population-derived score > 0.
            assert!(
                v["properties"]["population"].as_u64().unwrap_or(0) > 0,
                "Monte-Carlo must carry its population"
            );
            let score = v["properties"]["carmen:score"].as_i64().unwrap();
            assert!(score > 0, "Monte-Carlo (populated) must have carmen:score > 0, got {score}");
        }
    }

    assert!(found_monte_carlo, "expected to find Monte-Carlo among the extracted places");
}

/// G8 (cross-language search) pin: the Monaco fixture's "Monaco-Ville"
/// suburb node carries `name:en=Monaco City` — a genuinely different
/// English name from the primary French `name` tag (distinct from
/// "Monte-Carlo", which the fixture only tags with da/et/mk/ru/tr variants,
/// none of them English) — so it's the clearer pin for "place feature
/// gains a name:<lang> alias" (e.g. "Den Haag" gains "The Hague").
#[test]
fn monaco_place_extraction_monaco_ville_gains_name_en_alias() {
    let out_dir = std::env::temp_dir().join("ae_place_monaco_intl_names_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load(MONACO.as_ref(), &nodes).unwrap();

    place::extract_places(&admin, &out_dir).unwrap();

    let path = out_dir.join("place.geojsonl");
    let lines = read_lines(&path);

    let mut found_monaco_ville = false;
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let text = v["properties"]["carmen:text"].as_str().unwrap();
        if text.split(',').next() == Some("Monaco-Ville") {
            found_monaco_ville = true;
            assert!(
                text.contains("Monaco City"),
                "Monaco-Ville's carmen:text must include its name:en alias \"Monaco City\", got: {text}"
            );
        }
    }

    assert!(found_monaco_ville, "expected to find Monaco-Ville among the extracted places");
}

#[test]
fn monaco_country_extraction_contains_exactly_monaco() {
    let out_dir = std::env::temp_dir().join("ae_country_monaco_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load(MONACO.as_ref(), &nodes).unwrap();

    let count = place::extract_countries(&admin, &out_dir).unwrap();
    // Exactly one country-level (admin_level=2) area in the fixture is named
    // "Monaco" itself (the admin_level=2 "France - Mùnegu" territorial-water
    // rows are a distinct, differently-named area) -> exactly one emitted
    // country row, matching python's `extract_country`'s single synthetic
    // row per invocation, adapted here to "one row per admin_level=2 area".
    assert_eq!(count, 1, "expected exactly one country feature, got {count}");

    let path = out_dir.join("country.geojsonl");
    let lines = read_lines(&path);
    assert_eq!(lines.len(), 1);

    let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(v["type"], "Feature");
    assert_eq!(v["geometry"]["type"], "Point");

    let text = v["properties"]["carmen:text"].as_str().unwrap();
    assert_eq!(text, "Monaco", "country carmen:text must be exactly \"Monaco\"");

    // Country centroid rounds to 4dp (python extract_country ~line 552:
    // `round(avg(lon),4)` / `round(avg(lat),4)`).
    let center = v["properties"]["carmen:center"].as_array().unwrap();
    let lon = center[0].as_f64().unwrap();
    let lat = center[1].as_f64().unwrap();
    assert_eq!(lon, round4(lon), "carmen:center lon must be rounded to 4dp");
    assert_eq!(lat, round4(lat), "carmen:center lat must be rounded to 4dp");
    // Sanity: centroid is inside Monaco's bbox.
    assert!((7.3..7.6).contains(&lon), "lon out of range: {lon}");
    assert!((43.6..43.9).contains(&lat), "lat out of range: {lat}");
}

#[test]
fn monaco_region_extraction_runs_without_admin_level_4() {
    // Monaco (a microstate) has no admin_level=4 areas and no
    // place=state/region nodes in the fixture: region.geojsonl is
    // legitimately empty. This pins that "zero regions" is a valid,
    // non-error outcome (not every country has a region tier) rather than
    // asserting a specific count.
    let out_dir = std::env::temp_dir().join("ae_region_monaco_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load(MONACO.as_ref(), &nodes).unwrap();

    let count = place::extract_regions(&admin, &out_dir).unwrap();
    let path = out_dir.join("region.geojsonl");
    let lines = read_lines(&path);
    assert_eq!(lines.len() as u64, count);

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["type"], "Feature");
        assert_eq!(v["geometry"]["type"], "Point");
        assert!(v["properties"]["carmen:text"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(v["properties"]["carmen:score"].is_number());
    }
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

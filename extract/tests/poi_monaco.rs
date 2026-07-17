use std::fs;

use atlas_extract::boundaries::AdminSet;
use atlas_extract::hierarchy::HierarchyIndex;
use atlas_extract::layers::poi;
use atlas_extract::nodes::NodeTable;

const MONACO: &str = "tests/fixtures/monaco.osm.pbf";

#[test]
fn monaco_poi_extraction_meets_contract() {
    let out_dir = std::env::temp_dir().join("ae_poi_monaco_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load(MONACO.as_ref(), &nodes).unwrap();
    let hier = HierarchyIndex::build(&admin);

    let count = poi::extract(MONACO.as_ref(), &nodes, &hier, &out_dir).unwrap();

    assert!(count > 300, "expected > 300 POIs, got {count}");

    let path = out_dir.join("poi.geojsonl");
    let contents = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len() as u64, count, "line count must match returned count");

    let mut found_casino = false;

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("failed to parse line as JSON: {e}\nline: {line}"));

        assert_eq!(v["type"], "Feature");

        let id = v["id"].as_u64().expect("id must be a u64");
        assert!(id < (1u64 << 56), "id {id} must be < 2^56");

        let text = v["properties"]["carmen:text"]
            .as_str()
            .expect("carmen:text must be a string");
        assert!(!text.is_empty(), "carmen:text must be non-empty");

        let center = v["properties"]["carmen:center"]
            .as_array()
            .expect("carmen:center must be an array");
        assert_eq!(center.len(), 2, "carmen:center must be [lon,lat]");

        let name = v["properties"]["carmen:text"].as_str().unwrap();
        if name.split(',').next() == Some("Casino de Monte Carlo") {
            found_casino = true;
            assert_eq!(
                v["properties"]["category"], "casino",
                "Casino de Monte Carlo must be categorized as casino"
            );
            assert!(
                v["properties"]["locality"].is_string(),
                "Casino de Monte Carlo must have a resolved locality"
            );

            // G8 (cross-language search) pin: the Monaco fixture's Casino
            // de Monte Carlo carries `name:en=Monte-Carlo Casino and Opera
            // House` — a genuinely different English name, not just a
            // transliteration of the French `name` tag — plus several other
            // `name:<lang>` tags (cs/de/es/it/ko/pt/zh). carmen:text must
            // surface at least the English alias so a "Monte Carlo Casino"
            // search finds this POI even though its primary OSM `name` is
            // French.
            assert!(
                text.contains("Monte-Carlo Casino and Opera House"),
                "Casino de Monte Carlo's carmen:text must include its name:en alias, got: {text}"
            );
        }
    }

    assert!(found_casino, "expected to find Casino de Monte Carlo among the extracted POIs");
}

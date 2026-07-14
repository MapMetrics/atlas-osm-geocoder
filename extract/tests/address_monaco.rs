use std::collections::HashMap;
use std::fs;

use atlas_extract::boundaries::AdminSet;
use atlas_extract::hierarchy::HierarchyIndex;
use atlas_extract::layers::address;
use atlas_extract::nodes::NodeTable;

const MONACO: &str = "tests/fixtures/monaco.osm.pbf";

#[test]
fn monaco_address_extraction_meets_contract() {
    let out_dir = std::env::temp_dir().join("ae_address_monaco_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load(MONACO.as_ref(), &nodes).unwrap();
    let hier = HierarchyIndex::build(&admin);

    let count = address::extract(MONACO.as_ref(), &nodes, &hier, &out_dir).unwrap();

    assert!(count >= 1, "expected >= 1 address group feature, got {count}");

    let path = out_dir.join("address.geojsonl");
    let contents = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len() as u64, count, "line count must match returned count");

    // Every feature id must be unique across the whole output.
    let mut seen_ids: HashMap<u64, ()> = HashMap::new();
    let mut has_postcode = false;

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("failed to parse line as JSON: {e}\nline: {line}"));

        assert_eq!(v["type"], "Feature");

        let id = v["id"].as_u64().expect("id must be a u64");
        assert!(id < (1u64 << 56), "id {id} must be < 2^56");
        assert!(
            seen_ids.insert(id, ()).is_none(),
            "duplicate feature id {id}"
        );

        assert_eq!(
            v["geometry"]["type"], "MultiPoint",
            "geometry must be MultiPoint"
        );
        let coords = v["geometry"]["coordinates"]
            .as_array()
            .expect("coordinates must be an array");

        let numbers = v["properties"]["carmen:addressnumber"]
            .as_array()
            .expect("carmen:addressnumber must be an array");

        assert_eq!(
            numbers.len(),
            coords.len(),
            "carmen:addressnumber length must exactly match MultiPoint coordinate count"
        );
        assert!(
            coords.len() <= 2000,
            "group segment must not exceed 2000 members, got {}",
            coords.len()
        );
        assert!(!coords.is_empty(), "segment must have at least one member");

        for c in coords {
            let pair = c.as_array().expect("each coordinate must be [lon,lat]");
            assert_eq!(pair.len(), 2);
        }

        let text = v["properties"]["carmen:text"]
            .as_str()
            .expect("carmen:text must be a string");
        assert!(!text.is_empty(), "carmen:text must be non-empty");

        let center = v["properties"]["carmen:center"]
            .as_array()
            .expect("carmen:center must be an array");
        assert_eq!(center.len(), 2, "carmen:center must be [lon,lat]");

        // Check if this feature has a postcode property.
        if v["properties"].get("postcode").is_some() {
            has_postcode = true;
        }
    }

    assert!(
        has_postcode,
        "at least one emitted address feature must contain a postcode property"
    );
}

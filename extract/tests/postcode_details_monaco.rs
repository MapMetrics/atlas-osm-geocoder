//! Integration tests for `layers::postcode` and `layers::details` against
//! the Monaco fixture.
//!
//! `postcode::extract` mirrors `extract_country_v3.py`'s `extract_postcode`
//! (see `scripts/extract_country_v3.py`,
//! `extract_postcode` ~lines 590-617) exactly, adapted from ClickHouse
//! `postcodes_v3` row inputs (one row per distinct postcode already grouped
//! server-side) to raw OSM `addr:postcode` tag inputs (grouped client-side
//! here, one Feature per distinct code at the MEAN coordinate of its
//! members).
//!
//! `details::extract` writes `poi_details.jsonl` — the /details sidecar
//! source (2026-07-14-details-sidecar-design.md),
//! adapted from `extract_poi_details`'s ClickHouse `pois_v3` columns to raw
//! OSM tags. See `layers::details` module doc for the full per-field pin.

use std::collections::HashSet;
use std::fs;

use atlas_extract::boundaries::AdminSet;
use atlas_extract::hierarchy::HierarchyIndex;
use atlas_extract::layers::{details, poi, postcode};
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
fn monaco_postcode_extraction_contains_98000() {
    let out_dir = std::env::temp_dir().join("ae_postcode_monaco_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();

    let count = postcode::extract(MONACO.as_ref(), &nodes, &out_dir).unwrap();
    assert!(count > 0, "expected > 0 postcode features, got {count}");

    let path = out_dir.join("postcode.geojsonl");
    let lines = read_lines(&path);
    assert_eq!(lines.len() as u64, count, "line count must match returned count");

    let mut seen_ids: HashSet<u64> = HashSet::new();
    let mut found_98000 = false;

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("failed to parse line as JSON: {e}\nline: {line}"));

        assert_eq!(v["type"], "Feature");

        let id = v["id"].as_u64().expect("id must be a u64");
        assert!(id < (1u64 << 56), "id {id} must be < 2^56");
        assert!(seen_ids.insert(id), "duplicate feature id {id}");

        assert_eq!(v["geometry"]["type"], "Point", "postcode geometry must be Point");
        let coords = v["geometry"]["coordinates"].as_array().expect("coordinates must be an array");
        assert_eq!(coords.len(), 2, "Point coordinates must be [lon,lat]");

        let text = v["properties"]["carmen:text"].as_str().expect("carmen:text must be a string");
        assert!(!text.is_empty(), "carmen:text must be non-empty");

        let center = v["properties"]["carmen:center"].as_array().expect("carmen:center must be an array");
        assert_eq!(center.len(), 2, "carmen:center must be [lon,lat]");

        if text == "98000" {
            found_98000 = true;
        }
    }

    assert!(found_98000, "expected postcode.geojsonl to contain '98000' among its carmen:text values");
}

#[test]
fn monaco_postcode_ids_are_distinct_per_code() {
    // Two distinct codes must not collide, and re-running must be
    // deterministic (same code -> same id every time).
    let out_dir = std::env::temp_dir().join("ae_postcode_monaco_distinct_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    postcode::extract(MONACO.as_ref(), &nodes, &out_dir).unwrap();

    let path = out_dir.join("postcode.geojsonl");
    let lines = read_lines(&path);

    let mut texts: HashSet<String> = HashSet::new();
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let text = v["properties"]["carmen:text"].as_str().unwrap().to_string();
        assert!(texts.insert(text.clone()), "duplicate carmen:text (postcode) {text} — one feature per distinct code expected");
    }
    assert!(texts.len() >= 2, "expected at least 2 distinct postcodes in Monaco fixture, got {}", texts.len());
}

#[test]
fn monaco_details_ids_are_subset_of_poi_ids() {
    let poi_out = std::env::temp_dir().join("ae_details_monaco_poi_test");
    let details_out = std::env::temp_dir().join("ae_details_monaco_test");
    fs::create_dir_all(&poi_out).unwrap();
    fs::create_dir_all(&details_out).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load(MONACO.as_ref(), &nodes).unwrap();
    let hier = HierarchyIndex::build(&admin);

    poi::extract(MONACO.as_ref(), &nodes, &hier, &poi_out).unwrap();
    let poi_lines = read_lines(&poi_out.join("poi.geojsonl"));
    let poi_ids: HashSet<u64> = poi_lines
        .iter()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["id"].as_u64().unwrap()
        })
        .collect();

    let count = details::extract(MONACO.as_ref(), &nodes, &details_out).unwrap();
    assert!(count > 0, "expected > 0 details records in Monaco fixture, got {count}");

    let details_path = details_out.join("poi_details.jsonl");
    let details_lines = read_lines(&details_path);
    assert_eq!(details_lines.len() as u64, count, "line count must match returned count");

    let mut seen_ids: HashSet<u64> = HashSet::new();
    for line in &details_lines {
        // details lines are bare JSON objects, NOT GeoJSON Features.
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("failed to parse line as JSON: {e}\nline: {line}"));
        assert!(v.get("type").is_none(), "details lines must not be GeoJSON Features");

        let id = v["id"].as_u64().expect("id must be a u64");
        assert!(seen_ids.insert(id), "duplicate details id {id}");
        assert!(
            poi_ids.contains(&id),
            "details id {id} not found among poi.geojsonl ids — details ids must be a subset"
        );

        // NO rating/price/review keys, ever (open product) — check both the
        // JSON structure and the raw line text as a belt-and-suspenders
        // substring check per the brief.
        assert!(v.get("rating").is_none(), "details record must never carry a rating key");
        assert!(v.get("price").is_none(), "details record must never carry a price key");
        assert!(v.get("reviews").is_none(), "details record must never carry a reviews key");
        assert!(v.get("review_count").is_none(), "details record must never carry a review_count key");
    }
}

#[test]
fn monaco_details_lines_never_contain_forbidden_substrings() {
    let out_dir = std::env::temp_dir().join("ae_details_monaco_substrings_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    details::extract(MONACO.as_ref(), &nodes, &out_dir).unwrap();

    let path = out_dir.join("poi_details.jsonl");
    let lines = read_lines(&path);
    assert!(!lines.is_empty(), "expected at least one details line in Monaco fixture");

    for line in &lines {
        assert!(!line.contains("rating"), "details line must never contain the substring 'rating': {line}");
        assert!(!line.contains("price"), "details line must never contain the substring 'price': {line}");
        assert!(!line.contains("review"), "details line must never contain the substring 'review': {line}");
    }
}

#[test]
fn monaco_details_have_expected_fields() {
    let out_dir = std::env::temp_dir().join("ae_details_monaco_fields_test");
    fs::create_dir_all(&out_dir).unwrap();

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    details::extract(MONACO.as_ref(), &nodes, &out_dir).unwrap();

    let path = out_dir.join("poi_details.jsonl");
    let lines = read_lines(&path);

    let mut found_hours = false;
    let mut found_phone = false;
    let mut found_website = false;
    let mut found_email = false;
    let mut found_socials = false;
    let mut found_address = false;

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v["id"].is_u64(), "every details line must carry a u64 id");

        // Every non-id field must be non-empty when present (the extractor
        // must omit empty fields, not emit empty strings/arrays/objects).
        if let Some(h) = v.get("hours") {
            assert!(!h.as_str().unwrap().is_empty());
            found_hours = true;
        }
        if let Some(p) = v.get("phone") {
            let arr = p.as_array().unwrap();
            assert!(!arr.is_empty());
            found_phone = true;
        }
        if let Some(w) = v.get("website") {
            assert!(!w.as_str().unwrap().is_empty());
            found_website = true;
        }
        if let Some(e) = v.get("email") {
            assert!(!e.as_str().unwrap().is_empty());
            found_email = true;
        }
        if let Some(s) = v.get("socials") {
            assert!(!s.as_object().unwrap().is_empty());
            found_socials = true;
        }
        if let Some(a) = v.get("address") {
            assert!(!a.as_str().unwrap().is_empty());
            found_address = true;
        }

        // At least one detail field (beyond id/name) must be present — the
        // non-empty gate from the brief.
        let has_detail = v.get("hours").is_some()
            || v.get("phone").is_some()
            || v.get("website").is_some()
            || v.get("email").is_some()
            || v.get("socials").is_some()
            || v.get("address").is_some();
        assert!(has_detail, "details line has no non-empty detail field: {line}");
    }

    assert!(found_hours, "expected at least one details line with opening hours in Monaco fixture");
    assert!(found_phone, "expected at least one details line with a phone number in Monaco fixture");
    assert!(found_website, "expected at least one details line with a website in Monaco fixture");
    assert!(found_email, "expected at least one details line with an email in Monaco fixture");
    // Socials/address are less certain to appear in the fixture; check but
    // don't hard-require to keep the test robust to fixture drift.
    let _ = (found_socials, found_address);
}

/// Converter details smoke: if the Rust converter dev binary is available
/// (env `CONVERT_BIN`, else repo-relative fallback path), run
/// `--emit-details --src <outdir> --dst <tmp>` against our own output and
/// assert it exits 0 — i.e. our poi_details.jsonl actually satisfies the
/// converter's DetailsIn schema end-to-end, not just our own test
/// assumptions about it.
#[test]
fn monaco_details_satisfy_converter_emit_details_smoke() {
    let convert_bin = std::env::var("CONVERT_BIN").ok().unwrap_or_else(|| {
        // Fallback: repo-relative path from CARGO_MANIFEST_DIR
        // (../../converter/target/release/convert)
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| ".".to_string());
        let repo_root = std::path::PathBuf::from(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        repo_root
            .join("converter/target/release/convert")
            .to_string_lossy()
            .to_string()
    });

    if !std::path::Path::new(&convert_bin).exists() {
        eprintln!("skipping converter smoke test: {convert_bin} not found (set CONVERT_BIN to override)");
        return;
    }

    let out_dir = std::env::temp_dir().join("ae_details_monaco_converter_src");
    let dst_dir = std::env::temp_dir().join("ae_details_monaco_converter_dst");
    fs::create_dir_all(&out_dir).unwrap();
    let _ = fs::remove_dir_all(&dst_dir);

    let nodes = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
    let count = details::extract(MONACO.as_ref(), &nodes, &out_dir).unwrap();
    assert!(count > 0, "need at least one details record to smoke-test the converter");

    let status = std::process::Command::new(&convert_bin)
        .arg("--emit-details")
        .arg("--src")
        .arg(&out_dir)
        .arg("--dst")
        .arg(&dst_dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {convert_bin}: {e}"));

    assert!(status.success(), "convert --emit-details exited with {status}");
}

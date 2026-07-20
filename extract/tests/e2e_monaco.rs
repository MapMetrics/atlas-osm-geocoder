//! End-to-end test: `lib::run` on the Monaco fixture, then (if available) a
//! smoke test of the downstream Rust converter over our own output.
//!
//! Mirrors `extract_country_v3.py`'s `main()` (see
//! `scripts/extract_country_v3.py`,
//! ~lines 619-649) run end-to-end: pass 1a/1b index builds, then all seven
//! carmen layer emitters + the `/details` sidecar source, in one call.

use std::fs;

use atlas_extract::{run, RunOpts};

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
fn monaco_e2e_produces_all_layer_files() {
    let out_dir = std::env::temp_dir().join("ae_e2e_monaco_test");
    let _ = fs::remove_dir_all(&out_dir);

    let opts = RunOpts { max_nodes: 10_000_000, details: true };
    let summary = run(MONACO.as_ref(), &out_dir, &opts).expect("lib::run must succeed on the Monaco fixture");

    // Every layer we expect to have run, in order, with a name we can look
    // up by.
    let names: Vec<&str> = summary.per_layer.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec!["poi", "address", "street", "place", "region", "country", "postcode", "poi_details"],
        "layer emission order must match extract_country_v3.py's main() order"
    );

    let counts: std::collections::HashMap<&str, u64> =
        summary.per_layer.iter().map(|(n, c)| (n.as_str(), *c)).collect();

    // The 7 carmen layer files + poi_details.jsonl must all exist.
    let expected_files = [
        "poi.geojsonl",
        "address.geojsonl",
        "street.geojsonl",
        "place.geojsonl",
        "region.geojsonl",
        "country.geojsonl",
        "postcode.geojsonl",
        "poi_details.jsonl",
    ];
    for fname in expected_files {
        let path = out_dir.join(fname);
        assert!(path.exists(), "expected {fname} to exist in {}", out_dir.display());
    }

    // All non-empty EXCEPT region, which is legitimately empty for Monaco
    // (a microstate with no admin_level=4 areas and no place=state/region
    // nodes — see tests/layers_monaco.rs
    // monaco_region_extraction_runs_without_admin_level_4).
    for fname in expected_files {
        let path = out_dir.join(fname);
        let lines = read_lines(&path);
        let layer_key = fname.trim_end_matches(".geojsonl").trim_end_matches(".jsonl");
        let layer_key = if layer_key == "poi_details" { "poi_details" } else { layer_key };

        if layer_key == "region" {
            // May be empty; just check line count matches the reported count.
            assert_eq!(lines.len() as u64, counts["region"], "region.geojsonl line count must match reported count");
            continue;
        }

        assert!(!lines.is_empty(), "expected {fname} to be non-empty");
        assert_eq!(
            lines.len() as u64,
            counts[layer_key],
            "{fname} line count must match reported count"
        );
    }

    // Sanity: poi_details ids must be a subset of poi.geojsonl ids (already
    // pinned in tests/postcode_details_monaco.rs, but re-verified here as
    // part of the full end-to-end run to catch any orchestration-level
    // ordering bugs between the poi and poi_details emitters).
    let poi_ids: std::collections::HashSet<u64> = read_lines(&out_dir.join("poi.geojsonl"))
        .iter()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["id"].as_u64().unwrap()
        })
        .collect();
    for line in read_lines(&out_dir.join("poi_details.jsonl")) {
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = v["id"].as_u64().unwrap();
        assert!(poi_ids.contains(&id), "poi_details id {id} not found among poi.geojsonl ids");
    }
}

/// Converter end-to-end smoke: if the Rust converter dev binary is
/// available (env `CONVERT_BIN`, else repo-relative fallback path — same
/// resolution pattern as
/// tests/postcode_details_monaco.rs::monaco_details_satisfy_converter_emit_details_smoke),
/// run BOTH the normal bundle-build mode (`--src <out> --dst <tmp>`) and
/// the `--emit-details` sidecar mode against our own e2e output, and assert
/// both exit 0. The bundle-build run must also leave a `manifest.json` in
/// the destination directory. If the binary is absent, eprintln-skip (CI
/// hint) rather than fail.
#[test]
fn monaco_e2e_converter_bundle_and_details_smoke() {
    let convert_bin = std::env::var("CONVERT_BIN").ok().unwrap_or_else(|| {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
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
        eprintln!("skipping converter e2e smoke test: {convert_bin} not found (set CONVERT_BIN to override)");
        return;
    }

    let out_dir = std::env::temp_dir().join("ae_e2e_monaco_converter_src");
    let _ = fs::remove_dir_all(&out_dir);

    let opts = RunOpts { max_nodes: 10_000_000, details: true };
    let summary = run(MONACO.as_ref(), &out_dir, &opts).expect("lib::run must succeed on the Monaco fixture");
    assert!(!summary.per_layer.is_empty());

    // 1) Normal bundle-build mode.
    let bundle_dst = std::env::temp_dir().join("ae_e2e_monaco_converter_bundle_dst");
    let _ = fs::remove_dir_all(&bundle_dst);

    let status = std::process::Command::new(&convert_bin)
        .arg("--src")
        .arg(&out_dir)
        .arg("--dst")
        .arg(&bundle_dst)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {convert_bin}: {e}"));
    assert!(status.success(), "convert --src <out> --dst <tmp> exited with {status}");

    let manifest_path = bundle_dst.join("manifest.json");
    assert!(
        manifest_path.exists(),
        "expected manifest.json in bundle dst dir {}",
        bundle_dst.display()
    );

    // 2) --emit-details mode.
    let details_dst = std::env::temp_dir().join("ae_e2e_monaco_converter_details_dst");
    let _ = fs::remove_dir_all(&details_dst);

    let status = std::process::Command::new(&convert_bin)
        .arg("--emit-details")
        .arg("--src")
        .arg(&out_dir)
        .arg("--dst")
        .arg(&details_dst)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {convert_bin}: {e}"));
    assert!(status.success(), "convert --emit-details --src <out> --dst <tmp> exited with {status}");
}

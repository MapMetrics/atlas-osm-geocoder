//! End-to-end orchestration: PBF -> layer files.
//!
//! Mirrors `extract_country_v3.py`'s `main()` (see
//! `scripts/extract_country_v3.py`,
//! ~lines 619-649): pass 1a (`NodeTable::load`) -> pass 1b
//! (`AdminSet::load`) -> hierarchy index build (`HierarchyIndex::build`) ->
//! pass 2 emitters, run sequentially in the python's exact layer order
//! (poi, address, street, place, region, country, postcode, +poi_details),
//! printing one `[extract] {name}: {n} rows -> {path}` progress line per
//! layer (python ~line 648) as each finishes.

pub mod ids;
pub mod emit;
pub mod taxonomy;
pub mod error;
pub mod nodes;
pub mod boundaries;
pub mod hierarchy;
pub mod layers;

use std::path::{Path, PathBuf};

use crate::boundaries::AdminSet;
use crate::error::ExtractError;
use crate::hierarchy::HierarchyIndex;
use crate::layers::{address, details, place, poi, postcode, street};
use crate::nodes::NodeTable;

/// Options controlling a single `run` invocation.
#[derive(Debug, Clone)]
pub struct RunOpts {
    /// Node-table capacity guard passed straight through to
    /// [`NodeTable::load`]. v1 targets metro/regional extracts only; see
    /// [`ExtractError::TooManyNodes`].
    pub max_nodes: u64,
    /// Also emit the `/details` sidecar source (`poi_details.jsonl`) via
    /// [`layers::details::extract`].
    pub details: bool,
}

impl Default for RunOpts {
    fn default() -> Self {
        RunOpts { max_nodes: 400_000_000, details: false }
    }
}

/// Result of a full `run`: one `(layer_name, row_count)` pair per layer
/// emitted, in emission order.
#[derive(Debug, Clone)]
pub struct Summary {
    pub per_layer: Vec<(String, u64)>,
}

/// Run the full extraction pipeline for a single PBF: pass 1a/1b index
/// builds, then all seven carmen layer emitters (plus the `/details`
/// sidecar source when `opts.details`), writing every layer file into
/// `out` (created if missing). Returns a [`Summary`] with each layer's row
/// count in emission order.
pub fn run(pbf: &Path, out: &Path, opts: &RunOpts) -> Result<Summary, ExtractError> {
    std::fs::create_dir_all(out)?;

    let nodes = NodeTable::load(pbf, opts.max_nodes)?;
    let admin = AdminSet::load(pbf, &nodes)?;
    let hier = HierarchyIndex::build(&admin);

    let mut per_layer: Vec<(String, u64)> = Vec::new();

    run_layer(&mut per_layer, "poi", out, || poi::extract(pbf, &nodes, &hier, out))?;
    run_layer(&mut per_layer, "address", out, || address::extract(pbf, &nodes, &hier, out))?;
    run_layer(&mut per_layer, "street", out, || street::extract(pbf, &nodes, &hier, out))?;
    run_layer(&mut per_layer, "place", out, || place::extract_places(&admin, out))?;
    run_layer(&mut per_layer, "region", out, || place::extract_regions(&admin, out))?;
    run_layer(&mut per_layer, "country", out, || place::extract_countries(&admin, out))?;
    run_layer(&mut per_layer, "postcode", out, || postcode::extract(pbf, &nodes, out))?;

    if opts.details {
        run_layer(&mut per_layer, "poi_details", out, || details::extract(pbf, &nodes, out))?;
    }

    Ok(Summary { per_layer })
}

/// Layer filename for progress-line reporting. Matches each layer emitter's
/// own `out_dir.join(...)` call.
fn layer_filename(name: &str) -> &'static str {
    match name {
        "poi" => "poi.geojsonl",
        "address" => "address.geojsonl",
        "street" => "street.geojsonl",
        "place" => "place.geojsonl",
        "region" => "region.geojsonl",
        "country" => "country.geojsonl",
        "postcode" => "postcode.geojsonl",
        "poi_details" => "poi_details.jsonl",
        _ => unreachable!("unknown layer name: {name}"),
    }
}

/// Run one layer emitter, print its `[extract] {name}: {n} rows -> {path}`
/// progress line (python `extract_country_v3.py` ~line 648), and record its
/// count into `per_layer`.
fn run_layer(
    per_layer: &mut Vec<(String, u64)>,
    name: &str,
    out_dir: &Path,
    f: impl FnOnce() -> Result<u64, ExtractError>,
) -> Result<(), ExtractError> {
    println!("[extract] {name} ...");
    let n = f()?;
    let path: PathBuf = out_dir.join(layer_filename(name));
    println!("[extract] {name}: {n} rows -> {}", path.display());
    per_layer.push((name.to_string(), n));
    Ok(())
}

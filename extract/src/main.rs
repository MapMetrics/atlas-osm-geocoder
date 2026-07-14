//! `atlas-extract` CLI: OSM PBF -> carmen-format geocoder layer files.
//!
//! `atlas-extract --pbf <file> --out <dir> [--max-nodes N] [--details]`

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use atlas_extract::{run, RunOpts};

#[derive(Parser)]
#[command(name = "atlas-extract", version, about = "OSM PBF -> carmen-format geocoder layer files")]
struct Cli {
    /// Input OSM PBF file (e.g. a country/metro extract).
    #[arg(long)]
    pbf: PathBuf,

    /// Output directory for the layer files (created if missing).
    #[arg(long)]
    out: PathBuf,

    /// Node-table capacity guard. v1 targets metro/regional extracts only;
    /// planet-scale inputs need a different (disk-backed) node store.
    #[arg(long, default_value_t = 400_000_000)]
    max_nodes: u64,

    /// Also emit the `/details` sidecar source (`poi_details.jsonl`).
    #[arg(long, default_value_t = false)]
    details: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let opts = RunOpts { max_nodes: cli.max_nodes, details: cli.details };

    match run(&cli.pbf, &cli.out, &opts) {
        Ok(_summary) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("atlas-extract: {e}");
            ExitCode::FAILURE
        }
    }
}

//! Pass 1a: an in-memory node location table.
//!
//! We store `(lon, lat)` as `f32` pairs (8 bytes/node) rather than `f64`
//! (16 bytes/node). `f32` gives ~7 decimal digits of precision, i.e. sub-cm
//! accuracy at the equator — more than enough for geocoding — while roughly
//! halving the resident memory for the largest extracts this crate targets
//! (Netherlands-scale, ~60M nodes: ~1.4 GB including `FxHashMap` overhead,
//! vs. ~2.4+ GB with `f64`). Planet-scale extracts are explicitly out of
//! scope for v1 (see `ExtractError::TooManyNodes`); a disk-backed table
//! would be needed there.

use std::path::Path;

use fxhash::FxHashMap;
use osmpbf::{Element, ElementReader};

use crate::error::ExtractError;

#[derive(Debug)]
pub struct NodeTable {
    locations: FxHashMap<i64, (f32, f32)>,
}

impl NodeTable {
    /// Read every node (both plain `Element::Node` and the denser
    /// `Element::DenseNode` encoding) out of `pbf`, keeping only their
    /// `(lon, lat)` locations keyed by OSM node id. Bails out with
    /// `ExtractError::TooManyNodes` as soon as the node count would exceed
    /// `max_nodes`, rather than growing an unbounded map for planet-scale
    /// inputs.
    pub fn load(pbf: &Path, max_nodes: u64) -> Result<Self, ExtractError> {
        let reader = ElementReader::from_path(pbf)?;
        let mut locations: FxHashMap<i64, (f32, f32)> = FxHashMap::default();
        let mut nodes_seen: u64 = 0;

        reader.for_each(|element| {
            match element {
                Element::Node(n) => {
                    nodes_seen += 1;
                    if nodes_seen <= max_nodes {
                        locations.insert(n.id(), (n.lon() as f32, n.lat() as f32));
                    }
                }
                Element::DenseNode(n) => {
                    nodes_seen += 1;
                    if nodes_seen <= max_nodes {
                        locations.insert(n.id(), (n.lon() as f32, n.lat() as f32));
                    }
                }
                _ => {}
            }
        })?;

        if nodes_seen > max_nodes {
            return Err(ExtractError::TooManyNodes {
                seen: nodes_seen,
                max: max_nodes,
            });
        }

        Ok(NodeTable { locations })
    }

    /// Look up a node's `(lon, lat)` by OSM id, widened back to `f64`.
    pub fn get(&self, id: i64) -> Option<(f64, f64)> {
        self.locations
            .get(&id)
            .map(|&(lon, lat)| (lon as f64, lat as f64))
    }

    pub fn len(&self) -> u64 {
        self.locations.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixture lives at extract/tests/fixtures/monaco.osm.pbf; cargo runs unit
    // tests with CWD = the crate root (extract/), matching the integration
    // test's relative path.
    const MONACO: &str = "tests/fixtures/monaco.osm.pbf";

    #[test]
    fn get_returns_none_for_unknown_id() {
        let t = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
        assert_eq!(t.get(i64::MAX), None);
    }

    #[test]
    fn get_returns_a_coordinate_inside_monaco_bbox() {
        let t = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();

        // Ground truth: ask osmpbf directly for a handful of real node ids
        // from the fixture, then confirm NodeTable resolves each of them to
        // a sane (lon, lat) inside Monaco's bounding box (~7.40..7.45,
        // ~43.72..43.75, widened for safety margin).
        let reader = osmpbf::ElementReader::from_path(MONACO).unwrap();
        let mut sample_ids = Vec::new();
        reader
            .for_each(|element| {
                if sample_ids.len() >= 50 {
                    return;
                }
                let id = match element {
                    osmpbf::Element::Node(n) => Some(n.id()),
                    osmpbf::Element::DenseNode(n) => Some(n.id()),
                    _ => None,
                };
                if let Some(id) = id {
                    sample_ids.push(id);
                }
            })
            .unwrap();

        assert!(!sample_ids.is_empty(), "fixture had no node elements");
        for id in sample_ids {
            let (lon, lat) = t.get(id).unwrap_or_else(|| panic!("missing node {id}"));
            assert!((7.3..7.6).contains(&lon), "lon out of range: {lon}");
            assert!((43.6..43.9).contains(&lat), "lat out of range: {lat}");
        }
    }

    #[test]
    fn len_matches_number_of_stored_nodes() {
        let t = NodeTable::load(MONACO.as_ref(), 10_000_000).unwrap();
        assert!(!t.is_empty());
        assert_eq!(t.len(), t.locations.len() as u64);
    }

    #[test]
    fn too_many_nodes_is_rejected_with_typed_error() {
        // Monaco has ~41.5K nodes (see task-3 report); capping far below
        // that must trip the capacity guard rather than silently truncate.
        let err = NodeTable::load(MONACO.as_ref(), 10).unwrap_err();
        match err {
            ExtractError::TooManyNodes { seen, max } => {
                assert!(seen >= 10, "seen={seen}");
                assert_eq!(max, 10);
            }
            other => panic!("expected TooManyNodes, got {other}"),
        }
    }

    #[test]
    fn capacity_guard_boundary_test_exact_max_should_succeed() {
        // Determine Monaco's exact node count first.
        let reader = osmpbf::ElementReader::from_path(MONACO).unwrap();
        let mut exact_count = 0u64;
        reader
            .for_each(|element| {
                match element {
                    osmpbf::Element::Node(_) | osmpbf::Element::DenseNode(_) => {
                        exact_count += 1;
                    }
                    _ => {}
                }
            })
            .unwrap();

        eprintln!("Monaco exact node count: {}", exact_count);

        // BOUNDARY TEST 1: max_nodes == exact_count should succeed (INCLUSIVE semantics).
        let result = NodeTable::load(MONACO.as_ref(), exact_count);
        assert!(
            result.is_ok(),
            "Loading Monaco with max_nodes={} (exact count) should succeed, but got: {:?}",
            exact_count,
            result
        );
        let table = result.unwrap();
        assert_eq!(
            table.len(),
            exact_count,
            "Loaded table should have exactly {} nodes",
            exact_count
        );

        // BOUNDARY TEST 2: max_nodes == exact_count + 1 should also succeed.
        let result = NodeTable::load(MONACO.as_ref(), exact_count + 1);
        assert!(
            result.is_ok(),
            "Loading Monaco with max_nodes={} (one above count) should succeed",
            exact_count + 1
        );

        // BOUNDARY TEST 3: max_nodes == exact_count - 1 should fail.
        let err = NodeTable::load(MONACO.as_ref(), exact_count - 1).unwrap_err();
        match err {
            ExtractError::TooManyNodes { seen, max } => {
                assert_eq!(max, exact_count - 1);
                assert!(
                    seen >= exact_count,
                    "Overflow must be detected: seen={} should be >= {}",
                    seen,
                    exact_count
                );
            }
            other => panic!("expected TooManyNodes, got {other}"),
        }
    }
}

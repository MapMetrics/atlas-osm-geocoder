//! Small helpers shared across layer emitters (`poi`, `address`, ...).
//!
//! Both `layers::poi` and `layers::address` need to (a) turn an `osmpbf`
//! tag iterator into an owned [`TagMap`], and (b) resolve a way's centroid
//! from its member node refs via a [`NodeTable`]. Lifted out of `poi.rs`
//! (where these were originally private) into this module so `address.rs`
//! can reuse them without duplicating the logic.

use crate::nodes::NodeTable;
use crate::taxonomy::TagMap;

/// Collect an `osmpbf` tag iterator (`(&str, &str)` pairs) into an owned
/// [`TagMap`].
pub(crate) fn tags_to_map<'a>(iter: impl Iterator<Item = (&'a str, &'a str)>) -> TagMap {
    iter.map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Centroid (simple arithmetic mean) of a way's resolvable member node
/// locations. Returns `None` if zero member nodes resolve via `nodes`
/// (caller counts this as a skip, per the brief).
pub(crate) fn way_centroid(way_refs: &[i64], nodes: &NodeTable) -> Option<(f64, f64)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_to_map_collects_pairs() {
        let pairs = vec![("amenity", "cafe"), ("name", "Joe's")];
        let map = tags_to_map(pairs.into_iter());
        assert_eq!(map.get("amenity").map(String::as_str), Some("cafe"));
        assert_eq!(map.get("name").map(String::as_str), Some("Joe's"));
    }
}

//! Pass 1b: admin boundary assembly + place nodes.
//!
//! Two things come out of this pass:
//!
//! 1. **Admin areas** — `boundary=administrative` relations (e.g. countries,
//!    regions, municipalities), each carrying an `admin_level` and a set of
//!    closed polygon rings built by stitching together the node-lists of
//!    their `outer`-role member ways.
//! 2. **Place nodes** — plain OSM nodes tagged `place=*` with a `name`
//!    (cities, towns, villages, ...), used later for point-based geocoding
//!    entries and as PIP fallbacks.
//!
//! Ring assembly is a three-sub-pass streaming read of the same `.osm.pbf`
//! (each sub-pass opens its own `ElementReader`, since `osmpbf` readers are
//! single-use):
//!
//! 1. **Relations**: scan every `Relation` with `boundary=administrative`
//!    and a non-empty `name`, recording its `admin_level` + name and the set
//!    of member way ids that have role `outer` (or empty role, which OSM
//!    convention also treats as outer for old data).
//! 2. **Ways**: scan every `Way`, and for those whose id was collected in
//!    step 1, record its ordered list of node refs.
//! 3. **Assembly**: for each relation, stitch its member ways' node-ref
//!    lists into closed rings via endpoint matching, resolve node ids to
//!    coordinates via the `NodeTable`, and build a `geo::Polygon` per closed
//!    ring. Rings that cannot be closed are dropped (never panic) with a
//!    warning; a running counter is reported in the final summary printed by
//!    the CLI / test harness.
//!
//! v1 scope note: only exterior (`outer`) rings are assembled into
//! `AdminArea::rings`. Inner rings (`inner`-role members, i.e. holes such as
//! enclaves/exclaves) are intentionally NOT subtracted in v1 — a
//! point-in-polygon test against `rings` alone can produce a false positive
//! for a point that actually falls inside a hole. This is an accepted v1
//! limitation (see brief); proper hole support is deferred to v2.

use std::collections::HashMap;
use std::path::Path;

use geo::{Coord, LineString, Polygon};
use osmpbf::{Element, ElementReader, RelMemberType};

use crate::error::ExtractError;
use crate::nodes::NodeTable;

#[derive(Debug, Clone)]
pub struct AdminArea {
    pub name: String,
    pub admin_level: u8,
    pub rings: Vec<Polygon<f64>>,
}

#[derive(Debug, Clone)]
pub struct PlaceNode {
    pub name: String,
    pub place: String,
    pub population: u64,
    pub lon: f64,
    pub lat: f64,
    pub id: i64,
}

#[derive(Debug, Default)]
pub struct AdminSet {
    areas: Vec<AdminArea>,
    place_nodes: Vec<PlaceNode>,
}

/// An admin relation collected in sub-pass 1 (relations), before its member
/// ways' node lists have been resolved.
struct RelationRecord {
    name: String,
    admin_level: u8,
    /// Member way ids with role `outer` (or empty role), in relation member
    /// order.
    outer_way_ids: Vec<i64>,
}

impl AdminSet {
    /// Streaming three-sub-pass read of `pbf`: relations -> ways -> ring
    /// assembly, plus a single-pass collection of `place=*` nodes (folded
    /// into the ways sub-pass to avoid a fourth full read).
    pub fn load(pbf: &Path, nodes: &NodeTable) -> Result<Self, ExtractError> {
        // ── Sub-pass 1: relations ───────────────────────────────────────
        // boundary=administrative relations with a name; record admin_level
        // and the way ids referenced with an "outer" (or empty) role.
        let mut relations: Vec<RelationRecord> = Vec::new();
        let mut wanted_way_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();

        let reader = ElementReader::from_path(pbf)?;
        reader.for_each(|element| {
            if let Element::Relation(rel) = element {
                let mut tags: HashMap<&str, &str> = HashMap::new();
                for (k, v) in rel.tags() {
                    tags.insert(k, v);
                }

                let is_admin_boundary = tags.get("boundary") == Some(&"administrative");
                let name = tags.get("name").copied().unwrap_or("");
                if !is_admin_boundary || name.is_empty() {
                    return;
                }

                let admin_level = match admin_relation_gate(&tags) {
                    Some(level) => level,
                    None => {
                        eprintln!(
                            "warning: admin relation '{name}' has missing or unparseable admin_level, skipping"
                        );
                        return;
                    }
                };

                let mut outer_way_ids = Vec::new();
                for member in rel.members() {
                    if member.member_type != RelMemberType::Way {
                        continue;
                    }
                    let role = member.role().unwrap_or("");
                    if role == "outer" || role.is_empty() {
                        outer_way_ids.push(member.member_id);
                        wanted_way_ids.insert(member.member_id);
                    }
                }

                if outer_way_ids.is_empty() {
                    return;
                }

                relations.push(RelationRecord {
                    name: name.to_string(),
                    admin_level,
                    outer_way_ids,
                });
            }
        })?;

        // ── Sub-pass 2: ways ─────────────────────────────────────────────
        // Capture node-id lists only for ways referenced by an admin
        // relation above, plus collect place=* nodes in the same pass.
        let mut way_node_ids: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut place_nodes: Vec<PlaceNode> = Vec::new();

        let reader = ElementReader::from_path(pbf)?;
        reader.for_each(|element| match element {
            Element::Way(way) => {
                if wanted_way_ids.contains(&way.id()) {
                    let refs: Vec<i64> = way.refs().collect();
                    way_node_ids.insert(way.id(), refs);
                }
            }
            Element::Node(node) => {
                if let Some(pn) =
                    place_node_from_tags(node.id(), node.lon(), node.lat(), node.tags())
                {
                    place_nodes.push(pn);
                }
            }
            Element::DenseNode(node) => {
                if let Some(pn) =
                    place_node_from_tags(node.id(), node.lon(), node.lat(), node.tags())
                {
                    place_nodes.push(pn);
                }
            }
            _ => {}
        })?;

        // ── Sub-pass 3: ring assembly ────────────────────────────────────
        let mut areas = Vec::with_capacity(relations.len());
        let mut dropped_rings: u64 = 0;

        for rel in &relations {
            // Gather this relation's outer ways' node-id lists (skip member
            // ways we never captured, e.g. missing from the file).
            let way_lists: Vec<&Vec<i64>> = rel
                .outer_way_ids
                .iter()
                .filter_map(|wid| way_node_ids.get(wid))
                .collect();

            if way_lists.is_empty() {
                eprintln!(
                    "warning: admin relation '{}' (admin_level={}) has no resolvable outer ways, skipping",
                    rel.name, rel.admin_level
                );
                continue;
            }

            let (closed_rings, dropped) = stitch_rings(&way_lists);
            dropped_rings += dropped;

            let mut polygons = Vec::with_capacity(closed_rings.len());
            for ring_ids in closed_rings {
                match resolve_ring(&ring_ids, nodes) {
                    Some(coords) => {
                        let line_string = LineString::new(coords);
                        polygons.push(Polygon::new(line_string, vec![]));
                    }
                    None => {
                        dropped_rings += 1;
                        eprintln!(
                            "warning: admin relation '{}' (admin_level={}) dropped a ring with unresolvable node coordinates",
                            rel.name, rel.admin_level
                        );
                    }
                }
            }

            if polygons.is_empty() {
                eprintln!(
                    "warning: admin relation '{}' (admin_level={}) produced zero closed rings, skipping",
                    rel.name, rel.admin_level
                );
                continue;
            }

            areas.push(AdminArea {
                name: rel.name.clone(),
                admin_level: rel.admin_level,
                rings: polygons,
            });
        }

        if dropped_rings > 0 {
            eprintln!("boundaries: dropped {dropped_rings} unclosed/unresolvable ring(s)");
        }

        Ok(AdminSet { areas, place_nodes })
    }

    pub fn areas(&self) -> &[AdminArea] {
        &self.areas
    }

    pub fn place_nodes(&self) -> &[PlaceNode] {
        &self.place_nodes
    }

    /// Test-only constructor so downstream consumers (Task 5+) can build an
    /// `AdminSet` directly from in-memory fixtures without a `.osm.pbf`
    /// round-trip.
    pub fn for_test(areas: Vec<AdminArea>, places: Vec<PlaceNode>) -> Self {
        AdminSet {
            areas,
            place_nodes: places,
        }
    }
}

/// Build a `PlaceNode` from a node's id/lon/lat/tags iterator if it carries
/// `place=*` and a non-empty `name`. Population is parsed leniently: strip
/// spaces/commas/dots, then `parse().unwrap_or(0)`.
fn place_node_from_tags<'a>(
    id: i64,
    lon: f64,
    lat: f64,
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<PlaceNode> {
    let mut place: Option<&str> = None;
    let mut name: Option<&str> = None;
    let mut population_raw: Option<&str> = None;

    for (k, v) in tags {
        match k {
            "place" => place = Some(v),
            "name" => name = Some(v),
            "population" => population_raw = Some(v),
            _ => {}
        }
    }

    let place = place?;
    let name = name?;
    if name.is_empty() {
        return None;
    }

    let population = population_raw
        .map(|raw| {
            raw.chars()
                .filter(|c| !matches!(c, ' ' | ',' | '.'))
                .collect::<String>()
        })
        .and_then(|cleaned| cleaned.parse::<u64>().ok())
        .unwrap_or(0);

    Some(PlaceNode {
        name: name.to_string(),
        place: place.to_string(),
        population,
        lon,
        lat,
        id,
    })
}

/// Gate for whether a relation's tags qualify it as an admin boundary worth
/// processing, and if so, its parsed `admin_level`.
///
/// Returns `None` when `boundary != "administrative"`, or when
/// `admin_level` is missing or fails to parse as `u8`. OSM admin levels
/// start at `2` (there is no level `0` or `1` in practice), so a missing/
/// unparseable `admin_level` has no safe default: synthesizing `0` would
/// silently outrank every real country in lowest-level-wins logic
/// downstream. Callers must skip the relation entirely in that case (mirror
/// the `name.is_empty()` early-return already used for missing names).
fn admin_relation_gate(tags: &HashMap<&str, &str>) -> Option<u8> {
    if tags.get("boundary") != Some(&"administrative") {
        return None;
    }
    tags.get("admin_level").and_then(|s| s.parse::<u8>().ok())
}

/// A candidate way that can extend a chain at one of its ends, found while
/// scanning `remaining` in [`stitch_rings`].
struct Candidate {
    /// Index into `remaining`.
    index: usize,
    /// Whether the candidate's node list needs to be reversed before
    /// splicing (i.e. its matching endpoint was the *other* end of its own
    /// list).
    reverse: bool,
    /// Whether the candidate attaches to the chain's start (prepend) rather
    /// than the chain's end (append).
    prepend: bool,
    /// The candidate's node id at its non-matching (far) end, after
    /// accounting for `reverse` — i.e. the node id the chain would grow to
    /// if this candidate is chosen. Used for the "closes immediately" and
    /// degree tie-break heuristics.
    far_endpoint: i64,
}

/// Deterministic adjacency-based ring stitcher: given a set of way node-id
/// lists (each way's endpoints are its first/last node ids), chain ways
/// whose endpoints match into closed rings.
///
/// At each chain end:
/// - **Zero** matching unused candidates: the chain can't be extended
///   further from that end.
/// - **Exactly one** matching candidate: extend deterministically (handling
///   reversal/prepend as needed).
/// - **More than one** (an ambiguous junction, e.g. three-plus boundaries
///   meeting at a shared node): prefer a candidate that closes the chain
///   immediately (its far endpoint equals the chain's other end) if one
///   exists; otherwise prefer the candidate whose far endpoint has the
///   fewest other remaining candidates touching it (degree heuristic, so we
///   walk into the least-branchy part of the graph first); ties broken
///   deterministically by lowest far-endpoint node id — a property of the
///   graph itself, not of input list order or hash/iteration order — so
///   results are reproducible across runs regardless of the order ways
///   happen to appear in the relation's member list.
///
/// A chain that cannot be extended from either end and isn't closed is
/// dropped (counted, `eprintln!` warning) rather than guessed at. Returns
/// `(closed_rings, dropped_count)`. A ring is closed when its final chained
/// node-id list starts and ends with the same node id and has at least 4
/// node ids (3 distinct + closing point).
fn stitch_rings(way_lists: &[&Vec<i64>]) -> (Vec<Vec<i64>>, u64) {
    // Work on owned copies since we consume ways as we chain them.
    let mut remaining: Vec<Vec<i64>> = way_lists
        .iter()
        .filter(|w| w.len() >= 2)
        .map(|w| (*w).clone())
        .collect();

    let mut closed_rings = Vec::new();
    let mut dropped = 0u64;

    while !remaining.is_empty() {
        let mut chain = remaining.remove(0);

        loop {
            if chain.first() == chain.last() && chain.len() >= 2 {
                // Already closed.
                break;
            }

            let chain_start = *chain.first().unwrap();
            let chain_end = *chain.last().unwrap();

            // Collect every remaining way that can attach to either end of
            // the chain, in any orientation.
            let mut candidates: Vec<Candidate> = Vec::new();
            for (i, candidate) in remaining.iter().enumerate() {
                let c_start = *candidate.first().unwrap();
                let c_end = *candidate.last().unwrap();

                if c_start == chain_end {
                    candidates.push(Candidate {
                        index: i,
                        reverse: false,
                        prepend: false,
                        far_endpoint: c_end,
                    });
                } else if c_end == chain_end && c_start != c_end {
                    candidates.push(Candidate {
                        index: i,
                        reverse: true,
                        prepend: false,
                        far_endpoint: c_start,
                    });
                }

                if c_end == chain_start {
                    candidates.push(Candidate {
                        index: i,
                        reverse: false,
                        prepend: true,
                        far_endpoint: c_start,
                    });
                } else if c_start == chain_start && c_start != c_end {
                    candidates.push(Candidate {
                        index: i,
                        reverse: true,
                        prepend: true,
                        far_endpoint: c_end,
                    });
                }
            }

            let chosen = match candidates.len() {
                0 => None,
                1 => Some(candidates.into_iter().next().unwrap()),
                _ => {
                    // Ambiguous junction: more than one way could extend
                    // this chain end. Resolve deterministically.

                    // 1. Prefer a candidate that closes the ring immediately.
                    if let Some(pos) = candidates
                        .iter()
                        .position(|c| c.far_endpoint == chain_start || c.far_endpoint == chain_end)
                    {
                        Some(candidates.swap_remove(pos))
                    } else {
                        // 2. Degree heuristic: prefer the candidate whose far
                        // endpoint has the fewest OTHER remaining candidates
                        // touching it (excluding this candidate itself).
                        let degree_of = |far: i64, skip_index: usize| -> usize {
                            remaining
                                .iter()
                                .enumerate()
                                .filter(|&(i, w)| {
                                    i != skip_index
                                        && (*w.first().unwrap() == far || *w.last().unwrap() == far)
                                })
                                .count()
                        };

                        let mut best_pos = 0usize;
                        let mut best_degree = usize::MAX;
                        let mut tie = false;
                        for (pos, c) in candidates.iter().enumerate() {
                            let d = degree_of(c.far_endpoint, c.index);
                            if d < best_degree {
                                best_degree = d;
                                best_pos = pos;
                                tie = false;
                            } else if d == best_degree {
                                tie = true;
                            }
                        }

                        if tie {
                            // Still tied on degree: break deterministically
                            // by lowest far-endpoint node id. This is a
                            // property of the graph itself (not of input
                            // list order or hash/iteration order), so the
                            // result is reproducible regardless of the
                            // order ways happen to appear in the relation's
                            // member list.
                            candidates.sort_by_key(|c| (c.far_endpoint, c.index));
                            Some(candidates.into_iter().next().unwrap())
                        } else {
                            Some(candidates.swap_remove(best_pos))
                        }
                    }
                }
            };

            match chosen {
                Some(c) => {
                    let mut candidate = remaining.remove(c.index);
                    if c.reverse {
                        candidate.reverse();
                    }
                    if c.prepend {
                        // candidate's matching endpoint == chain's first
                        // node; drop the duplicate join point.
                        candidate.pop();
                        candidate.extend(chain);
                        chain = candidate;
                    } else {
                        // candidate's matching endpoint == chain's last
                        // node; drop the duplicate join point.
                        let mut candidate = candidate;
                        candidate.remove(0);
                        chain.extend(candidate);
                    }
                }
                None => {
                    // No more ways can extend this chain: it never closed.
                    break;
                }
            }
        }

        if chain.first() == chain.last() && chain.len() >= 4 {
            closed_rings.push(chain);
        } else {
            dropped += 1;
            eprintln!(
                "warning: dropped unclosed ring (segment of {} node ids, first={:?}, last={:?})",
                chain.len(),
                chain.first(),
                chain.last()
            );
        }
    }

    (closed_rings, dropped)
}

/// Resolve a closed ring's node ids to `(lon, lat)` coordinates via the
/// `NodeTable`. Returns `None` (dropping the ring) if any node id can't be
/// resolved, rather than panicking or silently producing a malformed
/// polygon.
fn resolve_ring(ring_ids: &[i64], nodes: &NodeTable) -> Option<Vec<Coord<f64>>> {
    let mut coords = Vec::with_capacity(ring_ids.len());
    for &id in ring_ids {
        let (lon, lat) = nodes.get(id)?;
        coords.push(Coord { x: lon, y: lat });
    }
    Some(coords)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stitch_rings_closes_two_ways_sharing_endpoints() {
        // Two ways sharing endpoints: 1-2-3 and 3-4-1 stitch into a closed
        // ring 1-2-3-4-1.
        let way_a = vec![1, 2, 3];
        let way_b = vec![3, 4, 1];
        let ways: Vec<&Vec<i64>> = vec![&way_a, &way_b];
        let (rings, dropped) = stitch_rings(&ways);
        assert_eq!(dropped, 0);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].first(), rings[0].last());
        assert!(rings[0].len() >= 4);
    }

    #[test]
    fn stitch_rings_handles_reversed_way_orientation() {
        // way_b's node list is stored tail-to-head relative to way_a; the
        // stitcher must try reversed matches too.
        let way_a = vec![1, 2, 3];
        let way_b = vec![1, 4, 3]; // shares endpoints 1 and 3 with way_a, reversed
        let ways: Vec<&Vec<i64>> = vec![&way_a, &way_b];
        let (rings, dropped) = stitch_rings(&ways);
        assert_eq!(dropped, 0);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].first(), rings[0].last());
    }

    #[test]
    fn stitch_rings_drops_unclosed_segment_without_panicking() {
        // A single way whose endpoints don't match anything else never
        // closes; it must be dropped, not panic.
        let way_a = vec![1, 2, 3, 4]; // 1 != 4, never closes
        let ways: Vec<&Vec<i64>> = vec![&way_a];
        let (rings, dropped) = stitch_rings(&ways);
        assert_eq!(rings.len(), 0);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn stitch_rings_already_closed_single_way() {
        let way_a = vec![1, 2, 3, 1];
        let ways: Vec<&Vec<i64>> = vec![&way_a];
        let (rings, dropped) = stitch_rings(&ways);
        assert_eq!(dropped, 0);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0], vec![1, 2, 3, 1]);
    }

    #[test]
    fn stitch_rings_handles_multiple_independent_rings() {
        // Two disjoint closed rings among the input ways.
        let ring1a = vec![1, 2, 3];
        let ring1b = vec![3, 1];
        let ring2 = vec![10, 20, 30, 10];
        let ways: Vec<&Vec<i64>> = vec![&ring1a, &ring1b, &ring2];
        let (rings, dropped) = stitch_rings(&ways);
        assert_eq!(dropped, 0);
        assert_eq!(rings.len(), 2);
    }

    #[test]
    fn stitch_rings_does_not_mis_weld_at_shared_junction_node() {
        // A=[1,2], B=[2,3], C=[3,1], D=[2,4], E=[4,1].
        //
        // A, B, C form a genuine closed ring: 1->2->3->1.
        // D, E form an unclosed chain: 2->4->1 (needs 1->2 to close, but way A
        // is already consumed by the first ring and each way may only be used
        // once).
        //
        // Node 2 is a shared junction: both B and D start there, and A ends
        // there. A naive greedy stitcher can weld A to D (or B) by iteration
        // order alone, producing a spurious ring and leaving a genuine ring's
        // ways stranded/dropped. The correct decomposition is exactly ONE
        // closed ring (a rotation of 1-2-3-1) plus one dropped unclosed chain.
        let way_a = vec![1, 2];
        let way_b = vec![2, 3];
        let way_c = vec![3, 1];
        let way_d = vec![2, 4];
        let way_e = vec![4, 1];
        let ways: Vec<&Vec<i64>> = vec![&way_a, &way_b, &way_c, &way_d, &way_e];

        let (rings, dropped) = stitch_rings(&ways);

        assert_eq!(dropped, 1, "expected exactly one dropped unclosed chain");
        assert_eq!(rings.len(), 1, "expected exactly one closed ring");

        let ring = &rings[0];
        assert_eq!(ring.first(), ring.last(), "ring must be closed");

        // The closed ring's node sequence must be a rotation of 1-2-3-1, in
        // either winding direction. Normalize by rotating to start at node 1
        // (dropping the duplicate closing point first).
        let mut interior: Vec<i64> = ring[..ring.len() - 1].to_vec();
        let start = interior
            .iter()
            .position(|&n| n == 1)
            .expect("ring must contain node 1");
        interior.rotate_left(start);

        let forward = vec![1, 2, 3];
        let mut reversed = forward.clone();
        reversed.reverse();
        assert!(
            interior == forward || interior == reversed,
            "expected ring interior to be a rotation of 1-2-3 (either direction), got {interior:?}"
        );
    }

    #[test]
    fn stitch_rings_junction_disambiguation_is_order_independent() {
        // Same topology as `stitch_rings_does_not_mis_weld_at_shared_junction_node`
        // (A=[1,2], B=[2,3], C=[3,1], D=[2,4], E=[4,1]), but with D listed
        // immediately after A. A naive greedy scanner (matches whichever
        // candidate it encounters first in list order) welds A to D here
        // instead of A to B, producing a spurious ring 1-2-4-1 and dropping
        // B+C instead of D+E. The result must not depend on input order: it
        // must still be exactly one closed ring, a rotation of 1-2-3-1.
        let way_a = vec![1, 2];
        let way_d = vec![2, 4];
        let way_b = vec![2, 3];
        let way_c = vec![3, 1];
        let way_e = vec![4, 1];
        let ways: Vec<&Vec<i64>> = vec![&way_a, &way_d, &way_b, &way_c, &way_e];

        let (rings, dropped) = stitch_rings(&ways);

        assert_eq!(dropped, 1, "expected exactly one dropped unclosed chain");
        assert_eq!(rings.len(), 1, "expected exactly one closed ring");

        let ring = &rings[0];
        assert_eq!(ring.first(), ring.last(), "ring must be closed");

        let mut interior: Vec<i64> = ring[..ring.len() - 1].to_vec();
        let start = interior
            .iter()
            .position(|&n| n == 1)
            .expect("ring must contain node 1");
        interior.rotate_left(start);

        let forward = vec![1, 2, 3];
        let mut reversed = forward.clone();
        reversed.reverse();
        assert!(
            interior == forward || interior == reversed,
            "expected ring interior to be a rotation of 1-2-3 (either direction) regardless of input order, got {interior:?}"
        );
    }

    fn tags<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Iterator<Item = (&'a str, &'a str)> {
        pairs.iter().copied()
    }

    #[test]
    fn place_node_requires_name() {
        let result = place_node_from_tags(1, 7.42, 43.73, tags(&[("place", "city")]));
        assert!(result.is_none());
    }

    #[test]
    fn place_node_requires_place_tag() {
        let result = place_node_from_tags(1, 7.42, 43.73, tags(&[("name", "Monaco")]));
        assert!(result.is_none());
    }

    #[test]
    fn place_node_rejects_empty_name() {
        let result = place_node_from_tags(1, 7.42, 43.73, tags(&[("place", "city"), ("name", "")]));
        assert!(result.is_none());
    }

    #[test]
    fn place_node_parses_population_leniently() {
        let pn = place_node_from_tags(
            1,
            7.42,
            43.73,
            tags(&[
                ("place", "city"),
                ("name", "Monaco"),
                ("population", "38,300"),
            ]),
        )
        .unwrap();
        assert_eq!(pn.population, 38_300);
        assert_eq!(pn.name, "Monaco");
        assert_eq!(pn.place, "city");
        assert_eq!(pn.id, 1);
    }

    #[test]
    fn place_node_population_defaults_to_zero_when_missing_or_unparseable() {
        let pn = place_node_from_tags(1, 7.42, 43.73, tags(&[("place", "city"), ("name", "X")]))
            .unwrap();
        assert_eq!(pn.population, 0);

        let pn2 = place_node_from_tags(
            2,
            7.42,
            43.73,
            tags(&[("place", "city"), ("name", "Y"), ("population", "n/a")]),
        )
        .unwrap();
        assert_eq!(pn2.population, 0);
    }

    #[test]
    fn place_node_population_strips_spaces_and_dots() {
        let pn = place_node_from_tags(
            1,
            7.42,
            43.73,
            tags(&[
                ("place", "town"),
                ("name", "Test"),
                ("population", "1.234.567"),
            ]),
        )
        .unwrap();
        assert_eq!(pn.population, 1_234_567);
    }

    #[test]
    fn admin_relation_gate_skips_when_admin_level_missing() {
        // boundary=administrative + name present, but no admin_level tag at
        // all: OSM admin levels start at 2, so there is no safe default.
        // Synthesizing 0 would outrank every real country in
        // lowest-level-wins logic downstream. The relation must be skipped,
        // not defaulted.
        let tags: HashMap<&str, &str> =
            HashMap::from([("boundary", "administrative"), ("name", "Testland")]);
        assert_eq!(admin_relation_gate(&tags), None);
    }

    #[test]
    fn admin_relation_gate_skips_when_admin_level_unparseable() {
        let tags: HashMap<&str, &str> = HashMap::from([
            ("boundary", "administrative"),
            ("name", "Testland"),
            ("admin_level", "not-a-number"),
        ]);
        assert_eq!(admin_relation_gate(&tags), None);
    }

    #[test]
    fn admin_relation_gate_accepts_valid_admin_level() {
        let tags: HashMap<&str, &str> = HashMap::from([
            ("boundary", "administrative"),
            ("name", "Testland"),
            ("admin_level", "2"),
        ]);
        assert_eq!(admin_relation_gate(&tags), Some(2));
    }

    #[test]
    fn admin_set_for_test_constructor_roundtrips() {
        let area = AdminArea {
            name: "Testland".to_string(),
            admin_level: 2,
            rings: vec![],
        };
        let place = PlaceNode {
            name: "Testville".to_string(),
            place: "town".to_string(),
            population: 42,
            lon: 1.0,
            lat: 2.0,
            id: 99,
        };
        let set = AdminSet::for_test(vec![area], vec![place]);
        assert_eq!(set.areas().len(), 1);
        assert_eq!(set.areas()[0].name, "Testland");
        assert_eq!(set.place_nodes().len(), 1);
        assert_eq!(set.place_nodes()[0].id, 99);
    }
}

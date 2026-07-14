//! Hierarchy index: resolve a `(lon, lat)` point to its enclosing
//! country/region/locality names.
//!
//! Built once from a [`boundaries::AdminSet`](crate::boundaries::AdminSet)
//! (admin-boundary polygons + `place=*` nodes), then queried millions of
//! times during pass 2 (once per POI). [`HierarchyIndex::resolve`] is
//! therefore optimized to be a pure read with minimal allocation:
//!
//! 1. Admin-area polygons are bucketed under every H3 resolution-5 cell that
//!    their exact polygon coverage touches (via `h3o::geom`'s `Tiler`, not a
//!    bounding-box approximation), so a query only has to point-in-polygon
//!    test against the handful of areas whose coverage happens to include
//!    the query's own res-5 cell (rather than every admin area in the
//!    extract).
//! 2. Place nodes (for the locality fallback) are bucketed the same way,
//!    keyed by their own single res-5 cell.
//!
//! Level mapping (see task brief):
//! - **country**: `admin_level == 2`.
//! - **region**: best (smallest-area) `admin_level == 4`; else `3`; else
//!   `5`.
//! - **locality**: best (smallest-area) `admin_level == 8`; else `7`; else
//!   `9`; else `10`; else the nearest qualifying place node (`place` in
//!   `city`/`town`/`village`/`hamlet`) within 10 km.
//!
//! "Best" among same-tier candidates means smallest polygon area (by
//! `geo::Area::unsigned_area`) — the most specific/local polygon wins over a
//! larger one that happens to also contain the point (e.g. a small
//! historical district nested inside a larger same-level area).

use fxhash::FxHashMap;
use geo::algorithm::area::Area;
use geo::algorithm::bounding_rect::BoundingRect;
use geo::algorithm::contains::Contains;
use geo::Point;
use h3o::geom::{ContainmentMode, TilerBuilder};
use h3o::{CellIndex, LatLng, Resolution};

use crate::boundaries::{AdminArea, AdminSet, PlaceNode};

/// Result of resolving a point against the hierarchy index: the enclosing
/// locality/region/country names, each `None` when nothing at that tier
/// could be resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parents {
    pub locality: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
}

/// H3 resolution used for the PIP/fallback spatial index. Res-5 cells have
/// an average edge length of ~9.85 km. Bucket cells are computed via
/// `h3o::geom::Tiler`'s exact polygon coverage (see [`bucket_cells_for_area`]),
/// not a hand-rolled lattice, so this is purely which resolution to bucket
/// at — it no longer influences a sampling step size.
const BUCKET_RESOLUTION: Resolution = Resolution::Five;

/// Maximum distance (meters) for the place-node locality fallback. This
/// bound is **inclusive**: a place node exactly `PLACE_FALLBACK_MAX_METERS`
/// away from the query point still qualifies (see the `dist > MAX` — not
/// `>=` — check in [`HierarchyIndex::nearest_place_within`]).
const PLACE_FALLBACK_MAX_METERS: f64 = 10_000.0;

/// Mean Earth radius (meters), matching the constant geo's (deprecated)
/// `HaversineDistance` impl uses (IUGG mean radius R1).
const EARTH_RADIUS_M: f64 = 6_371_008.771_4;

struct AreaEntry {
    admin_level: u8,
    name: String,
    /// Precomputed sum of unsigned areas of all rings, used as the
    /// "smallest wins" tie-breaker across same-tier candidates.
    area: f64,
    rings: Vec<geo::Polygon<f64>>,
}

struct PlaceEntry {
    name: String,
    lon: f64,
    lat: f64,
}

pub struct HierarchyIndex {
    areas: Vec<AreaEntry>,
    /// H3 res-5 cell -> indices into `areas` whose bbox covers that cell.
    area_buckets: FxHashMap<CellIndex, Vec<usize>>,
    qualifying_places: Vec<PlaceEntry>,
    /// H3 res-5 cell -> indices into `qualifying_places` located in that
    /// cell.
    place_buckets: FxHashMap<CellIndex, Vec<usize>>,
}

impl HierarchyIndex {
    /// Build the index from an already-loaded `AdminSet`.
    pub fn build(admin: &AdminSet) -> Self {
        let mut areas = Vec::with_capacity(admin.areas().len());
        let mut area_buckets: FxHashMap<CellIndex, Vec<usize>> = FxHashMap::default();

        for area in admin.areas() {
            let area_idx = areas.len();
            let total_area = polygon_area(area);
            for cell in bucket_cells_for_area(area) {
                area_buckets.entry(cell).or_default().push(area_idx);
            }
            areas.push(AreaEntry {
                admin_level: area.admin_level,
                name: area.name.clone(),
                area: total_area,
                rings: area.rings.clone(),
            });
        }

        let mut qualifying_places = Vec::new();
        let mut place_buckets: FxHashMap<CellIndex, Vec<usize>> = FxHashMap::default();

        for place in admin.place_nodes() {
            if !is_qualifying_locality_place(place) {
                continue;
            }
            let place_idx = qualifying_places.len();
            if let Some(cell) = cell_for_point(place.lon, place.lat) {
                place_buckets.entry(cell).or_default().push(place_idx);
            }
            qualifying_places.push(PlaceEntry {
                name: place.name.clone(),
                lon: place.lon,
                lat: place.lat,
            });
        }

        HierarchyIndex {
            areas,
            area_buckets,
            qualifying_places,
            place_buckets,
        }
    }

    /// Resolve a `(lon, lat)` point to its enclosing locality/region/country
    /// names. Pure read, no allocation beyond small candidate `Vec`s pulled
    /// from the bucket maps.
    pub fn resolve(&self, lon: f64, lat: f64) -> Parents {
        let point = Point::new(lon, lat);
        let cell = cell_for_point(lon, lat);

        let candidate_indices: &[usize] = cell
            .and_then(|c| self.area_buckets.get(&c))
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        let country = self.best_at_levels(candidate_indices, &point, &[2]);
        let region = self.best_at_levels(candidate_indices, &point, &[4, 3, 5]);
        let locality = self
            .best_at_levels(candidate_indices, &point, &[8, 7, 9, 10])
            .or_else(|| self.nearest_place_within(lon, lat, cell));

        Parents {
            locality,
            region,
            country,
        }
    }

    /// Among `candidate_indices`, find the smallest-area polygon match at
    /// the first level in `level_tiers` (in priority order) that has at
    /// least one containing polygon, and return its name.
    fn best_at_levels(
        &self,
        candidate_indices: &[usize],
        point: &Point<f64>,
        level_tiers: &[u8],
    ) -> Option<String> {
        for &level in level_tiers {
            let mut best: Option<(&str, f64)> = None;
            for &idx in candidate_indices {
                let entry = &self.areas[idx];
                if entry.admin_level != level {
                    continue;
                }
                if !area_contains(entry, point) {
                    continue;
                }
                match best {
                    Some((_, best_area)) if entry.area >= best_area => {}
                    _ => best = Some((entry.name.as_str(), entry.area)),
                }
            }
            if let Some((name, _)) = best {
                return Some(name.to_string());
            }
        }
        None
    }

    /// Locality fallback: nearest qualifying place node within
    /// `PLACE_FALLBACK_MAX_METERS`, searching the point's own res-5 cell
    /// plus its immediate k=1 ring (so places just across a bucket boundary
    /// are still found).
    fn nearest_place_within(&self, lon: f64, lat: f64, cell: Option<CellIndex>) -> Option<String> {
        let cell = cell?;

        let mut best: Option<(&str, f64)> = None;
        for neighbor in cell.grid_disk::<Vec<_>>(1) {
            let Some(indices) = self.place_buckets.get(&neighbor) else {
                continue;
            };
            for &idx in indices {
                let place = &self.qualifying_places[idx];
                let dist = haversine_meters(lon, lat, place.lon, place.lat);
                if dist > PLACE_FALLBACK_MAX_METERS {
                    continue;
                }
                match best {
                    Some((_, best_dist)) if dist >= best_dist => {}
                    _ => best = Some((place.name.as_str(), dist)),
                }
            }
        }
        best.map(|(name, _)| name.to_string())
    }
}

/// Sum of unsigned areas of every ring in an admin area (in the polygon's
/// native lon/lat "square degrees" units — only used for relative
/// smallest-wins comparisons within the same admin level, never as an
/// absolute physical area).
fn polygon_area(area: &AdminArea) -> f64 {
    area.rings.iter().map(|r| r.unsigned_area()).sum()
}

fn area_contains(entry: &AreaEntry, point: &Point<f64>) -> bool {
    entry.rings.iter().any(|ring| ring.contains(point))
}

fn is_qualifying_locality_place(place: &PlaceNode) -> bool {
    matches!(place.place.as_str(), "city" | "town" | "village" | "hamlet")
}

/// H3 res-5 cell containing `(lon, lat)`, or `None` if the coordinates are
/// out of range for `LatLng::new` (should not happen for real OSM data, but
/// PIP/fallback lookups degrade gracefully to "no match" rather than
/// panicking).
///
/// NOTE: `h3o::LatLng::new` takes `(lat, lng)` — the *opposite* order from
/// this crate's usual `(lon, lat)` tuples — so the two arguments are swapped
/// explicitly here.
fn cell_for_point(lon: f64, lat: f64) -> Option<CellIndex> {
    LatLng::new(lat, lon)
        .ok()
        .map(|ll| ll.to_cell(BUCKET_RESOLUTION))
}

/// Every res-5 H3 cell whose area intersects `area`'s rings, deduplicated.
///
/// Computed via `h3o::geom::Tiler` in [`ContainmentMode::Covers`] mode,
/// which is the mode that guarantees a *complete* coverage of the polygon:
/// every cell that the polygon boundary intersects is included, plus (per
/// `Covers`'s extension over plain `IntersectsBoundary`) the covering cell
/// in the edge case where the whole polygon is smaller than a single cell.
/// `ContainsCentroid`/`ContainsBoundary` are deliberately not used here —
/// both can leave real polygon area uncovered by any bucket cell, which is
/// exactly the class of bug this function replaces (see the regression test
/// `bucket_coverage_has_no_gaps_at_high_latitude`): the previous hand-rolled
/// bbox lattice walk, stepped at half of h3o's *global-average* res-5 edge
/// length converted via a flat 111 km/degree constant, could skip real
/// res-5 cells because a square lattice doesn't sample the same way a
/// hexagonal grid tiles — reproduced empirically at 60°N with a small
/// (~0.4° x 0.2°) bbox, where 2 real res-5 cells were never sampled.
///
/// If the Tiler rejects a ring as invalid geometry (e.g. degenerate/
/// self-intersecting), that single polygon falls back to the old bbox
/// lattice walk (see [`lattice_fallback_cells_for_bbox`]) rather than losing
/// the admin area from the index entirely; a warning is printed so the bad
/// geometry is visible without panicking pass-2 processing.
fn bucket_cells_for_area(area: &AdminArea) -> Vec<CellIndex> {
    let mut cells: Vec<CellIndex> = Vec::new();

    for ring in &area.rings {
        let mut tiler = TilerBuilder::new(BUCKET_RESOLUTION)
            .containment_mode(ContainmentMode::Covers)
            .build();
        match tiler.add(ring.clone()) {
            Ok(()) => cells.extend(tiler.into_coverage()),
            Err(err) => {
                eprintln!(
                    "hierarchy: H3 Tiler rejected a ring for admin area {:?} \
                     (admin_level={}): {err}; falling back to bbox lattice \
                     coverage for this ring",
                    area.name, area.admin_level
                );
                if let Some(rect) = ring.bounding_rect() {
                    cells.extend(lattice_fallback_cells_for_bbox(
                        rect.min().x,
                        rect.min().y,
                        rect.max().x,
                        rect.max().y,
                    ));
                }
            }
        }
    }

    cells.sort_unstable();
    cells.dedup();
    cells
}

/// Fallback bbox lattice walk, used only when `h3o::geom::Tiler` rejects a
/// ring as invalid geometry (see [`bucket_cells_for_area`]).
///
/// NOTE: this walk uses a flat 111 km/degree constant to convert the res-5
/// step size to degrees, which does *not* account for longitude compression
/// at higher latitudes (1 degree of longitude is ~111 km at the equator but
/// shrinks toward 0 near the poles). That only makes the fallback
/// *over*-sample at high latitudes (more redundant, deduped lattice points,
/// never fewer) — it does not reintroduce the square-lattice-vs-hex-tiling
/// undersampling bug this module was fixed for, since this path is now only
/// reached for a handful of geometrically-invalid rings rather than being
/// the primary coverage strategy.
fn lattice_fallback_cells_for_bbox(
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
) -> Vec<CellIndex> {
    let mut cells: Vec<CellIndex> = Vec::new();
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return cells;
    }

    let half_edge_km = BUCKET_RESOLUTION.edge_length_km() / 2.0;
    let step_deg = (half_edge_km / 111.0).max(1e-6);

    let mut y = min_y;
    loop {
        let mut x = min_x;
        loop {
            if let Some(cell) = cell_for_point(x, y) {
                cells.push(cell);
            }
            if x >= max_x {
                break;
            }
            x = (x + step_deg).min(max_x);
        }
        if y >= max_y {
            break;
        }
        y = (y + step_deg).min(max_y);
    }

    cells
}

/// Great-circle distance in meters between two `(lon, lat)` points, via the
/// haversine formula (mean Earth radius, matching geo's own haversine
/// constant). Written standalone rather than via `geo`'s
/// `HaversineDistance` trait, which is deprecated as of geo 0.29 in favor of
/// the more general (and heavier) `Distance`/`Haversine` API.
fn haversine_meters(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    let (lat1, lat2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = lat2 - lat1;
    let dlon = (lon2 - lon1).to_radians();

    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    EARTH_RADIUS_M * c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundaries::{AdminArea, AdminSet, PlaceNode};

    fn square(name: &str, level: u8, x0: f64, y0: f64, x1: f64, y1: f64) -> AdminArea {
        let ring = geo::Polygon::new(
            geo::LineString::from(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]),
            vec![],
        );
        AdminArea {
            name: name.into(),
            admin_level: level,
            rings: vec![ring],
        }
    }

    #[test]
    fn resolve_uses_pip_then_place_fallback() {
        let admin = AdminSet::for_test(
            vec![
                square("Testland", 2, 0., 0., 1., 1.),
                square("Mid", 4, 0., 0., 1., 1.),
                square("Town", 8, 0.2, 0.2, 0.4, 0.4),
            ],
            vec![PlaceNode {
                name: "FallbackVille".into(),
                place: "village".into(),
                population: 100,
                lon: 0.9,
                lat: 0.9,
                id: 1,
            }],
        );
        let idx = HierarchyIndex::build(&admin);
        let inside = idx.resolve(0.3, 0.3);
        assert_eq!(inside.locality.as_deref(), Some("Town"));
        assert_eq!(inside.region.as_deref(), Some("Mid"));
        assert_eq!(inside.country.as_deref(), Some("Testland"));
        let fallback = idx.resolve(0.9, 0.9); // no level-8 polygon here
        assert_eq!(fallback.locality.as_deref(), Some("FallbackVille"));
    }

    #[test]
    fn resolve_outside_everything_returns_all_none() {
        let admin = AdminSet::for_test(vec![square("Testland", 2, 0., 0., 1., 1.)], vec![]);
        let idx = HierarchyIndex::build(&admin);
        let outside = idx.resolve(50.0, 50.0);
        assert_eq!(outside.country, None);
        assert_eq!(outside.region, None);
        assert_eq!(outside.locality, None);
    }

    #[test]
    fn region_falls_through_level_tiers_in_order() {
        // No level-4 region, but a level-3 one exists: region should fall
        // through to it.
        let admin = AdminSet::for_test(
            vec![
                square("Testland", 2, 0., 0., 1., 1.),
                square("Level3Region", 3, 0., 0., 1., 1.),
            ],
            vec![],
        );
        let idx = HierarchyIndex::build(&admin);
        let result = idx.resolve(0.5, 0.5);
        assert_eq!(result.region.as_deref(), Some("Level3Region"));
    }

    #[test]
    fn region_prefers_level_4_over_level_3_and_5() {
        let admin = AdminSet::for_test(
            vec![
                square("Level3Region", 3, 0., 0., 1., 1.),
                square("Level4Region", 4, 0., 0., 1., 1.),
                square("Level5Region", 5, 0., 0., 1., 1.),
            ],
            vec![],
        );
        let idx = HierarchyIndex::build(&admin);
        let result = idx.resolve(0.5, 0.5);
        assert_eq!(result.region.as_deref(), Some("Level4Region"));
    }

    #[test]
    fn best_at_level_picks_smallest_area_among_same_tier_candidates() {
        // Two level-8 polygons both contain the point; the smaller one
        // should win (more specific/local).
        let big = square("BigTown", 8, 0., 0., 1., 1.);
        let small = square("SmallTown", 8, 0.4, 0.4, 0.6, 0.6);
        let admin = AdminSet::for_test(vec![big, small], vec![]);
        let idx = HierarchyIndex::build(&admin);
        let result = idx.resolve(0.5, 0.5);
        assert_eq!(result.locality.as_deref(), Some("SmallTown"));
    }

    #[test]
    fn place_fallback_ignores_non_qualifying_place_types() {
        // A "suburb" place node should NOT satisfy the locality fallback.
        let admin = AdminSet::for_test(
            vec![square("Testland", 2, 0., 0., 1., 1.)],
            vec![PlaceNode {
                name: "SomeSuburb".into(),
                place: "suburb".into(),
                population: 10,
                lon: 0.5,
                lat: 0.5,
                id: 1,
            }],
        );
        let idx = HierarchyIndex::build(&admin);
        let result = idx.resolve(0.5, 0.5);
        assert_eq!(result.locality, None);
    }

    #[test]
    fn place_fallback_respects_10km_radius() {
        // Place node is far enough away that it should NOT be picked up.
        let admin = AdminSet::for_test(
            vec![],
            vec![PlaceNode {
                name: "FarAway".into(),
                place: "town".into(),
                population: 10,
                lon: 10.0,
                lat: 10.0,
                id: 1,
            }],
        );
        let idx = HierarchyIndex::build(&admin);
        let result = idx.resolve(0.0, 0.0);
        assert_eq!(result.locality, None);
    }

    #[test]
    fn place_fallback_picks_nearest_of_multiple_candidates() {
        let admin = AdminSet::for_test(
            vec![],
            vec![
                PlaceNode {
                    name: "Nearer".into(),
                    place: "village".into(),
                    population: 10,
                    lon: 0.01,
                    lat: 0.01,
                    id: 1,
                },
                PlaceNode {
                    name: "Farther".into(),
                    place: "village".into(),
                    population: 10,
                    lon: 0.05,
                    lat: 0.05,
                    id: 2,
                },
            ],
        );
        let idx = HierarchyIndex::build(&admin);
        let result = idx.resolve(0.0, 0.0);
        assert_eq!(result.locality.as_deref(), Some("Nearer"));
    }

    #[test]
    fn bucket_coverage_has_no_gaps_at_high_latitude() {
        // Regression test for a reviewed CRITICAL finding: the old bbox
        // lattice walk (stepped at half of h3o's *global-average* res-5
        // edge length, using a flat 111 km/degree constant) could skip real
        // res-5 cells because a square lattice doesn't tile the same way a
        // hex grid does — the gap is a lattice-vs-hex sampling artifact, not
        // (primarily) a latitude/longitude-compression artifact. It was
        // empirically reproduced at 60°N with a small (~0.4° x 0.2°) bbox,
        // where 2 real res-5 cells were never sampled, so points inside
        // those cells silently failed to resolve the enclosing locality.
        //
        // This test builds a small polygon in exactly that regime and then
        // *densely* samples its interior (finer than any single H3 res-5
        // cell), asserting every sample point resolves. A single skipped
        // cell anywhere under the polygon's interior will surface as a
        // `None` locality on at least one of the ~800 sample points.
        let admin = AdminSet::for_test(
            vec![square("Northland", 8, 5.1, 60.0, 5.5, 60.2)],
            vec![],
        );
        let idx = HierarchyIndex::build(&admin);

        let step = 0.01;
        let mut checked = 0;
        let mut lon = 5.1 + step;
        while lon < 5.5 {
            let mut lat = 60.0 + step;
            while lat < 60.2 {
                let result = idx.resolve(lon, lat);
                assert_eq!(
                    result.locality.as_deref(),
                    Some("Northland"),
                    "gap in H3 bucket coverage at ({lon}, {lat}): point inside \
                     the polygon failed to resolve"
                );
                checked += 1;
                lat += step;
            }
            lon += step;
        }
        // Sanity check: make sure the dense grid actually exercised a
        // meaningful number of sample points (guards against a future edit
        // accidentally shrinking the loop to a no-op).
        assert!(
            checked >= 700,
            "expected a dense sample grid (~800 points), only checked {checked}"
        );
    }

    #[test]
    fn haversine_meters_known_distance_new_york_to_london() {
        // Same fixture pair used in geo's own HaversineDistance doctest.
        let dist = haversine_meters(-74.006, 40.7128, -0.1278, 51.5074);
        assert!(
            (dist - 5_570_230.0).abs() < 1000.0,
            "expected ~5,570,230 m, got {dist}"
        );
    }
}

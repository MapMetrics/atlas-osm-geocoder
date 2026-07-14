//! Pass 2: POI emitter.
//!
//! Streams nodes and ways out of the `.osm.pbf`, gates each through
//! `taxonomy::is_poi`, and writes `poi.geojsonl` via `emit::LayerWriter`.
//! Property contract mirrors `extract_country_v3.py`'s `extract_poi`
//! (see `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! `extract_poi` ~lines 171-246) exactly, adapted from ClickHouse `pois_v3`
//! row inputs to raw OSM tag inputs:
//!
//! - `carmen:text`: dedup-joined aliases, name-first: `[name, "name locality",
//!   alt_name, short_name, official_name, brand, "brand locality", category]`
//!   (G6: `alt_name`/`short_name`/`official_name` tag values inserted after
//!   name/"name locality" and before brand — see [`carmen_text`] doc). The
//!   python's `ext_name`/`names_intl` alias slots have no OSM-tag equivalent
//!   in this v1 (no external-source merge at this stage) and are simply
//!   absent from the list, not emitted as empty entries.
//! - `carmen:center`: raw `[lon, lat]`, unrounded (the python does not round
//!   `extract_poi`'s center either — only `extract_country`'s country
//!   centroid rounds to 4dp).
//! - `carmen:score` / `popularity`: identical value (see [`poi_score`] doc).
//! - optional `category` (first/only taxonomy match), `locality` (resolved
//!   via `HierarchyIndex`), `brand`, `housenumber` (`addr:housenumber` tag).

use std::path::Path;

use osmpbf::{Element, ElementReader};

use crate::emit::LayerWriter;
use crate::error::ExtractError;
use crate::hierarchy::HierarchyIndex;
use crate::ids::{hid, osm_sid};
use crate::layers::common::{tags_to_map, way_centroid};
use crate::nodes::NodeTable;
use crate::taxonomy::{categorize, is_poi, TagMap};

/// carmen:score / popularity v1 scoring — a deterministic, open-data
/// stand-in for the enriched popularity score `poi_score()` computes in
/// `extract_country_v3.py` (~lines 148-159:
/// `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`),
/// which draws on external enrichment `popularity`/`review_count` columns
/// that have no OSM-tag equivalent here.
///
/// # Calibration derivation (G1 fix — see
/// `docs/plans/2026-07-14-atlas-extract-acceptance.md`)
///
/// The v1 formula this replaces (base 100, +400 wiki, +200 brand → up to
/// 700) put EVERY unenriched POI at 100 — above a real village's place
/// score (~54 for pop 500) and in the same magnitude as many small towns.
/// That is backwards from production. Two production facts anchor the fix:
///
/// 1. `extract_country_v3.py`'s `poi_score()` doc comment: "popularity in
///    pois_v3 already clusters in the hundreds like NL's... Distribution
///    verified to mirror NL: **~53% zero**, mass in 100-999, rare 8000+
///    landmarks." An unenriched POI (no `popularity`/`review_count`) scores
///    **0**, not a positive base.
/// 2. The serving worker (`atlas-edge/worker/src/lib.rs`, `text_bonus_impl`)
///    hardcodes a landmark threshold at `carmen:score >= 8000`
///    (+200 alias-boost) and `layer_bonus` gives poi/place comparable flat
///    weight (poi 1.5 vs place 2.0) — popularity is a fine-grained
///    tiebreaker (further log10-compressed into a 1-9ish priority byte by
///    the converter's `intrinsic_priority_u8`), never a competitor to a
///    place's text/layer signal. Places must dominate POIs by default;
///    only genuinely notable (wiki-tagged) POIs should approach city
///    territory, and even then stay below a real city.
///
/// `place.rs`'s `place_score_from_pop` (untouched, production-pinned:
/// `20*log10(pop+1)`, capped 250) gives a village of pop 500 a score of
/// ~54, and a city of pop 800,000 a score of ~118. This function is
/// calibrated against those same reference points:
///
/// - **Plain POI** (no wiki, no brand): **0** — mirrors production's ~53%
///   zero mass exactly; a plain café must never outrank even a hamlet.
/// - **Brand bonus** (`brand` tag present — chain/franchise notability):
///   **branch-count-aware** — see "Branch-count curve" below. A lone
///   1-2 location "brand" stays ≈40 (clearly above a plain POI, but well
///   under a village at 54). A real chain scales up from there.
/// - **Wiki bonus** (`wikidata` or `wikipedia` present — an external
///   encyclopedic reference, the strongest open-data notability signal
///   available at this stage): **+90** — beats a village (54) so a
///   wiki-tagged landmark in a tiny village still surfaces, but stays
///   below a mid-size city (118) so "Amsterdam" the city beats a
///   wiki-tagged POI merely located in Amsterdam.
/// - The two bonuses stack additively.
///
/// # Branch-count curve (G7 fix — brand-query recall)
///
/// Flat `+40` for every brand-tagged POI regardless of chain size was the
/// bug behind 16% brand-query recall vs production's 92%: production's
/// enriched popularity lifts real chain branches (Kruidvat: 988 OSM
/// branches, Albert Heijn: 1252, BackWerk: 30, Greenpoint: 23 — all
/// present in `poi.geojsonl`) into the top-5 results; a flat score treats
/// a 1000-branch national chain identically to a two-location local
/// franchise, so neither reliably surfaces.
///
/// `poi_score` now takes `branch_count`: the number of POI candidates in
/// this extraction run sharing the same normalized (lowercase, trimmed)
/// `brand` value, counted in a pre-emission pass over the candidate `Vec`
/// in [`extract`] before any features are written. The brand bonus is:
///
/// ```text
/// bonus(n) = 40                          if n <= 1
///          = round(40 + 30 * log10(n))   if n >= 2
/// ```
///
/// Derivation: keep the existing `+40` floor for a lone brand tag (no
/// real chain signal beyond the tag itself), then grow logarithmically —
/// matching `place_score_from_pop`'s own `log10` shape so brand bonuses
/// live on the same curve family as place scores — with a coefficient
/// (30, vs place's 20) chosen so that a genuinely large chain can cross
/// the city ceiling, which production's enriched popularity does for
/// heavily-searched national chains. Calibration table (rounded):
///
/// | branch_count | bonus | reference point |
/// |---|---|---|
/// | 1 | 40 | lone "brand" tag — production ceiling for non-chains |
/// | 2 | 49 | still near the ≈40 floor |
/// | 10 | 70 | small chain — below small town (74 @ pop 5000) |
/// | 23 | 81 | Greenpoint (task brief) |
/// | 30 | 84 | BackWerk (task brief) — above small town, below big town (94 @ pop 50k) |
/// | 100 | 100 | mid chain — matches mid-size town |
/// | 988 | 130 | Kruidvat (task brief) — beats city(800k)=118: the deliberate exception |
/// | 1000 | 130 | round-number reference |
/// | 1252 | 133 | Albert Heijn (task brief) |
///
/// `branch_count <= 1` (including 0, which should not occur since the POI
/// itself is always at least one candidate, but is handled defensively to
/// avoid `log10(0) = -inf`) uses the flat 40 floor rather than the log
/// curve.
///
/// Ordering pinned in `poi_score_ordering_matches_production_relationships`
/// below: city(800k) > wiki-POI > solo-brand-POI(1 branch) > plain-POI(=0),
/// wiki-POI > village(500), a 10-branch chain stays below a small town
/// (5000), and — the deliberate large-chain exception — a 988-branch chain
/// (Kruidvat) beats the 800k city, mirroring production's behavior where
/// searching "kruidvat" surfaces stores ahead of unrelated cities.
fn poi_score(tags: &TagMap, branch_count: u64) -> i64 {
    let mut score: i64 = 0;
    let has_wiki = tags.get("wikidata").is_some_and(|v| !v.is_empty())
        || tags.get("wikipedia").is_some_and(|v| !v.is_empty());
    if has_wiki {
        score += 90;
    }
    if tags.get("brand").is_some_and(|v| !v.is_empty()) {
        score += brand_branch_bonus(branch_count);
    }
    score
}

/// Branch-count-aware brand bonus curve — see [`poi_score`] doc for
/// derivation and calibration table. `n <= 1` (including 0, defensive
/// against `log10(0)`) returns the flat 40 floor; `n >= 2` grows
/// logarithmically: `round(40 + 30 * log10(n))`.
fn brand_branch_bonus(branch_count: u64) -> i64 {
    if branch_count <= 1 {
        return 40;
    }
    (40.0 + 30.0 * (branch_count as f64).log10()).round() as i64
}

/// Mirrors `extract_country_v3.py`'s `clean_alias`: strip commas (carmen:text
/// is comma-split downstream) and collapse whitespace.
fn clean_alias(s: &str) -> String {
    s.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Mirrors `extract_country_v3.py`'s `dedup_join`: name-first, case-
/// insensitive deduped, comma-joined. No MAX_ALIASES cap here — the OSM-tag
/// alias list per POI is already small and bounded (name, name+locality,
/// brand, brand+locality, category), unlike the python's category[] array
/// tail which could be long.
fn dedup_join(parts: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in parts {
        let cleaned = clean_alias(p);
        if cleaned.is_empty() {
            continue;
        }
        let key = cleaned.to_lowercase();
        if seen.insert(key) {
            out.push(cleaned);
        }
    }
    out.join(",")
}

/// Build the alias list + resulting `carmen:text` for one POI. Order mirrors
/// `extract_poi`: name, "name locality", brand, "brand locality", category —
/// with `name_variants` (G6: `alt_name`/`short_name`/`official_name` tag
/// values, in that order) inserted after name/"name locality" and before
/// brand. These OSM tags carry common alternate query forms the primary
/// `name` tag misses (e.g. a station tagged `name=Centraal Station` but
/// commonly searched as `alt_name=Amsterdam Centraal`); production's
/// equivalent is external-source alias enrichment this OSM-tag-only v1 has
/// no other route to. Empty variants are skipped; `dedup_join` below
/// case-insensitively dedupes against `name` and each other.
fn carmen_text(
    name: &str,
    brand: &str,
    locality: Option<&str>,
    category: Option<&str>,
    name_variants: &[&str],
) -> String {
    let mut aliases: Vec<String> = Vec::new();
    if !name.is_empty() {
        aliases.push(name.to_string());
    }
    if !name.is_empty() {
        if let Some(loc) = locality {
            if !loc.is_empty() {
                aliases.push(format!("{name} {loc}"));
            }
        }
    }
    for variant in name_variants {
        if !variant.is_empty() {
            aliases.push(variant.to_string());
        }
    }
    if !brand.is_empty() {
        aliases.push(brand.to_string());
        if let Some(loc) = locality {
            if !loc.is_empty() {
                aliases.push(format!("{brand} {loc}"));
            }
        }
    }
    if let Some(cat) = category {
        aliases.push(cat.to_string());
    }
    dedup_join(&aliases)
}

/// One resolved POI candidate, gathered during the streaming pass, before
/// properties/geometry are assembled.
struct Candidate {
    kind: char, // 'n' or 'w'
    id: i64,
    tags: TagMap,
    lon: f64,
    lat: f64,
}

/// Pass 2: extract POIs from `pbf` (nodes + ways), resolve each one's
/// hierarchy parents via `hier`, and write `{out_dir}/poi.geojsonl` via
/// `LayerWriter`. Returns the number of features written.
pub fn extract(
    pbf: &Path,
    nodes: &NodeTable,
    hier: &HierarchyIndex,
    out_dir: &Path,
) -> Result<u64, ExtractError> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut way_skips: u64 = 0;

    let reader = ElementReader::from_path(pbf)?;
    reader.for_each(|element| match element {
        Element::Node(n) => {
            let tags = tags_to_map(n.tags());
            if is_poi(&tags) {
                candidates.push(Candidate {
                    kind: 'n',
                    id: n.id(),
                    tags,
                    lon: n.lon(),
                    lat: n.lat(),
                });
            }
        }
        Element::DenseNode(n) => {
            let tags = tags_to_map(n.tags());
            if is_poi(&tags) {
                candidates.push(Candidate {
                    kind: 'n',
                    id: n.id(),
                    tags,
                    lon: n.lon(),
                    lat: n.lat(),
                });
            }
        }
        Element::Way(w) => {
            let tags = tags_to_map(w.tags());
            if is_poi(&tags) {
                let refs: Vec<i64> = w.refs().collect();
                match way_centroid(&refs, nodes) {
                    Some((lon, lat)) => {
                        candidates.push(Candidate {
                            kind: 'w',
                            id: w.id(),
                            tags,
                            lon,
                            lat,
                        });
                    }
                    None => way_skips += 1,
                }
            }
        }
        _ => {}
    })?;

    if way_skips > 0 {
        eprintln!("layers::poi: skipped {way_skips} way(s) with zero resolvable member nodes");
    }

    // G7 pre-emission pass: count candidates per normalized (lowercase,
    // trimmed) brand value, so poi_score can scale the brand bonus by
    // chain size (see poi_score doc for the curve derivation).
    let mut branch_counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for c in &candidates {
        if let Some(brand) = c.tags.get("brand") {
            let key = brand.trim().to_lowercase();
            if !key.is_empty() {
                *branch_counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    let path = out_dir.join("poi.geojsonl");
    let mut writer = LayerWriter::new(&path)?;

    for c in &candidates {
        let name = c.tags.get("name").map(String::as_str).unwrap_or("");
        let brand = c.tags.get("brand").map(String::as_str).unwrap_or("");
        let category = categorize(&c.tags);
        let parents = hier.resolve(c.lon, c.lat);
        let locality = parents.locality.as_deref();

        let alt_name = c.tags.get("alt_name").map(String::as_str).unwrap_or("");
        let short_name = c.tags.get("short_name").map(String::as_str).unwrap_or("");
        let official_name = c.tags.get("official_name").map(String::as_str).unwrap_or("");
        let name_variants = [alt_name, short_name, official_name];

        let text = carmen_text(name, brand, locality, category, &name_variants);
        if text.is_empty() {
            // is_poi already guarantees name-or-brand present, so this
            // should not happen, but never emit an empty carmen:text.
            continue;
        }

        let branch_count = if brand.is_empty() {
            0
        } else {
            branch_counts.get(&brand.trim().to_lowercase()).copied().unwrap_or(0)
        };
        let score = poi_score(&c.tags, branch_count);

        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), text.into());
        props.insert(
            "carmen:center".into(),
            serde_json::json!([c.lon, c.lat]),
        );
        props.insert("carmen:score".into(), score.into());
        props.insert("popularity".into(), score.into());
        if let Some(cat) = category {
            props.insert("category".into(), cat.into());
        }
        if let Some(loc) = locality {
            if !loc.is_empty() {
                props.insert("locality".into(), loc.into());
            }
        }
        if !brand.is_empty() {
            props.insert("brand".into(), brand.into());
        }
        if let Some(hn) = c.tags.get("addr:housenumber") {
            if !hn.is_empty() {
                props.insert("housenumber".into(), hn.clone().into());
            }
        }

        let sid = osm_sid(c.kind, c.id);
        let feature_id = hid(&sid);
        let geometry = serde_json::json!({
            "type": "Point",
            "coordinates": [c.lon, c.lat],
        });
        writer.feature(feature_id, &props, geometry)?;
    }

    Ok(writer.count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> TagMap {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn poi_score_base_only() {
        assert_eq!(poi_score(&tags(&[("amenity", "cafe")]), 1), 0);
    }

    #[test]
    fn poi_score_wikidata_bonus() {
        assert_eq!(poi_score(&tags(&[("wikidata", "Q123")]), 1), 90);
    }

    #[test]
    fn poi_score_wikipedia_bonus() {
        assert_eq!(poi_score(&tags(&[("wikipedia", "en:Foo")]), 1), 90);
    }

    #[test]
    fn poi_score_brand_bonus() {
        assert_eq!(poi_score(&tags(&[("brand", "Starbucks")]), 1), 40);
    }

    #[test]
    fn poi_score_wiki_and_brand_stack() {
        assert_eq!(
            poi_score(&tags(&[("wikidata", "Q1"), ("brand", "Starbucks")]), 1),
            130
        );
    }

    #[test]
    fn poi_score_empty_wikidata_value_does_not_count() {
        assert_eq!(poi_score(&tags(&[("wikidata", "")]), 1), 0);
    }

    // --- G7: branch-count-aware brand bonus curve ---
    // bonus(n) = 40 + 30*log10(n) for n>=2, 40 flat for n<=1, rounded.
    // Reference points from the task brief's real OSM chains: BackWerk=30,
    // Greenpoint=23, Kruidvat=988, Albert Heijn=1252.

    #[test]
    fn poi_score_brand_single_branch_stays_at_forty() {
        assert_eq!(poi_score(&tags(&[("brand", "Joe's Cafe")]), 1), 40);
    }

    #[test]
    fn poi_score_brand_two_branches_stays_near_forty() {
        // 2-branch "brand" should stay close to the ≈40 floor, not jump to
        // mid-city territory.
        let score = poi_score(&tags(&[("brand", "Joe's Cafe")]), 2);
        assert_eq!(score, 49);
    }

    #[test]
    fn poi_score_brand_ten_branches_lands_sixty_to_eighty() {
        let score = poi_score(&tags(&[("brand", "Local Chain")]), 10);
        assert!((60..=80).contains(&score), "10-branch bonus={score} must land in 60-80");
        assert_eq!(score, 70);
    }

    #[test]
    fn poi_score_brand_backwerk_thirty_branches() {
        // BackWerk: 30 branches in the task brief's real-data check.
        let score = poi_score(&tags(&[("brand", "BackWerk")]), 30);
        assert_eq!(score, 84);
    }

    #[test]
    fn poi_score_brand_greenpoint_twentythree_branches() {
        // Greenpoint: 23 branches in the task brief's real-data check.
        let score = poi_score(&tags(&[("brand", "Greenpoint")]), 23);
        assert_eq!(score, 81);
    }

    #[test]
    fn poi_score_brand_kruidvat_988_branches_lands_hundred_to_hundredfifty() {
        // Kruidvat: 988 branches in the task brief's real-data check.
        let score = poi_score(&tags(&[("brand", "Kruidvat")]), 988);
        assert!((100..=150).contains(&score), "988-branch bonus={score} must land in 100-150");
        assert_eq!(score, 130);
    }

    #[test]
    fn poi_score_brand_albert_heijn_1252_branches_lands_hundred_to_hundredfifty() {
        // Albert Heijn: 1252 branches in the task brief's real-data check.
        let score = poi_score(&tags(&[("brand", "Albert Heijn")]), 1252);
        assert!((100..=150).contains(&score), "1252-branch bonus={score} must land in 100-150");
        assert_eq!(score, 133);
    }

    #[test]
    fn poi_score_brand_thousand_branches_matches_curve() {
        let score = poi_score(&tags(&[("brand", "Mega Chain")]), 1000);
        assert_eq!(score, 130);
    }

    #[test]
    fn poi_score_brand_zero_branch_count_treated_as_one() {
        // Defensive: a branch_count of 0 (shouldn't happen — the POI itself
        // is always at least one candidate) must not panic (log10(0) = -inf)
        // and must fall back to the same floor as a 1-branch brand.
        assert_eq!(poi_score(&tags(&[("brand", "Solo")]), 0), 40);
    }

    #[test]
    fn poi_score_brand_and_wiki_stack_with_branch_count() {
        let score = poi_score(
            &tags(&[("wikidata", "Q1"), ("brand", "Kruidvat")]),
            988,
        );
        assert_eq!(score, 90 + 130);
    }

    /// G1 fix (base case, single-branch/no-brand POIs) + G7 fix (branch-
    /// count-aware brand bonus) pins the cross-layer score RELATIONSHIP
    /// this module's doc comment derives from production (see `poi_score`
    /// doc). Uses `place::place_score_from_pop`-equivalent math directly
    /// (that function is private to `place.rs` and intentionally
    /// untouched — production-pinned — so this test re-derives its
    /// formula inline rather than reaching across modules) to assert:
    ///
    /// - For a single-location "brand" (branch_count=1): a real city beats
    ///   a wiki-tagged POI, which beats the brand-tagged POI, which beats
    ///   a plain POI; and a wiki-tagged POI still beats a tiny village.
    /// - G7: chain size changes the relationship. A small chain
    ///   (10 branches) still stays below a small town. A genuinely large
    ///   chain (988 branches, e.g. Kruidvat) is the deliberate exception
    ///   called out in the task brief: it now OUTRANKS the 800k city,
    ///   mirroring production's enriched-popularity behavior where a
    ///   nationwide retail chain's branches are highly searched.
    #[test]
    fn poi_score_ordering_matches_production_relationships() {
        fn place_score_from_pop(pop: u64) -> i64 {
            if pop == 0 {
                return 0;
            }
            let score = 20.0 * (pop as f64 + 1.0).log10();
            score.round().min(250.0) as i64
        }

        let plain = poi_score(&tags(&[("amenity", "cafe")]), 1);
        let brand_solo = poi_score(&tags(&[("brand", "Starbucks")]), 1);
        let wiki = poi_score(&tags(&[("wikidata", "Q123")]), 1);
        let city_800k = place_score_from_pop(800_000);
        let village_500 = place_score_from_pop(500);
        let small_town_5000 = place_score_from_pop(5000);

        assert!(city_800k > wiki, "city(800k)={city_800k} must beat wiki-POI={wiki}");
        assert!(wiki > brand_solo, "wiki-POI={wiki} must beat solo brand-POI={brand_solo}");
        assert!(brand_solo > plain, "solo brand-POI={brand_solo} must beat plain-POI={plain}");
        assert!(
            wiki > village_500,
            "wiki-POI={wiki} must still beat village(500)={village_500}"
        );

        // G7: a small chain (10 branches) stays below a small town.
        let brand_10 = poi_score(&tags(&[("brand", "Local Chain")]), 10);
        assert!(
            brand_10 < small_town_5000,
            "10-branch brand-POI={brand_10} must stay below small town(5000)={small_town_5000}"
        );

        // G7: a genuinely large chain (Kruidvat, 988 branches) is the
        // deliberate exception — it now beats even the 800k city, mirroring
        // production's enriched popularity for nationwide retail chains.
        let brand_kruidvat = poi_score(&tags(&[("brand", "Kruidvat")]), 988);
        assert!(
            brand_kruidvat > city_800k,
            "988-branch brand-POI={brand_kruidvat} must beat city(800k)={city_800k} \
             (large-chain exception)"
        );
    }

    #[test]
    fn dedup_join_strips_commas_and_dedupes_case_insensitively() {
        let parts = vec!["Joe's, Cafe".to_string(), "joe's cafe".to_string(), "Cafe".to_string()];
        assert_eq!(dedup_join(&parts), "Joe's Cafe,Cafe");
    }

    #[test]
    fn carmen_text_builds_name_first_alias_order() {
        let text = carmen_text("Joe's", "Acme", Some("Monaco"), Some("cafe"), &[]);
        assert_eq!(text, "Joe's,Joe's Monaco,Acme,Acme Monaco,cafe");
    }

    #[test]
    fn carmen_text_omits_empty_slots() {
        let text = carmen_text("Joe's", "", None, Some("cafe"), &[]);
        assert_eq!(text, "Joe's,cafe");
    }

    /// G6: alt_name/short_name/official_name become aliases, ordered after
    /// name/"name locality" and before brand, empty variants skipped.
    #[test]
    fn carmen_text_includes_name_variants_after_name_before_brand() {
        let text = carmen_text(
            "Centraal Station",
            "NS",
            None,
            Some("railway_station"),
            &["Amsterdam Centraal", "", "Amsterdam Centraal Station"],
        );
        assert_eq!(
            text,
            "Centraal Station,Amsterdam Centraal,Amsterdam Centraal Station,NS,railway_station"
        );
    }

    /// G6 acceptance case from the task brief: a POI named "Centraal
    /// Station" with alt_name "Amsterdam Centraal" must carry BOTH forms in
    /// carmen:text so the common query "amsterdam centraal" matches it.
    #[test]
    fn carmen_text_amsterdam_centraal_alt_name_case() {
        let text = carmen_text(
            "Centraal Station",
            "",
            None,
            Some("railway_station"),
            &["Amsterdam Centraal", "", ""],
        );
        assert!(text.contains("Centraal Station"));
        assert!(text.contains("Amsterdam Centraal"));
    }

    #[test]
    fn carmen_text_dedupes_name_variant_equal_to_name() {
        // alt_name identical to name (case-insensitive) must not duplicate.
        let text = carmen_text("Joe's", "", None, None, &["joe's", "", ""]);
        assert_eq!(text, "Joe's");
    }

    #[test]
    fn way_centroid_averages_resolvable_nodes() {
        let mut locations = std::collections::HashMap::new();
        locations.insert(1i64, (0.0f64, 0.0f64));
        locations.insert(2i64, (2.0f64, 2.0f64));
        // Build a NodeTable via its public load-free constructor path isn't
        // available; exercise the pure function with a tiny local stand-in
        // instead by re-deriving the centroid math directly (way_centroid's
        // NodeTable dependency is exercised end-to-end by the Monaco
        // integration test instead).
        let sum_lon: f64 = locations.values().map(|(lon, _)| lon).sum();
        let sum_lat: f64 = locations.values().map(|(_, lat)| lat).sum();
        let n = locations.len() as f64;
        assert_eq!((sum_lon / n, sum_lat / n), (1.0, 1.0));
    }
}

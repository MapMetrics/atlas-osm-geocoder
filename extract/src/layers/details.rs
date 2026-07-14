//! Pass 2: `/details` sidecar source emitter.
//!
//! Streams nodes and ways out of the `.osm.pbf`, gates each candidate
//! through `taxonomy::is_poi` (the SAME gate `layers::poi` uses, so every
//! emitted id is guaranteed to exist in `poi.geojsonl`), and writes
//! `poi_details.jsonl` — one bare JSON object per line (NOT a GeoJSON
//! Feature) — via a plain `serde_json` writer. Field contract mirrors
//! `extract_country_v3.py`'s `extract_poi_details` (see
//! `/Volumes/T7/osm.pbfconverter/atlas-edge/scripts/extract_country_v3.py`,
//! ~lines 249-324) and the sidecar design doc
//! (`/Volumes/T7/osm.pbfconverter/atlas-edge/docs/superpowers/specs/2026-07-14-details-sidecar-design.md`),
//! adapted from ClickHouse `pois_v3` enrichment columns (which have no
//! OSM-tag equivalent — `rating`/`review_count`/`price_range` are Google
//! Places enrichment outputs this crate never produces) to raw OSM tags:
//!
//! - `id`: `hid(osm_sid(kind, id))` — the exact SAME id the `layers::poi`
//!   emitter computes for the identical element (same `kind`/`id` pair),
//!   so `/details` ids join 1:1 against `poi.geojsonl` ids. This is the
//!   gate the Monaco integration test asserts directly (every details id
//!   is a member of the poi.geojsonl id set).
//! - `name`: the `name` tag, carried along for display but — per the
//!   python's doc comment (~line 257: "name is carried along for display
//!   but does NOT count toward the non-empty gate") — NOT one of the
//!   fields that satisfies the "at least one detail field" requirement
//!   below.
//! - `hours`: the `opening_hours` tag verbatim (python's `opening_hours`
//!   column, sourced from external enrichment there; here directly off the
//!   OSM tag of the same name).
//! - `phone`: `phone` + `contact:phone` tags, order-preserving deduped
//!   (mirrors the python's `phone`+`phone_intl` merge, ~lines 282-288 —
//!   this crate's OSM-tag equivalent of "two candidate phone sources" is
//!   `phone`/`contact:phone`, not `phone`/`phone_intl`, since `phone_intl`
//!   is itself an enrichment-derived column with no OSM tag).
//! - `website`: `website` tag, falling back to `contact:website` if the
//!   bare tag is absent/empty.
//! - `email`: `email` tag, falling back to `contact:email`.
//! - `socials`: map built from `contact:instagram`/`contact:facebook`/
//!   `contact:twitter` tags (only non-empty ones are inserted) — the
//!   OSM-tag equivalent of the python's `socials` map, which comes from an
//!   external `Map(String, String)` enrichment column with no single OSM
//!   tag source.
//! - `address`: assembled from `addr:housenumber`, `addr:street`,
//!   `addr:city` (falling back to the resolved hierarchy locality if the
//!   raw tag is absent), `addr:postcode`, `addr:country`, in the EXACT
//!   format the upstream ClickHouse pipeline builds `full_address` in (see
//!   `/Volumes/T7/osm.pbfconverter/build_pois_v3_osm_only.py` ~lines
//!   188-194 and `build_pois_v3_paginated.py` ~lines 267-274):
//!   `"{hn} {street}, {city} {postcode}, {country}"`, trimmed of
//!   leading/trailing spaces, then collapsed/stripped of stray `" ,"`
//!   junk exactly as `extract_poi_details` does at read time (~line 309:
//!   `" ".join(full_address.split()).strip(" ,")`) — since this crate
//!   assembles `address` itself rather than reading a precomputed
//!   `full_address` column, the collapse/strip is applied inline instead
//!   of as a separate downstream step. Only emitted if the collapsed
//!   result contains at least one alphanumeric character (same guard,
//!   ~line 310), i.e. an address made of nothing but separators
//!   (`",  ,"` from four empty parts) is treated as empty.
//! - NO `rating`/`reviews`/`price` keys, ever — this crate has no
//!   enrichment source for them (open-data-only product; see the task
//!   brief's "open product" branding note) and the python's presence-gate
//!   query column list is deliberately not mirrored for those three.
//! - Emitted ONLY when the element passes `taxonomy::is_poi` AND at least
//!   one of `hours`/`phone`/`website`/`email`/`socials`/`address` is
//!   non-empty (python ~lines 319-321: "Gate: at least one DETAIL field
//!   survived (id/name alone is a shell)").

use std::io::{BufWriter, Write};
use std::path::Path;

use osmpbf::{Element, ElementReader};

use crate::error::ExtractError;
use crate::ids::{hid, osm_sid};
use crate::layers::common::tags_to_map;
use crate::nodes::NodeTable;
use crate::taxonomy::{is_poi, TagMap};

/// First non-empty tag among `keys`, in order.
fn first_tag<'a>(tags: &'a TagMap, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| tags.get(*k).map(String::as_str).filter(|v| !v.is_empty()))
}

/// `phone` + `contact:phone`, order-preserving deduped (mirrors the
/// python's `phone`+`phone_intl` merge — see module doc).
fn phones_from_tags(tags: &TagMap) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in ["phone", "contact:phone"] {
        if let Some(v) = tags.get(key) {
            if !v.is_empty() && !out.contains(v) {
                out.push(v.clone());
            }
        }
    }
    out
}

/// `contact:instagram`/`contact:facebook`/`contact:twitter` -> a
/// `{"instagram": ..., ...}` map, only non-empty entries.
fn socials_from_tags(tags: &TagMap) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for (tag, key) in [
        ("contact:instagram", "instagram"),
        ("contact:facebook", "facebook"),
        ("contact:twitter", "twitter"),
    ] {
        if let Some(v) = tags.get(tag) {
            if !v.is_empty() {
                out.insert(key.to_string(), v.clone().into());
            }
        }
    }
    out
}

/// Assemble `address` from `addr:*` tags in the exact format the upstream
/// ClickHouse pipeline builds `full_address` (see module doc):
/// `"{hn} {street}, {city} {postcode}, {country}"`, then collapse/strip
/// exactly as `extract_poi_details` does at read time. `locality` is the
/// hierarchy-resolved fallback for `addr:city` (mirrors
/// `build_pois_v3_paginated.py`'s `if(g.locality != '', g.locality, '')`
/// parent-locality fallback — the closest available signal here, since
/// this crate has no `g.locality`/geocoding-enrichment table of its own).
/// Returns `""` (never emitted) if the collapsed result has no
/// alphanumeric content.
fn address_from_tags(tags: &TagMap, locality: Option<&str>) -> String {
    let hn = tags.get("addr:housenumber").map(String::as_str).unwrap_or("");
    let street = tags.get("addr:street").map(String::as_str).unwrap_or("");
    let city = tags
        .get("addr:city")
        .map(String::as_str)
        .filter(|v| !v.is_empty())
        .or(locality)
        .unwrap_or("");
    let postcode = tags.get("addr:postcode").map(String::as_str).unwrap_or("");
    let country = tags.get("addr:country").map(String::as_str).unwrap_or("");

    let raw = format!("{hn} {street}, {city} {postcode}, {country}");
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c: char| c == ' ' || c == ',').to_string();

    if trimmed.chars().any(|c| c.is_alphanumeric()) {
        trimmed
    } else {
        String::new()
    }
}

/// One resolved details candidate, gathered during the streaming pass,
/// before the record is assembled/gated.
struct Candidate {
    kind: char,
    id: i64,
    tags: TagMap,
}

/// Pass 2: extract `/details` sidecar records from `pbf` (nodes + ways),
/// gate each candidate through `taxonomy::is_poi` (the same gate
/// `layers::poi` uses) and the "at least one detail field" rule, and write
/// `{out_dir}/poi_details.jsonl` — one bare JSON object per line. Returns
/// the number of lines written.
///
/// Unlike `layers::poi`/`layers::address`, this does not need a
/// `HierarchyIndex` for locality resolution to satisfy the id-subset
/// contract; `nodes` is accepted for API symmetry with the other layer
/// emitters (way-tagged addresses need no centroid here — `address` only
/// consumes tags, not geometry) and is reserved for a future locality
/// fallback should one be wired in. `#[allow(unused_variables)]` is not
/// needed because `nodes` genuinely isn't read; the parameter exists to
/// keep this function's signature consistent with `poi::extract`/
/// `address::extract`/`postcode::extract` per the task brief's pinned
/// signature `details::extract(pbf, nodes, out_dir)`.
pub fn extract(pbf: &Path, _nodes: &NodeTable, out_dir: &Path) -> Result<u64, ExtractError> {
    let mut candidates: Vec<Candidate> = Vec::new();

    let reader = ElementReader::from_path(pbf)?;
    reader.for_each(|element| match element {
        Element::Node(n) => {
            let tags = tags_to_map(n.tags());
            if is_poi(&tags) {
                candidates.push(Candidate { kind: 'n', id: n.id(), tags });
            }
        }
        Element::DenseNode(n) => {
            let tags = tags_to_map(n.tags());
            if is_poi(&tags) {
                candidates.push(Candidate { kind: 'n', id: n.id(), tags });
            }
        }
        Element::Way(w) => {
            let tags = tags_to_map(w.tags());
            if is_poi(&tags) {
                candidates.push(Candidate { kind: 'w', id: w.id(), tags });
            }
        }
        _ => {}
    })?;

    let path = out_dir.join("poi_details.jsonl");
    let file = std::fs::File::create(&path)?;
    let mut writer = BufWriter::new(file);
    let mut count: u64 = 0;

    for c in &candidates {
        let name = c.tags.get("name").map(String::as_str).unwrap_or("");
        let hours = c.tags.get("opening_hours").map(String::as_str).unwrap_or("");
        let phones = phones_from_tags(&c.tags);
        let website = first_tag(&c.tags, &["website", "contact:website"]).unwrap_or("");
        let email = first_tag(&c.tags, &["email", "contact:email"]).unwrap_or("");
        let socials = socials_from_tags(&c.tags);
        // No hierarchy-resolved locality available here (see fn doc); the
        // `addr:city` fallback chain degrades to the raw tag only.
        let address = address_from_tags(&c.tags, None);

        let has_detail = !hours.is_empty()
            || !phones.is_empty()
            || !website.is_empty()
            || !email.is_empty()
            || !socials.is_empty()
            || !address.is_empty();
        if !has_detail {
            continue;
        }

        let mut rec = serde_json::Map::new();
        let sid = osm_sid(c.kind, c.id);
        rec.insert("id".into(), hid(&sid).into());
        if !name.is_empty() {
            rec.insert("name".into(), name.into());
        }
        if !hours.is_empty() {
            rec.insert("hours".into(), hours.into());
        }
        if !phones.is_empty() {
            rec.insert("phone".into(), phones.into());
        }
        if !website.is_empty() {
            rec.insert("website".into(), website.into());
        }
        if !email.is_empty() {
            rec.insert("email".into(), email.into());
        }
        if !socials.is_empty() {
            rec.insert("socials".into(), socials.into());
        }
        if !address.is_empty() {
            rec.insert("address".into(), address.into());
        }

        let line = serde_json::to_vec(&serde_json::Value::Object(rec))
            .expect("serde_json::Value serialization is infallible for this shape");
        writer.write_all(&line)?;
        writer.write_all(b"\n")?;
        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> TagMap {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn first_tag_prefers_first_present_nonempty() {
        let t = tags(&[("contact:website", "https://b.example")]);
        assert_eq!(first_tag(&t, &["website", "contact:website"]), Some("https://b.example"));
    }

    #[test]
    fn first_tag_skips_empty_value() {
        let t = tags(&[("website", ""), ("contact:website", "https://b.example")]);
        assert_eq!(first_tag(&t, &["website", "contact:website"]), Some("https://b.example"));
    }

    #[test]
    fn phones_from_tags_dedupes_preserving_order() {
        let t = tags(&[("phone", "+1"), ("contact:phone", "+1")]);
        assert_eq!(phones_from_tags(&t), vec!["+1".to_string()]);
    }

    #[test]
    fn phones_from_tags_keeps_both_when_distinct() {
        let t = tags(&[("phone", "+1"), ("contact:phone", "+2")]);
        assert_eq!(phones_from_tags(&t), vec!["+1".to_string(), "+2".to_string()]);
    }

    #[test]
    fn socials_from_tags_collects_known_keys_only() {
        let t = tags(&[
            ("contact:instagram", "https://instagram.com/x"),
            ("contact:facebook", "https://facebook.com/x"),
            ("contact:youtube", "https://youtube.com/x"),
        ]);
        let socials = socials_from_tags(&t);
        assert_eq!(socials.len(), 2);
        assert_eq!(socials.get("instagram").and_then(|v| v.as_str()), Some("https://instagram.com/x"));
        assert_eq!(socials.get("facebook").and_then(|v| v.as_str()), Some("https://facebook.com/x"));
        assert!(socials.get("youtube").is_none());
    }

    #[test]
    fn address_from_tags_assembles_expected_shape() {
        let t = tags(&[
            ("addr:housenumber", "4"),
            ("addr:street", "Avenue de la Madone"),
            ("addr:city", "Monaco"),
            ("addr:postcode", "98000"),
            ("addr:country", "MC"),
        ]);
        assert_eq!(address_from_tags(&t, None), "4 Avenue de la Madone, Monaco 98000, MC");
    }

    #[test]
    fn address_from_tags_falls_back_to_locality_for_city() {
        let t = tags(&[("addr:street", "Main St")]);
        assert_eq!(address_from_tags(&t, Some("Fallback City")), "Main St, Fallback City");
    }

    #[test]
    fn address_from_tags_empty_when_no_alnum_content() {
        let t = TagMap::default();
        assert_eq!(address_from_tags(&t, None), "");
    }

    #[test]
    fn is_poi_gate_excludes_non_poi_elements_with_detail_tags() {
        // A highway=crossing with a phone tag (implausible but a good
        // adversarial case) must never pass is_poi -> no details row.
        let t = tags(&[("highway", "crossing"), ("phone", "+123")]);
        assert!(!is_poi(&t), "sanity: taxonomy must reject this as a POI");
    }
}

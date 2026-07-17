//! Small helpers shared across layer emitters (`poi`, `address`, ...) and
//! `boundaries` (which is not itself under `layers/` but is part of the same
//! crate and needs the multilingual-name key filter below).
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

/// Does `key` look like an OSM multilingual name tag (`name:<lang>`) worth
/// surfacing as a cross-language search alias?
///
/// `lang` must be 2-3 lowercase ASCII letters (`name:en`, `name:ja`,
/// `name:zh`) — this intentionally does NOT match script/variant-suffixed
/// keys like `name:zh-Hans` or `name:sr-Latn` (those are variants of an
/// already-covered base language, not a new one, and keeping the filter
/// narrow avoids alias-list bloat from the same language appearing 2-3
/// times under different script tags).
///
/// Explicitly excluded even though they'd otherwise match a loose
/// `name:*` glob: `name:etymology` (and any `name:etymology:*` sub-keys —
/// these document what a name is named AFTER, e.g. a street named for a
/// person, not a translation of the name itself), `name:pronunciation`
/// (IPA/phonetic spelling, not a searchable text form), `name:left` /
/// `name:right` (carriageway-side name variants on dual carriageways, not
/// language variants). `int_name` (the OSM "international name" tag) is
/// handled by callers as a separate, explicit one-off addition — it does
/// not match this `name:<lang>` shape at all.
pub(crate) fn is_intl_name_key(key: &str) -> bool {
    let Some(lang) = key.strip_prefix("name:") else {
        return false;
    };
    if lang.starts_with("etymology") || lang == "pronunciation" || lang == "left" || lang == "right" {
        return false;
    }
    (2..=3).contains(&lang.len()) && lang.chars().all(|c| c.is_ascii_lowercase())
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

    // --- is_intl_name_key: name:<lang> alias filter ---

    #[test]
    fn is_intl_name_key_accepts_two_letter_lang() {
        assert!(is_intl_name_key("name:en"));
        assert!(is_intl_name_key("name:ja"));
        assert!(is_intl_name_key("name:zh"));
    }

    #[test]
    fn is_intl_name_key_accepts_three_letter_lang() {
        assert!(is_intl_name_key("name:fil"));
    }

    #[test]
    fn is_intl_name_key_rejects_etymology() {
        assert!(!is_intl_name_key("name:etymology"));
        assert!(!is_intl_name_key("name:etymology:wikidata"));
    }

    #[test]
    fn is_intl_name_key_rejects_pronunciation() {
        assert!(!is_intl_name_key("name:pronunciation"));
    }

    #[test]
    fn is_intl_name_key_rejects_carriageway_side_variants() {
        assert!(!is_intl_name_key("name:left"));
        assert!(!is_intl_name_key("name:right"));
    }

    #[test]
    fn is_intl_name_key_rejects_script_variant_suffixes() {
        // Script/variant-suffixed keys are not a NEW language, so they're
        // intentionally excluded from this narrow lang-only filter.
        assert!(!is_intl_name_key("name:zh-Hans"));
        assert!(!is_intl_name_key("name:zh-Hant"));
        assert!(!is_intl_name_key("name:sr-Latn"));
    }

    #[test]
    fn is_intl_name_key_rejects_non_name_keys() {
        assert!(!is_intl_name_key("brand"));
        assert!(!is_intl_name_key("official_name"));
        assert!(!is_intl_name_key("int_name")); // handled separately by callers
    }

    #[test]
    fn is_intl_name_key_rejects_uppercase_or_wrong_length() {
        assert!(!is_intl_name_key("name:EN"));
        assert!(!is_intl_name_key("name:e"));
        assert!(!is_intl_name_key("name:english"));
    }
}

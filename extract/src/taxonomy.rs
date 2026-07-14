//! Category taxonomy ported from `pois_all.lua` (read-only reference,
//! `/Volumes/T7/osm.pbfconverter/pois_all.lua`, 180 lines).
//!
//! IMPORTANT finding (see Task 2 report): `pois_all.lua` itself does NOT
//! contain a tag -> category resolution table. Its `is_poi()` (lines 55-58)
//! is a pure existence check over
//! `amenity or shop or tourism or leisure or office or craft or healthcare`,
//! and `get_attrs()` (lines 106-148) copies each of those tags verbatim into
//! its own column — it never collapses them into a single category string.
//! The lua's *tag family order* (amenity, shop, tourism, leisure, office,
//! craft, healthcare — see line 56) is what this module's table order is
//! pinned against for precedence purposes.
//!
//! The actual key=value -> category table lives in the sibling project file
//! `osm_to_category_mapping.py` (`OSM_CATEGORY_MAP`, dict/insertion order),
//! which is the only real, on-disk, ordered tag->category mapping in this
//! codebase. It is ported here row-by-row (its comment banners kept as Rust
//! comments) and re-grouped so that within each amenity/shop/tourism/... family
//! the lua's tag-family order is respected. Rows that map the SAME (key,value)
//! to different categories in the source only keep the first occurrence,
//! matching first-match-wins semantics.

use fxhash::FxHashMap;

pub type TagMap = FxHashMap<String, String>;

/// (key, value-or-"*", category). Walked top-to-bottom; first match wins.
pub static CATEGORY_TABLE: &[(&str, &str, &str)] = &[
    // ── Food & Drink ──────────────────────────────────────────────────────
    ("amenity", "restaurant", "restaurant"),
    ("amenity", "fast_food", "fast_food"),
    ("amenity", "cafe", "cafe"),
    ("amenity", "bar", "bar"),
    ("amenity", "pub", "pub"),
    ("amenity", "biergarten", "biergarten"),
    ("amenity", "food_court", "food_court"),
    ("amenity", "ice_cream", "ice_cream"),
    ("amenity", "juice_bar", "juice_bar"),
    ("amenity", "lounge", "lounge"),
    ("amenity", "canteen", "canteen"),
    ("amenity", "internet_cafe", "internet_cafe"),
    ("amenity", "karaoke_box", "karaoke"),

    // ── Accommodation (amenity family) ───────────────────────────────────
    ("amenity", "dormitory", "dormitory"),

    // ── Tourism & Attractions (amenity family) ───────────────────────────
    ("amenity", "arts_centre", "arts_centre"),
    ("amenity", "planetarium", "planetarium"),
    ("amenity", "exhibition_centre", "exhibition_centre"),

    // ── Transport (amenity family) ───────────────────────────────────────
    ("amenity", "parking", "parking"),
    ("amenity", "fuel", "gas_station"),
    ("amenity", "charging_station", "ev_charging"),
    ("amenity", "bus_station", "bus_station"),
    ("amenity", "ferry_terminal", "ferry_terminal"),
    ("amenity", "taxi", "taxi"),
    ("amenity", "car_rental", "car_rental"),
    ("amenity", "bicycle_rental", "bicycle_rental"),
    ("amenity", "bicycle_parking", "bicycle_parking"),
    ("amenity", "scooter_rental", "scooter_rental"),
    ("amenity", "motorcycle_parking", "motorcycle_parking"),
    ("amenity", "car_wash", "car_wash"),
    ("amenity", "vehicle_inspection", "vehicle_inspection"),
    ("amenity", "parking_space", "parking"),

    // ── Health & Medical (amenity family) ────────────────────────────────
    ("amenity", "hospital", "hospital"),
    ("amenity", "clinic", "clinic"),
    ("amenity", "doctors", "doctor"),
    ("amenity", "dentist", "dentist"),
    ("amenity", "pharmacy", "pharmacy"),
    ("amenity", "veterinary", "veterinary"),
    ("amenity", "nursing_home", "nursing_home"),
    ("amenity", "first_aid", "first_aid"),
    ("amenity", "healthcare", "healthcare"),
    ("amenity", "spa", "spa"),
    ("amenity", "public_bath", "public_bath"),

    // ── Finance (amenity family) ─────────────────────────────────────────
    ("amenity", "bank", "bank"),
    ("amenity", "atm", "atm"),
    ("amenity", "bureau_de_change", "currency_exchange"),
    ("amenity", "money_transfer", "money_transfer"),
    ("amenity", "payment_terminal", "payment_terminal"),

    // ── Sports & Fitness (amenity family) ────────────────────────────────
    ("amenity", "dojo", "martial_arts"),

    // ── Education (amenity family) ───────────────────────────────────────
    ("amenity", "school", "school"),
    ("amenity", "university", "university"),
    ("amenity", "college", "college"),
    ("amenity", "kindergarten", "kindergarten"),
    ("amenity", "library", "library"),
    ("amenity", "language_school", "language_school"),
    ("amenity", "music_school", "music_school"),
    ("amenity", "driving_school", "driving_school"),
    ("amenity", "dance_school", "dance_school"),
    ("amenity", "prep_school", "prep_school"),
    ("amenity", "training", "training_centre"),
    ("amenity", "research_institute", "research"),

    // ── Religious & Spiritual (amenity family) ───────────────────────────
    ("amenity", "place_of_worship", "place_of_worship"),
    ("amenity", "monastery", "monastery"),
    ("amenity", "meditation_centre", "meditation_centre"),
    ("amenity", "community_centre", "community_centre"),

    // ── Entertainment (amenity family) ───────────────────────────────────
    ("amenity", "cinema", "cinema"),
    ("amenity", "theatre", "theatre"),
    ("amenity", "nightclub", "nightclub"),
    ("amenity", "casino", "casino"),
    ("amenity", "karaoke", "karaoke"),
    ("amenity", "music_venue", "music_venue"),
    ("amenity", "concert_hall", "concert_hall"),
    ("amenity", "events_venue", "events_venue"),
    ("amenity", "circus", "circus"),
    ("amenity", "gambling", "gambling"),

    // ── Public Services (amenity family) ─────────────────────────────────
    ("amenity", "post_office", "post_office"),
    ("amenity", "police", "police"),
    ("amenity", "fire_station", "fire_station"),
    ("amenity", "courthouse", "courthouse"),
    ("amenity", "townhall", "town_hall"),
    ("amenity", "embassy", "embassy"),
    ("amenity", "prison", "prison"),
    ("amenity", "recycling", "recycling"),
    ("amenity", "waste_disposal", "waste_disposal"),
    ("amenity", "toilets", "public_toilet"),
    ("amenity", "drinking_water", "drinking_water"),
    ("amenity", "shelter", "shelter"),
    ("amenity", "social_facility", "social_facility"),
    ("amenity", "refugee_site", "refugee_site"),
    ("amenity", "bench", "bench"),
    ("amenity", "waste_basket", "waste_basket"),
    ("amenity", "post_box", "post_box"),
    ("amenity", "postalcode", "postal_area"),
    ("amenity", "admin_boundary", "admin_boundary"),

    // ── Real Estate (amenity family) ─────────────────────────────────────
    ("amenity", "retirement_home", "retirement_home"),

    // ── Additional OSM tags found in pois (amenity family) ───────────────
    ("amenity", "landmark", "landmark"),
    ("amenity", "park", "park"),
    ("amenity", "marketplace", "marketplace"),
    ("amenity", "vending_machine", "vending_machine"),

    // ── Shopping ──────────────────────────────────────────────────────────
    ("shop", "supermarket", "supermarket"),
    ("shop", "convenience", "convenience_store"),
    ("shop", "clothes", "clothing_store"),
    ("shop", "electronics", "electronics"),
    ("shop", "furniture", "furniture"),
    ("shop", "bakery", "bakery"),
    ("shop", "butcher", "butcher"),
    ("shop", "florist", "florist"),
    ("shop", "gift", "gift_shop"),
    ("shop", "jewelry", "jewelry"),
    ("shop", "sports", "sports_store"),
    ("shop", "toy", "toy_store"),
    ("shop", "books", "bookstore"),
    ("shop", "music", "music_store"),
    ("shop", "musical_instrument", "music_store"),
    ("shop", "photo", "photo_store"),
    ("shop", "hairdresser", "hair_salon"),
    ("shop", "beauty", "beauty_salon"),
    ("shop", "massage", "massage"),
    ("shop", "nail_salon", "nail_salon"),
    ("shop", "cosmetics", "cosmetics"),
    ("shop", "optician", "optician"),
    ("shop", "pharmacy", "pharmacy"),
    ("shop", "medical_supply", "medical_supply"),
    ("shop", "car", "car_dealer"),
    ("shop", "car_repair", "car_repair"),
    ("shop", "tyres", "tyre_shop"),
    ("shop", "car_parts", "car_parts"),
    ("shop", "motorcycle", "motorcycle_dealer"),
    ("shop", "bicycle", "bicycle_shop"),
    ("shop", "pet", "pet_store"),
    ("shop", "garden_centre", "garden_centre"),
    ("shop", "hardware", "hardware_store"),
    ("shop", "doityourself", "hardware_store"),
    ("shop", "trade", "trade_supplier"),
    ("shop", "wholesale", "wholesale"),
    ("shop", "department_store", "department_store"),
    ("shop", "mall", "shopping_mall"),
    ("shop", "marketplace", "marketplace"),
    ("shop", "organic", "organic_store"),
    ("shop", "hobby", "hobby_shop"),
    ("shop", "stationery", "stationery"),
    ("shop", "travel_agency", "travel_agency"),
    ("shop", "rental", "rental"),
    ("shop", "service", "service"),
    ("shop", "courier", "courier"),
    ("shop", "laundry", "laundry"),
    ("shop", "dry_cleaning", "dry_cleaning"),
    ("shop", "atv", "atv_dealer"),
    ("shop", "general", "general_store"),
    ("shop", "mobile_phone", "mobile_phone_shop"),
    // Generic fallback for any shop=* value not enumerated above. Not present
    // in either source file verbatim — pois_all.lua has no category table at
    // all, and osm_to_category_mapping.py has no wildcard row either. Added
    // per Task 2 brief's explicit requirement that `categorize` support `*`
    // wildcard values, using the generic bucket name "shop".
    ("shop", "*", "shop"),

    // ── Accommodation (tourism family) ───────────────────────────────────
    ("tourism", "hotel", "hotel"),
    ("tourism", "hostel", "hostel"),
    ("tourism", "motel", "motel"),
    ("tourism", "guest_house", "guest_house"),
    ("tourism", "chalet", "chalet"),
    ("tourism", "camp_site", "campsite"),
    ("tourism", "caravan_site", "caravan_site"),
    ("tourism", "apartment", "apartment"),

    // ── Tourism & Attractions ─────────────────────────────────────────────
    ("tourism", "attraction", "attraction"),
    ("tourism", "museum", "museum"),
    ("tourism", "gallery", "gallery"),
    ("tourism", "viewpoint", "viewpoint"),
    ("tourism", "theme_park", "theme_park"),
    ("tourism", "zoo", "zoo"),
    ("tourism", "aquarium", "aquarium"),
    ("tourism", "artwork", "artwork"),
    ("tourism", "information", "tourist_info"),
    ("historic", "monument", "monument"),
    ("historic", "memorial", "memorial"),
    ("historic", "castle", "castle"),
    ("historic", "ruins", "ruins"),
    ("historic", "archaeological_site", "archaeological_site"),
    ("historic", "building", "historic_building"),

    // ── Nature & Outdoors ─────────────────────────────────────────────────
    ("leisure", "park", "park"),
    ("leisure", "nature_reserve", "nature_reserve"),
    ("leisure", "garden", "garden"),
    ("leisure", "playground", "playground"),
    ("leisure", "beach_resort", "beach"),
    ("natural", "beach", "beach"),
    ("natural", "peak", "mountain_peak"),
    ("natural", "volcano", "volcano"),
    ("natural", "water", "water"),
    ("natural", "wood", "forest"),
    ("natural", "wetland", "wetland"),

    // ── Transport (non-amenity families) ─────────────────────────────────
    // NOTE: osm_to_category_mapping.py also carries a large highway=* block
    // ("Additional OSM tags found in pois" section: residential, tertiary,
    // secondary, primary, unclassified, service, track, trunk, footway,
    // motorway, living_street, path, pedestrian, cycleway -> road/footpath/
    // motorway/etc). That block is intentionally NOT ported here: pois_all.lua's
    // is_poi() (line 56) enumerates its POI tag family as exactly
    // `amenity, shop, tourism, leisure, office, craft, healthcare` and never
    // includes `highway` — highways are handled by the separate streets.lua
    // extractor, not pois_all.lua. Including a highway=* row here would also
    // contradict the Task 2 brief's own `non_poi_returns_none` test, which
    // asserts categorize(highway=residential) == None. railway/aeroway rows
    // ARE kept because pois_all.lua's downstream consumers treat them as
    // venue-like (see import_to_elasticsearch_ultra.py get_layer()), but
    // note they too fall outside is_poi()'s literal tag list, so is_poi()
    // still returns false for them (no amenity/shop/tourism/leisure/office/
    // craft/healthcare tag present).
    ("railway", "station", "train_station"),
    ("railway", "halt", "train_stop"),
    ("aeroway", "aerodrome", "airport"),
    ("aeroway", "terminal", "airport_terminal"),

    // ── Health & Medical (healthcare family) ─────────────────────────────
    ("healthcare", "clinic", "clinic"),
    ("healthcare", "doctor", "doctor"),
    ("healthcare", "hospital", "hospital"),
    ("healthcare", "pharmacy", "pharmacy"),
    ("healthcare", "dentist", "dentist"),
    ("healthcare", "alternative", "alternative_medicine"),
    ("healthcare", "optometrist", "optometrist"),

    // ── Finance (office family) ───────────────────────────────────────────
    ("office", "insurance", "insurance"),
    ("office", "accountant", "accountant"),
    ("office", "financial", "financial_services"),

    // ── Sports & Fitness ───────────────────────────────────────────────────
    ("leisure", "fitness_centre", "gym"),
    ("leisure", "sports_centre", "sports_centre"),
    ("leisure", "stadium", "stadium"),
    ("leisure", "swimming_pool", "swimming_pool"),
    ("leisure", "golf_course", "golf_course"),
    ("leisure", "pitch", "sports_field"),
    ("leisure", "track", "athletics_track"),
    ("leisure", "ice_rink", "ice_rink"),
    ("leisure", "bowling_alley", "bowling"),
    ("leisure", "dance", "dance_studio"),
    ("leisure", "martial_arts", "martial_arts"),
    ("leisure", "climbing", "climbing"),
    ("leisure", "water_park", "water_park"),
    ("leisure", "miniature_golf", "mini_golf"),
    ("sport", "swimming", "swimming_pool"),

    // ── Education (office family) ─────────────────────────────────────────
    ("office", "educational_institution", "educational_institution"),

    // ── Entertainment (leisure family) ────────────────────────────────────
    ("leisure", "amusement_arcade", "amusement_arcade"),

    // ── Public Services (office / government / emergency families) ───────
    ("office", "government", "government_office"),
    ("government", "justice", "courthouse"),
    ("emergency", "fire_hydrant", "fire_hydrant"),

    // ── Business & Services ────────────────────────────────────────────────
    ("office", "company", "company"),
    ("office", "it", "it_company"),
    ("office", "architect", "architect"),
    ("office", "lawyer", "lawyer"),
    ("office", "agent", "agent"),
    ("office", "ngo", "ngo"),
    ("office", "consulting", "consulting"),
    ("craft", "electrician", "electrician"),
    ("craft", "plumber", "plumber"),
    ("craft", "carpenter", "carpenter"),
    ("craft", "painter", "painter"),
    ("craft", "handyman", "handyman"),
    ("craft", "printer", "printing"),
    ("craft", "glaziery", "glazier"),
    ("craft", "general", "tradesperson"),
    ("man_made", "crane", "crane"),
    ("landuse", "industrial", "industrial"),
    ("office", "logistics", "logistics"),
    ("office", "association", "association"),
    ("office", "construction_company", "construction"),
    ("craft", "metal_construction", "metal_construction"),

    // ── Real Estate (non-amenity families) ────────────────────────────────
    ("landuse", "residential", "residential"),
    ("building", "apartments", "apartments"),
    ("office", "estate_agent", "estate_agent"),

    // ── Nature & Outdoors (leisure, cont'd) ───────────────────────────────
    ("leisure", "picnic_table", "picnic_area"),
];

/// Walk `CATEGORY_TABLE` in order; first (key, value-or-`*`) match wins.
pub fn categorize(tags: &TagMap) -> Option<&'static str> {
    for (key, value, category) in CATEGORY_TABLE {
        if let Some(tag_value) = tags.get(*key) {
            if *value == "*" || tag_value == value {
                return Some(category);
            }
        }
    }
    None
}

/// Mirrors `pois_all.lua`'s `is_poi()` (lines 55-58) — an object is a POI
/// candidate if it carries name or brand identification AND resolves to a
/// category. `pois_all.lua` itself only gates on tag presence
/// (`amenity or shop or tourism or leisure or office or craft or healthcare`);
/// the name/brand requirement is an addition from the Task 2 brief's stated
/// interface contract.
pub fn is_poi(tags: &TagMap) -> bool {
    let has_identity = tags.contains_key("name") || tags.contains_key("brand");
    has_identity && categorize(tags).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> TagMap {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn amenity_restaurant() {
        assert_eq!(categorize(&tags(&[("amenity", "restaurant")])), Some("restaurant"));
    }

    #[test]
    fn shop_wildcard_falls_back_to_generic() {
        // No `shop=*` row exists in either pois_all.lua (no table at all) or
        // osm_to_category_mapping.py (no wildcard row). Per the Task 2 brief's
        // explicit wildcard requirement, unknown shop=* values resolve to the
        // generic bucket "shop" (see CATEGORY_TABLE comment above the ("shop","*",...) row).
        assert_eq!(categorize(&tags(&[("shop", "zibzab")])), Some("shop"));
    }

    #[test]
    fn precedence_matches_lua_order() {
        // pois_all.lua's is_poi() / get_attrs() enumerate tag families in the
        // order: amenity, shop, tourism, leisure, office, craft, healthcare
        // (line 56). CATEGORY_TABLE places all amenity rows before all shop
        // rows to mirror that family precedence, so amenity wins when both
        // are present.
        let t = tags(&[("amenity", "cafe"), ("shop", "bakery")]);
        assert_eq!(categorize(&t), Some("cafe"));
    }

    #[test]
    fn non_poi_returns_none() {
        // highway is not part of pois_all.lua's POI tag family (line 56:
        // amenity, shop, tourism, leisure, office, craft, healthcare) — see
        // the NOTE above the railway/aeroway rows in CATEGORY_TABLE.
        assert_eq!(categorize(&tags(&[("highway", "residential")])), None);
    }

    #[test]
    fn unmapped_tag_returns_none() {
        assert_eq!(categorize(&tags(&[("foo", "bar")])), None);
    }

    #[test]
    fn is_poi_requires_name_or_brand_plus_category() {
        assert_eq!(is_poi(&tags(&[("amenity", "cafe")])), false); // no name/brand
        assert_eq!(is_poi(&tags(&[("amenity", "cafe"), ("name", "Joe's")])), true);
        assert_eq!(is_poi(&tags(&[("amenity", "cafe"), ("brand", "Starbucks")])), true);
        assert_eq!(is_poi(&tags(&[("foo", "bar"), ("name", "Nothing")])), false); // no category
    }
}

# Task 9 Report — Postcode emitter + OSM-tag details sidecar source

## Summary

Implemented `extract/src/layers/postcode.rs` (`layers::postcode::extract`)
and `extract/src/layers/details.rs` (`layers::details::extract`), wired via
`pub mod postcode;` / `pub mod details;` in `extract/src/layers/mod.rs`.
Added a new integration test file, `extract/tests/postcode_details_monaco.rs`
(6 tests), rather than extending `layers_monaco.rs` — the brief left this a
judgment call ("or a new details test file, your call"), and a dedicated
file matches the existing per-layer-pair convention (`poi_monaco.rs`,
`address_monaco.rs` are each their own file).

TDD followed per brief Step 1: wrote the test file first against the
not-yet-existing `layers::postcode`/`layers::details` modules, confirmed a
compile-time red (`error[E0432]: unresolved imports ... no postcode in
layers`, `no details in layers`), then implemented until green.

## `layers::postcode::extract` (mirrors `extract_postcode`, py lines 590-617)

Pinned python function re-read in full before implementing (not just the
brief's paraphrase):

```python
def extract_postcode(cc, out):
    ...
    SELECT id, postcode, lon, lat FROM geocoder.postcodes_v3
    WHERE parent_country_a = '{cc}' AND postcode != ''
    ...
    w(fh, {
        "type": "Feature", "id": hid(r["id"]),
        "properties": {"carmen:text": clean_alias(pc), "carmen:center": center},
        "geometry": {"type": "Point", "coordinates": center},
    })
```

Key finding: the python's `postcodes_v3` source table is **already**
one-row-per-distinct-code with a precomputed `(lon, lat)` (server-side
aggregation this crate has no equivalent table for) — so python's
`extract_postcode` itself does zero grouping. This crate's raw-tag input
(`addr:postcode` scattered across many nodes/ways) has no such
pre-aggregation, so the Rust side does the grouping the upstream SQL query
already did for the python: collects every node/way carrying a non-empty
`addr:postcode` (way via member-node centroid, same `way_centroid` helper
`layers::poi`/`layers::address` use), groups by the code string, and emits
one `Feature` per distinct code at the **arithmetic mean** of every member's
coordinate — the closest available signal to "the code's own precomputed
`(lon, lat)`" without a `postcodes_v3`-equivalent table.

- `carmen:text`: `clean_alias(code)` — verbatim port (comma-strip +
  whitespace-collapse).
- `carmen:center`: `[lon, lat]` mean, unrounded (python doesn't round this
  layer's center either — confirmed by re-reading the function body; only
  `extract_country` rounds, per Task 8's finding).
- **No `carmen:score`** — the python's `extract_postcode` body sets none
  (unlike `extract_place`/`extract_street`/`extract_region`, which all do).
- Feature id: `hid("postcode|" + code)` — namespaced (the python's bare
  `hid(r["id"])` operates on a `postcodes_v3` row id with no OSM-tag
  equivalent here), same rationale as `layers::address`'s `addr:` prefix
  and `layers::place`'s `region-area|`/`country-area|` prefixes.

**Monaco fixture verification** (`osmium tags-filter addr:postcode` +
`export`, ad hoc, before writing any code): 4 distinct codes present —
`98000` (468 occurrences), `06240` (8), `98020` (1), `06320` (4) — confirmed
`'98000'` is reachable and well-represented regardless of node-vs-way
sourcing, satisfying the brief's Step 1 assertion.

## `layers::details::extract` (mirrors `extract_poi_details`, py lines 249-324)

Pinned python function and the sidecar design doc
(`docs/superpowers/specs/2026-07-14-details-sidecar-design.md`) both read in
full first. Field-by-field adaptation from ClickHouse `pois_v3` enrichment
columns (Google Places-derived; no OSM-tag source at all for three of them)
to raw OSM tags:

| python field | OSM-tag source (this crate) |
|---|---|
| `id` | `hid(osm_sid(kind, id))` — same function `layers::poi` calls on the identical element, guaranteeing the id-subset contract |
| `name` | `name` tag (display-only, does NOT count toward the non-empty gate — python ~line 257 doc comment) |
| `hours` | `opening_hours` tag verbatim |
| `phone[]` | `phone` + `contact:phone`, order-preserving deduped (python merges `phone`+`phone_intl`, both enrichment columns; this crate's nearest OSM-tag pair is `phone`/`contact:phone`) |
| `website` | `website`, falling back to `contact:website` |
| `email` | `email`, falling back to `contact:email` |
| `socials{}` | built from `contact:instagram`/`contact:facebook`/`contact:twitter` (python's `socials` column is an external `Map(String,String)` with no single-tag OSM source) |
| `address` | assembled from `addr:*` tags — see below |
| `rating`/`reviews`/`price` | **never emitted** — Google Places enrichment outputs with zero OSM-tag equivalent; explicitly excluded per the brief's "open product" branding rule |
| `brand`/`names{}` | **not implemented** — out of the brief's field list (`{id, name, hours, phone[], website, email, socials{}, address}`); `layers::poi` already emits `brand` on the POI feature itself, and `names{}`(`names_intl`) has no OSM-tag source, consistent with `layers::poi`/`layers::street`/`layers::place` all omitting it for the same reason |

### `address` assembly — traced to the actual SQL, not guessed

The brief said `"street hn, postcode city"` as a rough sketch and told me to
"check what extract_poi_details assembles and mirror." `extract_poi_details`
itself only *reads* a precomputed `full_address` column — the assembly logic
lives upstream, in the ClickHouse population scripts. Traced it to two
matching SQL blocks (`build_pois_v3_osm_only.py` ~188-194,
`build_pois_v3_paginated.py` ~267-274):

```sql
trim(BOTH ' ' FROM concat(
    coalesce(p.addr_housenumber, ''), ' ',
    coalesce(p.addr_street, ''), ', ',
    coalesce(p.addr_city, ''), ' ',
    coalesce(p.addr_postcode, ''), ', ',
    coalesce(p.addr_country, '')
)) AS full_address
```

i.e. the actual pinned format is **`"{hn} {street}, {city} {postcode}, {country}"`**
(hn-then-street, not street-then-hn as the brief's paraphrase implied) — the
python wins per the brief's own rule. `extract_poi_details` then re-cleans
this precomputed value at read time (~line 309:
`" ".join(full_address.split()).strip(" ,")`, requiring alnum content to
survive). Since this crate assembles `address` itself rather than reading a
precomputed column, the collapse/strip is applied inline in
`address_from_tags` instead of as a separate downstream step — same net
effect. `addr:city` falls back to a `locality` parameter (reserved for a
future `HierarchyIndex`-resolved fallback, mirroring
`build_pois_v3_paginated.py`'s `if(g.locality != '', g.locality, '')`
pattern) — `details::extract`'s current call site passes `None` since no
`HierarchyIndex` is threaded through the brief's pinned signature
`details::extract(pbf, nodes, out_dir)`.

- Gate: emitted ONLY when the element passes `taxonomy::is_poi` (same gate
  `layers::poi` uses — this is what makes the id-subset test structurally
  guaranteed, not just empirically true) AND at least one of
  `hours`/`phone`/`website`/`email`/`socials`/`address` is non-empty (python
  ~lines 319-321: "at least one DETAIL field survived (id/name alone is a
  shell)").
- Output format: **`poi_details.jsonl`, one bare JSON object per line — NOT
  a GeoJSON `Feature`** (no `type`/`geometry` wrapper), per the brief and
  confirmed against the converter's `DetailsIn` deserialize struct
  (`atlas-edge/converter/src/main.rs` ~1949-1976), which expects exactly
  `{id, name?, hours?, phone[], website?, email?, socials{}, address?, ...}`
  at the top level.

## Converter compatibility — verified against the actual consumer, not assumed

Read `atlas-edge/converter/src/main.rs`'s `details_sidecar` module in full
(the `--emit-details` stage) before finalizing the field list. Its
`DetailsIn` struct (line 1949) uses `#[serde(default)]` on every field
except `id`, so my omission of `rating`/`reviews`/`price`/`brand`/`names` is
valid input — the converter's `into_rec()` gate (`has_detail`, line
2041-2051) just never sees those fields set, same effect as if I'd emitted
`null`. Confirmed empirically too: the brief's smoke-test step is not
optional in this repo — the dev binary already exists prebuilt at
`/Volumes/T7/osm.pbfconverter/atlas-edge/converter/target/release/convert`
(no `CONVERT_BIN` env needed), so
`monaco_details_satisfy_converter_emit_details_smoke` actually runs it
end-to-end:

```
✔ details sidecar → .../ae_details_monaco_converter_dst (833 records, dat 62195 bytes, 256 idx shards + meta)
```

Exit 0, 833 of the Monaco fixture's details candidates round-tripped through
the real converter into idx+dat+meta — not skipped.

## Test results

```
cd extract && cargo test
```

103 tests total, all green: 95 unit tests (crate-wide, +8 new: 2 in
`postcode.rs`, 6 in `details.rs`) + 1 `address_monaco` + 2
`boundaries_monaco` + 4 `layers_monaco` + 1 `nodes_monaco` + 1 `poi_monaco`
+ 6 `postcode_details_monaco` (new).

`postcode_details_monaco.rs`'s six tests, pinning the brief's exact
acceptance criteria plus a few adversarial extras:

- `monaco_postcode_extraction_contains_98000` — finds `'98000'` among
  `carmen:text` values; validates every feature's id uniqueness, `Point`
  geometry shape, non-empty `carmen:text`/`carmen:center`.
- `monaco_postcode_ids_are_distinct_per_code` — ≥2 distinct codes, one
  feature per code (no duplicate `carmen:text`).
- `monaco_details_ids_are_subset_of_poi_ids` — builds a `HashSet` from
  `poi.geojsonl` ids, asserts every `poi_details.jsonl` id is a member (the
  brief's core gate); also asserts no `rating`/`price`/`reviews`/
  `review_count` JSON key is ever present.
- `monaco_details_lines_never_contain_forbidden_substrings` — raw-text
  substring check for `"rating"`/`"price"`/`"review"` across every line
  (belt-and-suspenders alongside the structural key check above, per the
  brief's literal wording).
- `monaco_details_have_expected_fields` — every present field is non-empty
  (extractor must omit, never emit empty-string/array/object); finds real
  Monaco fixture data for hours, phone, website, and email.
- `monaco_details_satisfy_converter_emit_details_smoke` — runs the real
  `convert --emit-details` dev binary end-to-end (see above); would
  `eprintln!` and skip gracefully if the binary were absent, but it isn't.

```
cargo clippy --all-targets -- -D warnings
```

Clean, 0 warnings, no fixes needed.

`cargo fmt --check` continues to report crate-wide diffs predating this task
(noted already in the Task 8 report) — not addressed, out of scope, not a
regression.

## Files touched

- `extract/src/layers/mod.rs` — added `pub mod details;` / `pub mod postcode;`
- `extract/src/layers/postcode.rs` — new
- `extract/src/layers/details.rs` — new
- `extract/tests/postcode_details_monaco.rs` — new

## Fix round 1

**Issue**: Postcode grouping key was not normalized before lookup. Raw tag values
like `"98000"`, `"98000 "` (trailing space), and `" 98000"` (leading space) would
form 3 separate groups instead of collapsing into 1 logical postcode. Each group
would emit a feature with `carmen:text: "98000"` (after cleaning), resulting in
duplicate postcode features per code.

**Root cause**: The grouping key in the `record` closure (line 84) was the raw
postcode string from `postcode_from_tags()`, before `clean_alias()` normalization.
The cleanup was applied only at emit time (line 132), too late to affect grouping.

**Fix**: Normalize the grouping key with `clean_alias()` BEFORE the group lookup.
Skip recording if the cleaned code is empty. This ensures all whitespace/comma
variants of the same postcode form exactly one group with the mean coordinate over
all members.

**TDD verification**: Added a unit test `grouping_key_normalized_before_lookup()`
that feeds synthetic members with raw codes `"98000"`, `"98000 "`, and `" 98000"`,
demonstrates the bug (3 groups pre-fix), implements the fix logic inline, verifies
post-fix behavior (1 group with n=3, mean coordinate correct).

**Module doc correction**: Rewrote the module header to state the truth: Python's
`postcodes_v3` source is **NOT deduplicated** (e.g., NL '1183BS' has 3 rows at 3
coords; GB 4.37M rows / 2.64M distinct codes). Python's `extract_postcode` emits
one feature per ROW, producing duplicate features. **This crate's distinct-code
+ mean-coordinate grouping is a DELIBERATE DESIGN IMPROVEMENT**, not parity with
the Python.

**Test fallback**: Fixed `monaco_details_satisfy_converter_emit_details_smoke`
fallback path to compute repo-relative path from `CARGO_MANIFEST_DIR` (resolving
`../../atlas-edge/converter/target/release/convert`) instead of hardcoded
`/Volumes/T7/...`. Gracefully skips with `eprintln!` if the binary is absent.

**Verification**: `cargo test` green (all 103 tests), converter smoke test runs
successfully. `cargo clippy --all-targets -- -D warnings` clean.

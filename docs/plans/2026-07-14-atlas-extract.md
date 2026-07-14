# atlas-extract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One Rust binary that streams an `osm.pbf` and emits the exact 7-layer geojsonl contract the existing `convert` binary consumes — no databases anywhere.

**Architecture:** Two streaming passes over the pbf. Pass 1 builds (a) an in-memory node-location table and (b) an admin-boundary/place-node hierarchy index (polygon assembly + H3-bucketed point-in-polygon). Pass 2 streams nodes/ways again and emits POIs, addresses, streets, places, regions, countries, postcodes and the optional `poi_details.jsonl`, resolving each feature's parents through the pass-1 index.

**Tech Stack:** Rust 2021; crates: `osmpbf` (reader), `serde`/`serde_json`, `clap` (derive), `blake2`, `h3o`, `geo` (ray-cast PIP via `Contains`), `fxhash`. Fixture: a checked-in Monaco extract (~600 KB).

## Global Constraints

- Output contract is the one produced by `scripts/extract_country_v3.py` — field names, grouping, and `hid()` MUST match byte-for-byte semantics (spec: sub-project 1; the converter is the referee: `convert --src <outdir> --dst <dst>` must succeed).
- `hid(s) = big-endian u64 of blake2b(s, digest_size=7)` — 7 bytes, so ids are < 2^56 (the /details shard rule depends on this).
- No proprietary references: no ClickHouse, no `ext_`/`gext_`, no ratings/reviews/price anywhere in this crate.
- Feature ids in output use the same source-id strings the OSM pipeline uses: `"n{id}"`, `"w{id}"`, `"r{id}"` before hashing (verify against pois_all.lua's id convention in Task 2 Step 1 and adjust ONCE there if it differs; every later task uses `osm_sid()`).
- Memory target: NL-sized extracts ≤ 8 GB RAM; planet is out of scope for v1 (hard error with a clear message if the node table exceeds `--max-nodes`, default 400M).
- Crate lives at `extract/` in the atlas-edge repo (standalone crate like `converter/`), binary name `atlas-extract`.

---

### Task 1: Crate scaffold, `hid` port, geojsonl writer

**Files:**
- Create: `extract/Cargo.toml`, `extract/src/main.rs`, `extract/src/ids.rs`, `extract/src/emit.rs`
- Test: inline `#[cfg(test)]` in `ids.rs` and `emit.rs`

**Interfaces:**
- Produces: `ids::hid(s: &str) -> u64`; `ids::osm_sid(kind: char, id: i64) -> String` (`osm_sid('n', 123) == "n123"`); `emit::LayerWriter` with `fn new(path: &Path) -> io::Result<Self>`, `fn feature(&mut self, id: u64, props: &serde_json::Map<String, Value>, geometry: Value) -> io::Result<()>`, `fn count(&self) -> u64`.

- [ ] **Step 1: Generate hid test vectors from the Python source of truth**

```bash
cd /Volumes/T7/osm.pbfconverter/atlas-edge
/opt/homebrew/bin/python3 -c "
import hashlib
for s in ['n1','w42','r7444','n123456789','abc']:
    print(s, int.from_bytes(hashlib.blake2b(s.encode(), digest_size=7).digest(), 'big'))"
```
Record the 5 printed pairs; they become the assertions in Step 2.

- [ ] **Step 2: Write failing tests for `hid` + `osm_sid` (paste the recorded vectors)**

```rust
// extract/src/ids.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hid_matches_python_blake2b7() {
        // values from Task 1 Step 1 — Python is the source of truth
        assert_eq!(hid("n1"), /* paste */ 0);
        assert_eq!(hid("w42"), /* paste */ 0);
        assert_eq!(hid("r7444"), /* paste */ 0);
        assert_eq!(hid("n123456789"), /* paste */ 0);
        assert_eq!(hid("abc"), /* paste */ 0);
        assert!(hid("n1") < (1u64 << 56)); // 7-byte digest ⇒ /details shard rule holds
    }
    #[test]
    fn sid_format() { assert_eq!(osm_sid('n', 123), "n123"); }
}
```

- [ ] **Step 3: Run to verify failure** — `cd extract && cargo test` → FAIL (unresolved `hid`).

- [ ] **Step 4: Implement**

```rust
// extract/src/ids.rs
use blake2::{digest::{Update, VariableOutput}, Blake2bVar};

pub fn hid(s: &str) -> u64 {
    let mut h = Blake2bVar::new(7).expect("7-byte blake2b");
    h.update(s.as_bytes());
    let mut out = [0u8; 7];
    h.finalize_variable(&mut out).expect("finalize");
    let mut v: u64 = 0;
    for b in out { v = (v << 8) | b as u64; }
    v
}

pub fn osm_sid(kind: char, id: i64) -> String { format!("{kind}{id}") }
```

`extract/Cargo.toml`:
```toml
[package]
name = "atlas-extract"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "atlas-extract"
path = "src/main.rs"

[dependencies]
osmpbf = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
blake2 = "0.10"
h3o = "0.7"
geo = "0.30"
fxhash = "0.2"
```

- [ ] **Step 5: Write failing test for LayerWriter (one feature → one JSON line, key order irrelevant, `id` numeric)**

```rust
// extract/src/emit.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writes_one_feature_per_line() {
        let dir = std::env::temp_dir().join("ae_emit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("poi.geojsonl");
        let mut w = LayerWriter::new(&p).unwrap();
        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), "Cafe X".into());
        w.feature(42, &props, serde_json::json!({"type":"Point","coordinates":[4.9,52.3]})).unwrap();
        drop(w);
        let line = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["type"], "Feature");
        assert_eq!(v["id"], 42);
        assert_eq!(v["properties"]["carmen:text"], "Cafe X");
        assert_eq!(v["geometry"]["type"], "Point");
    }
}
```

- [ ] **Step 6: Implement LayerWriter** (BufWriter, `serde_json::to_writer` + `\n`, count increment). Run `cargo test` → PASS.

- [ ] **Step 7: Commit** — `git add extract && git commit -m "feat(extract): crate scaffold, hid port (python-vector-pinned), geojsonl writer"`

---

### Task 2: Category taxonomy port from pois_all.lua

**Files:**
- Create: `extract/src/taxonomy.rs`
- Reference (read-only): `/Volumes/T7/osm.pbfconverter/pois_all.lua`

**Interfaces:**
- Produces: `taxonomy::categorize(tags: &TagMap) -> Option<&'static str>` where `TagMap = fxhash::FxHashMap<String, String>`; `taxonomy::is_poi(tags: &TagMap) -> bool` (name or brand present AND categorize hits). Also `taxonomy::CATEGORY_TABLE: &[(&str, &str, &str)]` (key, value-or-`*`, category) exposed for docs generation.

- [ ] **Step 1: Read pois_all.lua fully.** It is 180 lines: extract the tag→category mapping tables AND the id convention (`n/w/r` prefixes — verify Global Constraint 4 now; if the lua uses e.g. `node/…` adjust `osm_sid` and its test in ONE commit).

- [ ] **Step 2: Write failing tests — one per taxonomy family, plus precedence**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn tags(pairs: &[(&str, &str)]) -> TagMap {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }
    #[test]
    fn amenity_restaurant() {
        assert_eq!(categorize(&tags(&[("amenity","restaurant")])), Some("restaurant"));
    }
    #[test]
    fn shop_wildcard_falls_back_to_generic() {
        // pois_all.lua maps unknown shop=* to a generic retail category — mirror its exact string
        assert_eq!(categorize(&tags(&[("shop","zibzab")])), Some(/* exact lua fallback */ "shop"));
    }
    #[test]
    fn precedence_matches_lua_order() {
        // a feature with BOTH amenity and shop must resolve the way the lua does — pin it
        let t = tags(&[("amenity","cafe"),("shop","bakery")]);
        assert_eq!(categorize(&t), Some(/* whichever the lua picks */ "cafe"));
    }
    #[test]
    fn non_poi_returns_none() {
        assert_eq!(categorize(&tags(&[("highway","residential")])), None);
    }
}
```

- [ ] **Step 3: Run → FAIL.** Implement `CATEGORY_TABLE` as a static slice ported row-by-row from the lua (keep lua comment lines as Rust comments so future diffs against the lua are reviewable), `categorize` walking the table in lua order (first match wins), wildcard `*` value support. Run → PASS.

- [ ] **Step 4: Commit** — `feat(extract): category taxonomy ported from pois_all.lua (order-pinned)`

---

### Task 3: Pass 1a — node location table

**Files:**
- Create: `extract/src/nodes.rs`
- Test: inline + fixture download script `extract/tests/fixtures/get_monaco.sh`

**Interfaces:**
- Produces: `nodes::NodeTable` with `fn load(pbf: &Path, max_nodes: u64) -> Result<Self, ExtractError>`, `fn get(&self, id: i64) -> Option<(f64, f64)>` (lon, lat), `fn len(&self) -> u64`. `ExtractError::TooManyNodes { seen: u64, max: u64 }` message includes "planet-scale extracts are not supported in v1".

- [ ] **Step 1: Add the fixture**

```bash
mkdir -p extract/tests/fixtures
curl -L -o extract/tests/fixtures/monaco.osm.pbf https://download.geofabrik.de/europe/monaco-latest.osm.pbf
ls -la extract/tests/fixtures/monaco.osm.pbf   # expect ~500-900 KB
git add extract/tests/fixtures/monaco.osm.pbf  # checked in: small, licence-fine (ODbL data used as test fixture with attribution note in tests/fixtures/README.md)
echo "Monaco extract © OpenStreetMap contributors, ODbL — test fixture" > extract/tests/fixtures/README.md
```

- [ ] **Step 2: Failing integration test** (`extract/tests/nodes_monaco.rs`):

```rust
use atlas_extract::nodes::NodeTable;
#[test]
fn monaco_nodes_load_and_resolve() {
    let t = NodeTable::load("tests/fixtures/monaco.osm.pbf".as_ref(), 10_000_000).unwrap();
    assert!(t.len() > 10_000, "monaco has tens of thousands of nodes, got {}", t.len());
    // every stored location is a sane coordinate
    // (probe: iterate first ways in Task 4's test instead; here just len + spot API shape)
}
```
(Requires `src/lib.rs` exposing modules; make `main.rs` thin over `lib.rs` now.)

- [ ] **Step 3: Implement** — `osmpbf::ElementReader::from_path`, match `Element::Node`/`Element::DenseNode`, store into `FxHashMap<i64, (f32, f32)>` (f32 pair = 8 bytes/node keeps NL ~60M nodes ≈ 1.4 GB incl. map overhead; document). Enforce `max_nodes` with the typed error. Run → PASS.

- [ ] **Step 4: Commit** — `feat(extract): pass-1a node location table (in-memory, capacity-guarded)`

---

### Task 4: Pass 1b — admin boundary assembly

**Files:**
- Create: `extract/src/boundaries.rs`
- Test: `extract/tests/boundaries_monaco.rs`

**Interfaces:**
- Consumes: `nodes::NodeTable`.
- Produces: `boundaries::AdminSet` — `fn load(pbf: &Path, nodes: &NodeTable) -> Result<Self, ExtractError>`; `pub struct AdminArea { pub name: String, pub admin_level: u8, pub rings: Vec<geo::Polygon<f64>> }`; `fn areas(&self) -> &[AdminArea]`. Also collects place NODES: `pub struct PlaceNode { pub name: String, pub place: String, pub population: u64, pub lon: f64, pub lat: f64, pub id: i64 }`, `fn place_nodes(&self) -> &[PlaceNode]`.

- [ ] **Step 1: Failing test** — Monaco must yield: ≥1 admin_level=2 area named "Monaco"; ≥5 place nodes; every assembled polygon has ≥4 points and closes (first==last after ring stitching).

```rust
use atlas_extract::{nodes::NodeTable, boundaries::AdminSet};
#[test]
fn monaco_admin_areas_assemble() {
    let nodes = NodeTable::load("tests/fixtures/monaco.osm.pbf".as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load("tests/fixtures/monaco.osm.pbf".as_ref(), &nodes).unwrap();
    assert!(admin.areas().iter().any(|a| a.admin_level == 2 && a.name == "Monaco"));
    assert!(admin.place_nodes().len() >= 5);
    for a in admin.areas() {
        for p in &a.rings { assert!(p.exterior().0.len() >= 4); }
    }
}
```

- [ ] **Step 2: Implement** — second streaming read: collect (a) ways referenced by admin relations (two sub-passes inside pass 1b: relations first to learn member way ids + roles, then ways to capture their node id lists), (b) `place=*` nodes with `name` (population parsed leniently: strip spaces/commas/dots, `parse().unwrap_or(0)`). Ring stitching: standard endpoint-matching merge of outer-role way node-lists into closed rings; DROP unclosed rings with a `eprintln!` warning counter (never panic). Build `geo::Polygon` per closed outer ring (inner/holes ignored in v1 — document: false-positive PIP inside holes accepted for v1). Run → PASS.

- [ ] **Step 3: Commit** — `feat(extract): pass-1b admin boundary assembly + place nodes (unclosed rings dropped, holes v2)`

---

### Task 5: Hierarchy index (H3-bucketed PIP + place-node fallback)

**Files:**
- Create: `extract/src/hierarchy.rs`
- Test: inline unit tests + extend `extract/tests/boundaries_monaco.rs`

**Interfaces:**
- Consumes: `boundaries::AdminSet`.
- Produces: `hierarchy::HierarchyIndex` — `fn build(admin: &AdminSet) -> Self`; `fn resolve(&self, lon: f64, lat: f64) -> Parents` where `pub struct Parents { pub locality: Option<String>, pub region: Option<String>, pub country: Option<String> }`. Level mapping: country = admin_level 2; region = best of 4 (else 3/5); locality = best of 8 (else 7/9/10, else nearest place node ≤ 10 km, city/town/village/hamlet only).

- [ ] **Step 1: Failing unit test with a synthetic square**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundaries::{AdminArea, AdminSet, PlaceNode};
    fn square(name: &str, level: u8, x0: f64, y0: f64, x1: f64, y1: f64) -> AdminArea {
        let ring = geo::Polygon::new(geo::LineString::from(vec![
            (x0,y0),(x1,y0),(x1,y1),(x0,y1),(x0,y0)]), vec![]);
        AdminArea { name: name.into(), admin_level: level, rings: vec![ring] }
    }
    #[test]
    fn resolve_uses_pip_then_place_fallback() {
        let admin = AdminSet::for_test(
            vec![square("Testland",2, 0.,0., 1.,1.), square("Mid",4, 0.,0., 1.,1.),
                 square("Town",8, 0.2,0.2, 0.4,0.4)],
            vec![PlaceNode{ name:"FallbackVille".into(), place:"village".into(),
                            population:100, lon:0.9, lat:0.9, id:1 }]);
        let idx = HierarchyIndex::build(&admin);
        let inside = idx.resolve(0.3, 0.3);
        assert_eq!(inside.locality.as_deref(), Some("Town"));
        assert_eq!(inside.region.as_deref(), Some("Mid"));
        assert_eq!(inside.country.as_deref(), Some("Testland"));
        let fallback = idx.resolve(0.9, 0.9); // no level-8 polygon here
        assert_eq!(fallback.locality.as_deref(), Some("FallbackVille"));
    }
}
```
(Add `AdminSet::for_test(areas, places)` constructor, `#[cfg(test)]`-gated is fine but the fixture tests also want it — make it `pub`.)

- [ ] **Step 2: Implement** — bucket each polygon under the H3 res-5 cells covering its bbox (`h3o::LatLng::to_cell` over a bbox lattice stepped at ~half a res-5 edge, dedup); `resolve` = point's res-5 cell → candidate polygons → `geo::Contains` ray-cast → smallest-area winner per level tier. Place fallback: grid-bucket place nodes at res-5 too; nearest within 10 km via k-ring 1 candidates + haversine. Run → PASS. Then extend the Monaco integration test: resolving the Casino de Monte-Carlo coordinate `(7.4247, 43.7394)` must give `country == Some("Monaco")`.

- [ ] **Step 3: Commit** — `feat(extract): hierarchy index — H3-bucketed PIP + place-node fallback`

---

### Task 6: Pass 2 — POI emitter

**Files:**
- Create: `extract/src/layers/poi.rs` (+ `extract/src/layers/mod.rs`)
- Test: `extract/tests/poi_monaco.rs`

**Interfaces:**
- Consumes: `NodeTable`, `HierarchyIndex`, `taxonomy`, `emit::LayerWriter`, `ids`.
- Produces: `layers::poi::extract(pbf, nodes, hier, out_dir) -> Result<u64>` writing `poi.geojsonl`. Property contract (mirrors extract_country_v3.py exactly): `carmen:text` = dedup-join of [name, "name locality", brand, "brand locality", every alias worth keeping], `carmen:center` [lon,lat], `carmen:score` (see scoring note), `popularity` = same value as carmen:score, plus optional `category`, `locality`, `brand`, `housenumber`. Way POIs use the centroid of their node locations. Scoring v1 (documented in code): base 100; +400 if `wikidata` or `wikipedia` tag present; +200 if brand present; ×1 otherwise — a deterministic open-data stand-in for the enriched popularity (converter only needs a rank-orderable number).

- [ ] **Step 1: Failing test** — extract Monaco POIs; assert: count > 300; a known POI exists (`Casino de Monte-Carlo`, category from taxonomy, has locality resolved); every line parses; every `carmen:text` non-empty; every id < 2^56.

- [ ] **Step 2: Implement.** Iterate elements; `is_poi` gate; nodes → point; ways → centroid via NodeTable (skip ways with <1 resolvable node, count skips); `Parents` from `hier.resolve`; write via LayerWriter with `hid(osm_sid(kind,id))`. Run → PASS.

- [ ] **Step 3: Converter smoke** — `cd converter && cargo run --release -- --src <monaco outdir> --dst /tmp/monaco_bundle` after also emitting empty-but-valid remaining layer files (LayerWriter creates them zero-length; verify the converter accepts zero-length layers — if not, note it and emit them in Task 8/9 order instead and move this smoke to Task 10).

- [ ] **Step 4: Commit** — `feat(extract): pass-2 POI emitter (taxonomy + hierarchy + centroid ways)`

---

### Task 7: Address emitter (grouped, carmen:addressnumber contract)

**Files:**
- Create: `extract/src/layers/address.rs`
- Test: `extract/tests/address_monaco.rs`

**Interfaces:**
- Produces: `layers::address::extract(...) -> Result<u64>` writing `address.geojsonl`. Contract (from extract_country_v3.py lines ~254-380): group by `(addr:street, locality)`; each group → Feature with `geometry: MultiPoint` of member coords, `carmen:addressnumber`: array of house numbers STRICTLY parallel (equal length) to the MultiPoint coordinates, `carmen:text` = "street,street locality", cap 2000 members per feature (split groups client-side, same as the python `cap=2000`). Group feature id = `hid("addr:" + street + ":" + locality + ":" + segment_index)`.

- [ ] **Step 1: Failing test** — Monaco: ≥1 address group; for every line: `len(carmen:addressnumber) == len(MultiPoint coords)`; no group exceeds 2000; a segment-split synthetic case (unit test with 4001 fake members → 3 features of 2000/2000/1).

- [ ] **Step 2: Implement** (collect `addr:housenumber`+`addr:street` nodes AND ways-with-address (centroid), group in a HashMap, sort members by hn for stable output, split, emit). Run → PASS.

- [ ] **Step 3: Commit** — `feat(extract): address emitter (grouped MultiPoint + parallel addressnumber, 2000-cap)`

---

### Task 8: Street, place, region, country emitters

**Files:**
- Create: `extract/src/layers/street.rs`, `extract/src/layers/place.rs`
- Test: `extract/tests/layers_monaco.rs`

**Interfaces:**
- `street::extract` — named highways (`highway` in {motorway..residential, pedestrian, living_street, unclassified, service-with-name}), grouped by `(name, locality)` like the python's street grouping; geometry = MultiPoint of way-midpoints per group (verify the python's exact street geometry choice at `extract_street` line ~316 and mirror it); props `carmen:text` = "name,name locality".
- `place::extract` — from pass-1b `PlaceNode`s: place.geojsonl (city/town/village/hamlet/suburb/quarter/neighbourhood), region.geojsonl (admin_level 4 areas as point features at ring centroid + `place=state/region` nodes), country.geojsonl (admin_level 2 centroid + name); score by population where present (`carmen:score = population`).

- [ ] **Step 1: Read `extract_street`/`extract_place`/`extract_region`/`extract_country` in scripts/extract_country_v3.py (lines ~316-470) and pin each geometry + text convention in test assertions first** (failing tests: Monaco has named streets ≥ 20; place file contains "Monte-Carlo"; country file contains exactly "Monaco").

- [ ] **Step 2: Implement all four; run → PASS.**

- [ ] **Step 3: Commit** — `feat(extract): street/place/region/country emitters`

---

### Task 9: Postcode emitter + poi_details.jsonl

**Files:**
- Create: `extract/src/layers/postcode.rs`, `extract/src/layers/details.rs`
- Test: extend `extract/tests/layers_monaco.rs`

**Interfaces:**
- `postcode::extract` — distinct `addr:postcode` values → one feature per code at the mean coordinate of its members; props `carmen:text` = the code (verify python `extract_postcode` line ~512 for the exact text/props shape).
- `details::extract` — `poi_details.jsonl` per the /details sidecar contract (docs/superpowers/specs/2026-07-14-details-sidecar-design.md): `{id: hid-sid, name, hours, phone[], website, email, socials{}, address}` from OSM tags ONLY (`opening_hours`, `phone`/`contact:phone`, `website`/`contact:website`, `email`/`contact:email`, `contact:instagram|facebook|twitter` → socials map, `addr:*` assembled). NO rating/reviews/price keys, ever (open product). Emit only when ≥1 detail field non-empty.

- [ ] **Step 1: Failing tests** — Monaco: postcode file has `98000`; details file: every line's id also exists in poi.geojsonl ids (build a HashSet in the test); no line contains the substrings `"rating"`, `"price"`, `"review"`.

- [ ] **Step 2: Implement both; run → PASS. Converter details smoke:** `convert --emit-details --src <outdir> --dst /tmp/monaco_bundle` succeeds.

- [ ] **Step 3: Commit** — `feat(extract): postcode emitter + OSM-tag details sidecar source`

---

### Task 10: CLI + end-to-end pipeline test

**Files:**
- Create: `extract/src/main.rs` (real CLI), `extract/tests/e2e_monaco.rs`
- Modify: `extract/src/lib.rs` (orchestration fn)

**Interfaces:**
- CLI: `atlas-extract --pbf <file> --out <dir> [--max-nodes N] [--details]`. Prints per-layer counts in the same style as the python (`[extract] poi: N rows -> path`).
- `lib::run(pbf, out, opts) -> Result<Summary>`; `Summary { per_layer: Vec<(String, u64)> }`.

- [ ] **Step 1: Failing e2e test** — run `lib::run` on Monaco into a temp dir; assert all 7 layer files + details exist and are non-empty except possibly region; then shell out to the converter (skip gracefully with eprintln if `converter/target/release/convert` absent — CI hint) and assert exit 0 and a manifest lands in the bundle dir.

- [ ] **Step 2: Implement CLI + orchestration (pass 1a → 1b → index build → pass 2 emitters in one binary run). Run → PASS.**

- [ ] **Step 3: Commit** — `feat(extract): CLI + monaco e2e (pbf -> layers -> bundle)`

---

### Task 11: Acceptance gate — NL parity run (semi-manual)

**Files:**
- Create: `docs/superpowers/plans/2026-07-14-atlas-extract-acceptance.md` (results log)

**Interfaces:** none (verification task).

- [ ] **Step 1:** `atlas-extract --pbf <NL extract from geofabrik> --out /tmp/nl_osm --details` (download NL pbf ~1.3 GB first; note wall time + peak RSS with `/usr/bin/time -l`).
- [ ] **Step 2:** `convert --src /tmp/nl_osm --dst /tmp/nl_bundle` + `--emit-details` → success; record bundle size vs the production nl bundle.
- [ ] **Step 3:** Upload to a THROWAWAY R2 prefix (`osmtest/v1`) with rclone; point a dev worker (wrangler `--env dev` or a `BUNDLE_PREFIX` var override) at it; run the 40-query NL live regression (`test/live_test.py` equivalent) — gate: ≥ 35/40 pass (5 allowed misses = enrichment-dependent popularity ranking cases; log each miss with a one-line reason).
- [ ] **Step 4:** Write the results log doc; commit. If gate fails → file the gaps as issues in the plan doc and STOP for review (do not tune ranking ad-hoc).

---

## Self-review notes (done at write time)

- Spec coverage: layer contract ✓ (T1,6-9), two-pass ✓ (T3-5), PIP ✓ (T5), node cache ✓ (T3, planet explicitly out per spec v1 memory target), taxonomy ✓ (T2), details-OSM-only ✓ (T9), converter-as-referee ✓ (T6/9/10), acceptance vs live regression ✓ (T11).
- Types consistent: `NodeTable::get -> Option<(f64,f64)>` used by T4/6/7; `Parents` fields used by T6-8; `LayerWriter::feature(id,&Map,Value)` used by all emitters.
- Known accepted gaps (documented in-code, not TBDs): polygon holes (v2), planet scale (v2), relation-type multipolygon POIs (v2 — nodes+ways cover the overwhelming majority).

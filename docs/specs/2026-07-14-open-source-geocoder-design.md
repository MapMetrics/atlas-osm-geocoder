# Open-Source OSM Geocoder Toolkit — Umbrella Design

Date: 2026-07-14
Status: approved direction (Jim, this session); sub-project 1 specced here,
2-4 get their own specs when reached.

## Vision

A public, self-hostable geocoder toolkit for the OSM community: feed it an
`osm.pbf` extract (region or planet), get the atlas-edge geocoder — search,
autocomplete, reverse, categories, details — running on your own Cloudflare
account (free tier viable for small regions). "Anyone can build this" is the
acceptance bar for every decision: no databases, no proprietary data, no
private tooling anywhere in the build path.

```
osm.pbf ──► atlas-extract ──► layer files ──► atlas-convert ──► bundle dir
                                                                   │
                                              wrangler deploy + rclone → R2
                                                                   ▼
                                                 your-geocoder.workers.dev
```

## What already qualifies (ships nearly as-is)

- **converter** (`convert`, Rust): geojsonl layers → FST/BM25/spatial bundle.
  Self-contained, no DB. Needs: CLI polish, docs.
- **worker** (Rust→wasm): the entire serving stack incl. stability hardening,
  BM25, fuzzy, budget, /details. Needs: enrichment-specific code paths gated
  or scrubbed (ext_/gext_ id handling stays but is inert on OSM data), config
  via wrangler.toml only.
- **demo site** (demo.html) and the bundle format (documented in code).

## What's missing: sub-project 1 — `atlas-extract` (pbf → layers)

The current chain (osm2pgsql → PostgreSQL → CSV → ClickHouse → extractor)
is replaced for the open product by ONE Rust binary in the same workspace.

- Crate: `extract/` using the `osmpbf` crate (streaming, planet-capable).
- Output: the EXACT layer contract the converter already consumes —
  `poi.geojsonl, address.geojsonl, street.geojsonl, place.geojsonl,
  region.geojsonl, country.geojsonl, postcode.geojsonl` and (optional)
  `poi_details.jsonl` from OSM tags (opening_hours, phone, website, email —
  OSM-tag fields only; no rating/review/price concepts in the open product).
- Two-pass streaming design:
  - **Pass 1 (boundaries + places):** collect `boundary=administrative`
    relations (admin_level 2..10) + `place=*` nodes; assemble polygons
    (way-stitching), index them into an H3-bucketed lookup (the same
    interior-cell + boundary-cell PIP trick as wof_h3_interior, in-memory or
    spilled to a temp mmap for planet scale).
  - **Pass 2 (features):** stream nodes/ways again; emit POIs (name or brand
    tag + category mapping from the existing pois_all.lua taxonomy, ported),
    addresses (addr:housenumber+street), streets (named highways, merged per
    name+locality like the current extractor's grouping), postcodes; resolve
    each feature's parent hierarchy (locality/region/country) via the pass-1
    PIP index.
  - Way geometry needs node locations: use osmpbf + a dense node-location
    cache (flat file, like osm2pgsql's flatnodes; sized by extract — planet
    ~90GB disk, a country extract ~MBs-GBs). `--locations-cache` path flag.
- Memory target: country extracts on a laptop (≤8 GB RAM); planet with the
  disk cache.
- Hierarchy quality note (accepted trade-off): OSM admin boundaries replace
  WoF names. Optional `--wof <dir>` enrichment can come later (open data,
  CC-BY attribution) — NOT in v1.
- Category taxonomy: port the pois_all.lua tag→category mapping into a Rust
  table (single source, unit-tested; documented for community edits).

## Sub-project 2 — public repo packaging

- NEW curated repo (no history import — the private repo's history contains
  enrichment work): `atlas-geocoder` (working name; final name TBD at repo
  creation — the only allowed TBD in this spec).
- Contents: extract/ + converter/ + worker/ + demo + docs + scripts
  (upload_bundle.sh, one-command Justfile/Makefile).
- Licence: code Apache-2.0 (patent-safe for corporate contributors); data
  produced is ODbL-derived (attribution guidance in README).
- Scrub checklist: no `poi_enrichment`/Google-merge references, no MapMetrics
  R2 credentials/bucket names, no gt_* proprietary test data (world mega-test
  harness CAN ship — its GT regenerates from any OSM build), telemetry OFF by
  default (WAE bindings optional).
- Sync model: public repo is a curated export; the private repo remains the
  enriched product. Shared-core drift is managed by periodic manual sync
  (upstreaming the worker/converter later is a separate decision, not v1).

## Sub-project 3 — hosted showcase

- osm.mapmetrics-atlas.net: second Worker service + R2 prefix (`osm/{gen}`),
  built from planet.pbf with atlas-extract ONLY (dogfood: if the showcase
  needs anything the public repo doesn't have, sub-project 1 isn't done).
- ODbL attribution in API responses and demo footer.

## Sub-project 4 — community UX

- README quickstart: pbf → deployed geocoder in ~5 commands.
- Region walkthrough (e.g. Netherlands, ~20 min on a laptop).
- R2/wrangler setup guide incl. free-tier limits table.
- CONTRIBUTING.md: category taxonomy edits, language/tokenizer additions.

## Order & gates

1 → (2 ∥ 3 dogfood) → 4. Gate for calling sub-project 1 done: an NL build via
atlas-extract passes the existing live regression suite (40 NL queries) at
parity minus enrichment-only fields, and a world-burst sample of the mega
harness runs against a showcase deploy without stability regressions.

## Out of scope (v1)

- WoF/ONSPD/OpenAddresses optional enrichment plugins (later, as opt-in
  downloads with their own attribution).
- Diff/minutely updates (rebuild-from-pbf is the v1 update model).
- Non-Cloudflare serving targets.

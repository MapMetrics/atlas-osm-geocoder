# atlas-extract NL Acceptance Run — Results Log

Date: 2026-07-14 evening
Gate: ≥35/45 on the live NL regression against a pure-OSM build. **RESULT: 24/45 — GATE FAILED, iteration required.** Per plan Task 11 Step 4: gaps filed below, no ad-hoc tuning.

## What unquestionably works

- **Full pipeline, one command**: `atlas-extract --pbf netherlands-latest.osm.pbf --out ... --details`
  → 11 min wall, 9.17 GB peak RSS (target was ≤8 GB — see gap G5), 495 warnings all legitimate
  (water boards without admin_level, cross-border ring drops).
- Layer counts sane: 268,313 POIs / 237,060 address groups / 1,187,552 streets / 10,672 places /
  32 regions / 3 countries (NL + border fragments) / **466,829 postcodes (real NL PC6 ≈ 459k — near-perfect)** /
  188,890 details records.
- Converter: full bundle in 10 s, 468 MB, manifest + details sidecar. Uploaded to
  `r2:atlas-osm-dev/nl/v7`, served by dev worker `atlas-osm-dev.jim9710.workers.dev`.
- Passing suites: autocomplete 4/4, category 2/2, batch 1/1, mapbox-compat 2/2, exact-POI search
  ("rijksmuseum" → Rijksmuseum), street+number addresses ("kalverstraat 1 amsterdam" ✓).
- Warm latency p50 ~140 ms — same class as production.

## Gaps (filed, prioritized)

**G1 — Score-scale mismatch between layers (causes ~12 of 21 failures; the big one).**
The v1 POI scorer (base 100, +400 wiki, +200 brand → up to 700) OUTSCORES the python-pinned
place formula (20·log10(pop+1), capped 250). So "Amsterdam" ranks a wiki-tagged POI above the
city; brand-near-city queries ("Jumbo Eindhoven") return the city or a rogue POI; equal-name
landmarks lose to distant twins (Anne Frank statue vs Huis). Fix direction: harmonize scales —
places must dominate POIs except true landmarks (production: city popularity ≫ POI popularity;
only enriched landmark scores compete). Concretely: uncap places (or raise cap ≈ 1000·log-pop)
and/or divide POI scores; pin relative ordering in unit tests (city > wiki-POI > brand-POI > plain).

**G2 — Fuzzy returns zero results (3 failures).** "Ritksmuseum"/"Rijksmueum"/"Krudvat" → empty at
~300 ms. Dict FSTs exist (bundle has 27 poi dict shards). Suspect: manifest field the worker's
fuzzy path gates on (tokenizer version? layer list?) differs from production manifests. Needs a
manifest diff vs production nl/v7 + a worker-log trace ("fuzzy: fired/skipped reason=...").

**G3 — Reverse geocoding 0/3 (top=none).** Worker's reverse path may require spatial shards for
more layers than the converter emitted from our layers (log showed fat spatial for poi only) or
the production bridge tables. Needs: check what production nl bundle contains spatially
(`rclone lsl r2:carmen-edge/nl/v7 | grep spatial`) vs ours, then emit parity.

**G4 — Glued NL postcode "3081RM" misses (1 failure).** Postcode features carry python-pinned
props (no carmen:score); need to verify the postcode doc text form ("3081 RM" spaced) matches
the worker's glued-split expectations, and whether the layer bonus suffices without a score.

**G5 — Peak RSS 9.17 GB > 8 GB target.** NodeTable holds ~120M NL nodes (~3 GB) + boundary
buffers + address grouping maps concurrently. Options: drop node table after pass-2 geometry
resolution stages, stream address grouping to disk, or accept and re-document the target.

**G6 (minor) — "Amsterdam Centraal" vs OSM name "Centraal Station"**: alias/name-variant gap —
OSM `name` differs from common query form. Production wins via enrichment names. Potential:
include `alt_name`/`short_name`/`official_name` tags as aliases in carmen:text.

## Verdict

The toolkit's mechanics are proven end-to-end (pbf → live geocoder in ~15 min). The remaining
work is *ranking calibration and two worker-compat investigations* — normal search-quality
iteration, not architectural failure. G1 alone likely brings the suite to ~33-36/45.

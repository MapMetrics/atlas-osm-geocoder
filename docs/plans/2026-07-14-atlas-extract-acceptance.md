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

## G1 fix round (2026-07-14, later same day)

**RESULT: still 24/45 — score unchanged.** G1+G6 were implemented and verified correct at the
data layer, but the live regression score didn't move, because **the 21 failures are not what
G1 predicted them to be**: cross-layer text/coverage ties, not popularity-scale domination.
Full detail below; commit `1c77efa`.

### What was fixed and confirmed correct

- **poi_score() recalibrated**: plain POI 0 (was 100), brand +40 (was +200), wiki +90 (was
  +400), stack 130 (was 700) — derived from production's `poi_score()` doc comment ("~53% zero,
  mass in 100-999, rare 8000+ landmarks") and the worker's `layer_bonus`/`text_bonus`/landmark
  threshold (`carmen:score >= 8000`). Cross-layer ordering pinned in a unit test against
  `place_score_from_pop`: city(800k)=118 > wiki-POI=90 > brand-POI=40 > plain-POI=0, and
  wiki-POI=90 > village(500)=54. NL rebuild score distribution: 77.5% zero / 17.8% brand /
  4.5% wiki / 0.1% both — directionally matches production's documented shape.
- **G6 alt_name/short_name/official_name aliases**: confirmed end-to-end against real OSM data.
  `alt_name=Kazerne Amsterdam Victor` on node "Kazerne Victor" → carmen:text
  `"Kazerne Victor,Kazerne Victor Amsterdam,Kazerne Amsterdam Victor,fire_station"`.
  `official_name=Basisschool Al Ummah` on node "Al Ummah" → carmen:text
  `"Al Ummah,Al Ummah Enschede,Basisschool Al Ummah,school"`. Both correctly inserted after
  name/"name locality", before brand.
- **Direct verification the G1 mechanism is fixed**: "Anne Frank" query — the wiki-tagged
  memorial/artwork POIs near the real Anne Frank House now score 90 (were 700, previously
  *outscoring* the correct answer via raw popularity). "Amsterdam" with `limit=5` and
  `debug=score` correctly returns the city (score 16.27, popularity 119) as #1, ahead of any POI.

### Why the regression score didn't move: two confounds found

1. **`BUNDLE_PREFIX` mismatch (process bug, not a code bug).** `worker/wrangler.osmdev.toml`
   pins `BUNDLE_PREFIX = "nl/v1"` (git blame: unchanged since the file's creation this same day,
   commit `c15de26`) and the worker reads *only* that env var with no per-request override. The
   task brief's instruction to sync to `r2:atlas-osm-dev/nl/v7` would have uploaded to a path the
   worker never reads — `nl/v1` was empty before this round. **The original 24/45 baseline run's
   claim of "uploaded to nl/v7, served by dev worker" appears to be a documentation error** (or
   the config differed transiently and was never committed); the worker can only ever have served
   `nl/v1`. Corrected: synced the new bundle to `r2:atlas-osm-dev/nl/v1` (627 files, ~466 MB,
   first real upload to that path) and redeployed. **Recommendation**: fix `BUNDLE_PREFIX` in
   `wrangler.osmdev.toml` to match whatever path is actually intended, or standardize future dev
   uploads on `nl/v1` to avoid re-hitting this. This session did not touch the worker's code or
   config file per the task's explicit boundary, only the R2 upload target.
2. **The live 21 failures are dominated by cross-layer text/coverage ties the worker's own BM25
   path produces, not by popularity-scale domination** (G1's exact diagnosis was right for *why*
   POIs used to beat places on raw score — that mechanism is now fixed — but it wasn't the only
   or even the primary cause of most of these 21 fails):
   - **"Amsterdam" at `limit=1`**: returns `'T Veldje` (a zoo, pop 100), while the *same query at
     `limit=5`* correctly returns Amsterdam (pop 119) first. This is a `limit`-dependent
     candidate-pool-truncation artifact in the worker (per-layer/pool caps scale with `limit`,
     e.g. `(limit*20).max(30)`, `(limit*2).max(10)` in several rescoring sites) — reproducible,
     independent of this round's changes, and the live_test.py golden-set harness calls with
     `limit=1` exactly, so it hits this path on every single-result assertion. Worker code —
     out of scope this round.
   - **"Amsterdam Centraal"**: a **street** literally named "Centraal Station" (`coverage=1.0`,
     full name match) outranks the actual train-station POI (`coverage=0.5` — the query
     "amsterdam centraal" partially matches the POI's "Amsterdam Centraal"/"Centraal Station"
     alias set but not as a full-string match). G6's alias fix is present and correct in the
     data (confirmed above); the loss is a street-vs-poi text-coverage tie, not an alias gap.
   - **"Anne Frank"**: now loses to *streets* named "Anne Frank" (full-coverage exact street-name
     match at `layer_bonus=1.0`) rather than to a wiki-inflated POI as before — the G1 failure
     mode is gone, replaced by a different (also worker-side, cross-layer) one.
   - **"Jumbo Eindhoven" / "Kruidvat Rotterdam"**: now return the *city* (Eindhoven/Rotterdam) as
     top result rather than "the city or a rogue POI" — an improvement in kind (no more rogue
     POI), but still not the intended brand POI, because a two-token brand+locality query's
     partial coverage on the city name edges out the POI's own partial coverage. This is a
     multi-token coverage/IDF interaction, not a popularity-scale issue — outside G1's specific
     claim.
   - **G2 (fuzzy zero results), G3 (reverse 0/3), G4 (postcode)**: unchanged, exactly as filed —
     confirmed still present, untouched, out of scope this round.

### Per-suite comparison (baseline → this round)

| Suite | Baseline | This round | Delta |
|---|---|---|---|
| Search (golden set) | 11/23 | 11/23 | 0 |
| Fuzzy Levenshtein | 2/5 | 2/5 | 0 |
| Autocomplete | 4/4 | 4/4 | 0 |
| Reverse geocoding | 0/3 | 0/3 | 0 |
| Category search | 2/2 | 2/2 | 0 |
| Batch geocoding | 1/1 | 1/1 | 0 |
| Mapbox compat | 2/2 | 2/2 | 0 |
| BM25 hybrid | 2/5 | 2/5 | 0 |
| **Overall** | **24/45 (53%)** | **24/45 (53%)** | **0** |

Identical failure set, identical pass/fail per query. Latency unaffected (median ~175-266 ms,
consistent with baseline's ~140-266 ms class).

### What remains / recommended next steps

- **G1/G6 are done and correct** — verified at the data layer (score distribution, unit tests,
  raw carmen:text/carmen:score inspection) and confirmed to fix their specifically-diagnosed
  failure mechanism (POI raw-score domination). They did not move the regression score because
  the regression suite's failures turn out to be dominated by *other* ranking mechanics
  (limit=1 pool truncation, cross-layer coverage ties on multi-token and street-collision
  queries) that G1/G6 were never going to touch.
  - **Fix the BUNDLE_PREFIX / upload-path mismatch first** in any future iteration — otherwise
    changes may silently not reach the served worker again.
  - **New gap, worth filing**: `limit=1` produces different (worse) top-1 results than
    `limit=3`/`limit=5` for the same query on the BM25 rescoring path — reproducible on
    "Amsterdam" (`'T Veldje` at limit=1 vs `Amsterdam` at limit=5). Since `live_test.py`'s
    primary golden-set suite (23 of 45 tests, the largest bucket) calls with `limit=1`
    specifically, this alone may be suppressing a meaningful fraction of the score. Worth
    investigating before further extract-side tuning, since no amount of score calibration in
    `extract/` can fix a pool-truncation bug in the worker.
  - G2/G3/G4 remain exactly as filed — untouched, still the likely next-highest-value fixes
    (worker-side manifest/spatial-shard/reverse-path investigations), still out of scope for
    extract-only work.

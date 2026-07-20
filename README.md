# atlas-osm-geocoder

Self-hostable geocoder for OpenStreetMap data: feed it an `osm.pbf` extract,
get a fast search/autocomplete/reverse geocoding API running on Cloudflare
Workers (free tier viable for small regions).

```
osm.pbf ──► atlas-extract ──► layer files ──► convert ──► bundle ──► your worker
```

Status: early development.

## Documentation

- **[Getting started](docs/getting-started.md)** — use the hosted API in 3 steps (curl / JS / Python)
- **[API reference](docs/api.md)** — every endpoint, parameter, and response field
- **[Self-hosting guide](docs/self-hosting.md)** — run your own, step by step (prerequisites → data → deploy → verify)

## Hosted free tier

**▶ Try it live:** https://osm-geo-demo.mapmetrics-atlas.net — an interactive
demo (map, search, autocomplete, reverse) running on the hosted instance.

We run a free, keyed public instance of this geocoder. Limits per API key:

| limit | value | window |
|-------|-------|--------|
| Rate | **10 requests / second** | burst-smoothed |
| Per key | **10,000 requests / day** | resets daily |
| Global | **2,000,000 requests / month** | shared across all free keys, resets monthly |

Reaching a limit returns HTTP `429`. **Need more?** This project is fully
open source — **self-host it and there are no limits** (you run it on your own
Cloudflare account; the guide below gets you there in ~5 commands). The hosted
tier is a convenience and a demo, not the intended path for heavy or commercial
use.

Get a free key: sign up at https://portal.mapmetrics.org and create an
API key with the `osm-geocode` scope. Call it at
`https://gateway.mapmetrics-atlas.net/osm-geocode/?token=YOUR_KEY&q=...`.

### Want more than OSM? Try the enriched V2 geocoder

This toolkit and the free tier serve **pure OpenStreetMap** data. If you need
broader coverage, the **enriched V2 geocoder** adds **millions of extra POIs** —
businesses, brands, and places that aren't in OSM — on top of the same fast
search, autocomplete, and reverse geocoding. It's a paid tier: sign up at
https://portal.mapmetrics.org, use the `geocode` scope, and call
`https://gateway.mapmetrics-atlas.net/v2/forward-geocode/?token=YOUR_KEY&q=...`
(add `&format=v1` for a Pelias-compatible response shape).

## API

Quick reference below — full details, error codes, and code samples in
**[docs/api.md](docs/api.md)**. Your deployed worker exposes these endpoints at its own origin. All responses are
GeoJSON-style `FeatureCollection`s. On the hosted free tier, `search` and `reverse`
are wrapped behind the gateway as `/osm-geocode/` and `/osm-reverse/` (add
`?token=YOUR_KEY`).

### `GET /search` — full-text geocoding

Place, address, POI, or postcode search.

| param | required | description |
|-------|:--------:|-------------|
| `q` | ✓ | query text — `Elfhuizen 9`, `Rijksmuseum Amsterdam`, `3011 Rotterdam` |
| `limit` | | max results (default 10) |
| `proximity` | | `lon,lat` — bias ranking toward a point |
| `types` | | comma-separated layer filter: `poi,place,address,postcode,region,country` |

```bash
# self-hosted:
curl "https://YOUR-WORKER/search?q=Elfhuizen%209&proximity=4.66,51.81"
# hosted free tier:
curl "https://gateway.mapmetrics-atlas.net/osm-geocode/?token=YOUR_KEY&q=Elfhuizen%209"
```

```json
{
  "type": "FeatureCollection",
  "query": ["elfhuizen", "117"],
  "features": [
    {
      "id": "address.43025135157773171",
      "place_type": ["address"],
      "relevance": 1.0,
      "text": "Elfhuizen 9",
      "place_name": "Elfhuizen 9, Dordrecht",
      "center": [4.664717, 51.812515],
      "geometry": { "type": "Point", "coordinates": [4.664717, 51.812515] },
      "context": [
        { "id": "place.5416240181831543792", "text": "Dordrecht" },
        { "id": "country.nl", "text": "Netherlands", "short_code": "nl" }
      ],
      "properties": { "housenumber": "117", "layer": "address" }
    }
  ]
}
```

### `GET /autocomplete` — typeahead

Low-latency prefix search for as-you-type UIs. Same params as `/search`. Returns
`results` (not `features`); each result adds a `matching_text` field.

```json
{
  "type": "FeatureCollection",
  "query": "rijksm",
  "results": [
    {
      "id": "poi.37355879192731132",
      "place_type": ["poi"],
      "text": "Rijksmuseum",
      "place_name": "Rijksmuseum, Amsterdam",
      "center": [4.8853736, 52.360065],
      "properties": { "category": "museum", "layer": "poi" },
      "matching_text": "Rijksm"
    }
  ]
}
```

### `GET /reverse` — coordinates → nearest place

| param | required | description |
|-------|:--------:|-------------|
| `lon` | ✓ | longitude |
| `lat` | ✓ | latitude |
| `limit` | | max results |

```bash
curl "https://YOUR-WORKER/reverse?lon=4.66&lat=51.81"
# hosted: https://gateway.mapmetrics-atlas.net/osm-reverse/?token=YOUR_KEY&lon=4.66&lat=51.81
```

Returns a `FeatureCollection` of the nearest features (same shape as `/search`).

### Response fields

| field | meaning |
|-------|---------|
| `text` | short name |
| `place_name` | full display name (`name, locality`) |
| `center` | `[lon, lat]` |
| `place_type` / `properties.layer` | `poi` \| `place` \| `address` \| `postcode` \| `region` \| `country` |
| `relevance` | 0–1 match score |
| `context[]` | parent hierarchy (locality, country) |
| `properties.housenumber` | present on address results |
| `properties.category` | POI category (e.g. `museum`, `restaurant`) |
| `id` | stable feature id — `<layer>.<id>` |

---

## Run your own — download the prebuilt world (no build required)

> Full step-by-step (prerequisites, R2 setup, deploy, verify):
> **[docs/self-hosting.md](docs/self-hosting.md)**.

Skip the PBF crunching: download the ready-made planet bundle, upload it to your
own Cloudflare R2, and deploy the worker. One file — **~6.4 GB** compressed
(~22 GB extracted, 181 countries, generation `v7b`).

```bash
# 1. Download the whole planet
curl -O https://pub-76c6897ca8ee46a78ba0827d502d1456.r2.dev/world-v7b.tar.zst

# 2. Extract -> produces {cc}/v7b/... exactly as the worker expects
tar --zstd -xf world-v7b.tar.zst

# 3. Upload to YOUR R2 bucket (parallel; this is the slow part, ~194k objects)
rclone copy . r2:your-bucket --transfers 64 --checkers 64

# 4. Deploy the worker pointing at your bucket (BUNDLE = your-bucket, BUNDLE_GEN = v7b)
wrangler deploy
```

The archive preserves the `{cc}/v7b/` layout, so step 3 lands the keys exactly
where the worker reads them — no renaming, no CLI. It's a full planet copy, so
your geocoder works worldwide out of the box.

## Build your own from a PBF (unlimited, any region)

Prefer to build from scratch — or want just one region instead of the planet?
Feed `atlas-extract` an `osm.pbf`, run `convert`, deploy the
worker to your own Cloudflare account. No keys, no caps, no cost to us or you
beyond your own Cloudflare usage (free tier covers small regions).

Code: Apache-2.0. Data you build with it is derived from OpenStreetMap —
© OpenStreetMap contributors, ODbL. Attribution required in your products.

# atlas-osm-geocoder

Self-hostable geocoder for OpenStreetMap data: feed it an `osm.pbf` extract,
get a fast search/autocomplete/reverse geocoding API running on Cloudflare
Workers (free tier viable for small regions).

```
osm.pbf ──► atlas-extract ──► layer files ──► convert ──► bundle ──► your worker
```

Status: early development. See docs/specs/ and docs/plans/.

## Hosted free tier

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

Get a free key: <sign-up URL TBD>.

## Run your own — download the prebuilt world (no build required)

Skip the PBF crunching: download the ready-made planet bundle, upload it to your
own Cloudflare R2, and deploy the worker. One file — ~14 GB compressed (~21 GB
extracted, 181 countries, generation `v7b`).

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

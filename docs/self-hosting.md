# Self-hosting guide

Run your own geocoder on your own Cloudflare account — no keys, no caps, no cost
to us. There are two ways to get the bundle data: **download the prebuilt world**
(fast) or **build it from a PBF** (any region, fully from source).

> **Status (read first).** This repo currently ships the **`extract`** stage and
> the prebuilt **data**. The **`convert`** step and the **serving worker** are
> being prepared for release. Until they land: the *download-prebuilt* path gets
> you a bucket full of ready-to-serve data, and the worker to point at it is the
> last piece to publish. The build-from-PBF path runs `extract` today;
> `convert` follows. This page documents the full intended flow end to end.

---

## Prerequisites

- A **Cloudflare account** (the free plan is enough for small regions).
- **Wrangler** — `npm i -g wrangler`, then `wrangler login`.
- **rclone** — for fast bulk upload to R2 (`brew install rclone` / see rclone.org).
- For the build path only: **Rust** (`rustup`) and an `osm.pbf` extract
  (from [Geofabrik](https://download.geofabrik.de/)).

### One-time: create your R2 bucket + rclone remote

```bash
# create the bucket
wrangler r2 bucket create my-geocoder

# configure an rclone S3 remote for R2 (needs an R2 API token: Cloudflare
# dashboard -> R2 -> Manage API Tokens -> create, note the access key + secret)
rclone config create r2 s3 provider=Cloudflare \
  access_key_id=YOUR_R2_KEY secret_access_key=YOUR_R2_SECRET \
  endpoint=https://YOUR_ACCOUNT_ID.r2.cloudflarestorage.com
```

---

## Path A — download the prebuilt world (fastest)

One file, the whole planet, ready to serve.

```bash
# 1. Download (~6.4 GB compressed, ~22 GB extracted, 181 countries, gen v7b)
curl -O https://pub-76c6897ca8ee46a78ba0827d502d1456.r2.dev/world-v7b.tar.zst

# 2. Extract -> produces {cc}/v7b/... exactly as the worker expects
tar --zstd -xf world-v7b.tar.zst

# 3. Upload to YOUR R2 bucket. This is the slow step (~194k small objects);
#    high parallelism helps a lot.
rclone copy . r2:my-geocoder --transfers 64 --checkers 64
```

Want just one country instead of the planet? Upload only that prefix, e.g.
`rclone copy nl r2:my-geocoder/nl`. The worker serves whatever countries are
present and returns empty results (or falls back) for the rest.

---

## Path B — build from a PBF (any region, from source)

```bash
# 1. Get a regional extract
curl -O https://download.geofabrik.de/europe/netherlands-latest.osm.pbf

# 2. Extract layer files from the PBF  (this repo's `extract` crate)
cd extract
cargo run --release -- ../netherlands-latest.osm.pbf --out ../build/nl

# 3. Convert layer files -> serving bundle   (convert step — see Status above)
convert ../build/nl --out ../bundle/nl/v7b

# 4. Upload the bundle to your R2 bucket
rclone copy ../bundle r2:my-geocoder --transfers 64
```

---

## Deploy the worker

Point the serving worker at your bucket. Its `wrangler.toml` needs an R2
binding named `BUNDLE` and the bundle generation:

```toml
name = "my-geocoder"
main = "build/worker/shim.mjs"       # the serving worker
compatibility_date = "2025-11-01"

[[r2_buckets]]
binding = "BUNDLE"
bucket_name = "my-geocoder"

[vars]
BUNDLE_PREFIX   = "nl/v1"   # default/primary layout marker
BUNDLE_GEN_OVERRIDE = "v7b" # the generation you uploaded
```

```bash
wrangler deploy
```

> The worker binary is the piece still being prepared for open release
> (see Status). Once published, this section deploys it unchanged.

---

## Verify

```bash
# search
curl "https://my-geocoder.YOUR-SUBDOMAIN.workers.dev/search?q=Elfhuizen%209&proximity=4.66,51.81"
# reverse
curl "https://my-geocoder.YOUR-SUBDOMAIN.workers.dev/reverse?lon=4.66&lat=51.81"
# health
curl "https://my-geocoder.YOUR-SUBDOMAIN.workers.dev/health"
```

A correct search returns a `FeatureCollection` whose top feature's `place_name`
matches your query. See **[api.md](api.md)** for the full response shape.

---

## Notes

- **No API keys or rate limits** on your own deployment — you own the whole
  stack. Add your own auth/limits if you expose it publicly.
- **R2 egress is free**, so serving costs are just storage + Class B reads;
  the Cloudflare free tier covers small regions comfortably.
- **License**: code is Apache-2.0; the data is derived from OpenStreetMap —
  **© OpenStreetMap contributors, ODbL**. You must attribute OSM in any product
  built on it, and share-alike applies to derived databases.

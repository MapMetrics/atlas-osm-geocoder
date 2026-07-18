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

## Self-host = unlimited

The whole point: feed `atlas-extract` an `osm.pbf`, run `convert`, deploy the
worker to your own Cloudflare account. No keys, no caps, no cost to us or you
beyond your own Cloudflare usage (free tier covers small regions).

Code: Apache-2.0. Data you build with it is derived from OpenStreetMap —
© OpenStreetMap contributors, ODbL. Attribution required in your products.

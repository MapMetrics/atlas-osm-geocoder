# atlas-osm-geocoder

Self-hostable geocoder for OpenStreetMap data: feed it an `osm.pbf` extract,
get a fast search/autocomplete/reverse geocoding API running on Cloudflare
Workers (free tier viable for small regions).

```
osm.pbf ──► atlas-extract ──► layer files ──► convert ──► bundle ──► your worker
```

Status: early development. See docs/specs/ and docs/plans/.

Code: Apache-2.0. Data you build with it is derived from OpenStreetMap —
© OpenStreetMap contributors, ODbL. Attribution required in your products.

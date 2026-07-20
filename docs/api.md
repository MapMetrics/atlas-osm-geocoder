# API reference

All endpoints return GeoJSON-style `FeatureCollection` JSON.

**Base URLs**
- **Hosted free tier** (via gateway, needs `?token=YOUR_KEY`):
  `https://gateway.mapmetrics-atlas.net/osm-geocode/` (search) and
  `https://gateway.mapmetrics-atlas.net/osm-reverse/` (reverse).
- **Self-hosted**: your worker's origin, with the raw paths below
  (`/search`, `/autocomplete`, `/reverse`, …).

---

## `GET /search` — full-text geocoding

Search places, addresses, POIs, and postcodes.

| param | required | description |
|-------|:--------:|-------------|
| `q` | ✓ | query text — `Elfhuizen 9`, `Rijksmuseum Amsterdam`, `3011 Rotterdam` |
| `limit` | | max results (default 10) |
| `proximity` | | `lon,lat` — bias ranking toward a point |
| `types` | | comma-separated layer filter: `poi,place,address,postcode,region,country` |
| `country` | | restrict to one ISO country code, e.g. `nl` |

**Example**

```bash
curl "https://gateway.mapmetrics-atlas.net/osm-geocode/?token=YOUR_KEY&q=Elfhuizen%209&proximity=4.66,51.81"
```

```json
{
  "type": "FeatureCollection",
  "query": ["elfhuizen", "9"],
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
      "properties": { "housenumber": "9", "layer": "address" }
    }
  ]
}
```

---

## `GET /autocomplete` — typeahead

Low-latency prefix search for as-you-type UIs. Takes the same params as
`/search`. **Returns `results`** (not `features`); each result adds a
`matching_text` field showing the matched prefix.

> Note: on the hosted free tier the gateway currently fronts only `search`
> and `reverse`. `autocomplete` is available on **self-hosted** deployments.

```bash
curl "https://YOUR-WORKER/autocomplete?q=rijksm&limit=6"
```

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

---

## `GET /reverse` — coordinates → nearest place

| param | required | description |
|-------|:--------:|-------------|
| `lon` | ✓ | longitude |
| `lat` | ✓ | latitude |
| `limit` | | max results (default 1) |

```bash
curl "https://gateway.mapmetrics-atlas.net/osm-reverse/?token=YOUR_KEY&lon=4.66&lat=51.81"
```

Returns a `FeatureCollection` of the nearest features (same shape as `/search`).

---

## Response fields

| field | meaning |
|-------|---------|
| `text` | short name |
| `place_name` | full display name (`name, locality`) |
| `center` | `[lon, lat]` |
| `geometry` | GeoJSON point (`center` as a geometry) |
| `place_type` / `properties.layer` | `poi` \| `place` \| `address` \| `postcode` \| `region` \| `country` |
| `relevance` | 0–1 match score (1.0 = exact) |
| `context[]` | parent hierarchy — locality then country (`short_code`) |
| `properties.housenumber` | present on `address` results |
| `properties.category` | POI category, e.g. `museum`, `restaurant`, `supermarket` |
| `properties.popularity` | relative importance signal |
| `id` | stable feature id — `<layer>.<id>` |

## Status & error codes

| code | meaning |
|------|---------|
| `200` | success (may be an empty `features`/`results` array for no match) |
| `400` | missing/invalid parameter (e.g. reverse without `lon`) |
| `401` | missing or invalid API key (hosted tier) |
| `403` | key scope or allowed-website check failed (hosted tier) |
| `429` | rate limit / quota reached — back off and retry (`Retry-After` header) |

## Rate limits (hosted free tier)

10 req/s per key · 10,000 req/day per key · 2,000,000 req/month global.
Self-hosted deployments have no such limits.

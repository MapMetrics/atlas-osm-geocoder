# Getting started (hosted free tier)

The fastest way to use the geocoder — no infrastructure, just an API key.

## 1. Try the live demo

Open **https://osm-geo-demo.mapmetrics-atlas.net** — an interactive map with
search, autocomplete, and reverse geocoding running on the hosted instance.

## 2. Get an API key

1. Sign up at **https://portal.mapmetrics.org**.
2. Create an API key and give it the **`osm-geocode`** scope.
3. (Optional but recommended) Lock the key to your site with an **allowed
   website** — set it to your domain's origin, e.g. `https://yourapp.com`
   (no trailing slash). Then the key only works from that origin. Leave it
   empty for server-side use.

## 3. Make your first request

The hosted geocoder lives behind the gateway. Pass your key as `?token=`.

```bash
curl "https://gateway.mapmetrics-atlas.net/osm-geocode/?token=YOUR_KEY&q=Rijksmuseum%20Amsterdam"
```

### JavaScript (browser or Node)

```js
const KEY = "YOUR_KEY";
const res = await fetch(
  `https://gateway.mapmetrics-atlas.net/osm-geocode/?token=${KEY}&q=${encodeURIComponent("Rijksmuseum Amsterdam")}`
);
const data = await res.json();
const top = data.features[0];
console.log(top.place_name, top.center); // "Rijksmuseum, Amsterdam" [4.8853736, 52.360065]
```

### Python

```python
import requests

KEY = "YOUR_KEY"
r = requests.get(
    "https://gateway.mapmetrics-atlas.net/osm-geocode/",
    params={"token": KEY, "q": "Rijksmuseum Amsterdam"},
)
top = r.json()["features"][0]
print(top["place_name"], top["center"])  # Rijksmuseum, Amsterdam [4.8853736, 52.360065]
```

### Reverse geocoding (coordinates → place)

```bash
curl "https://gateway.mapmetrics-atlas.net/osm-reverse/?token=YOUR_KEY&lon=4.885&lat=52.360"
```

## 4. Bias results to a location (recommended)

Pass `proximity=lon,lat` (usually your map centre or the user's location) so
ambiguous queries resolve to the nearest match:

```bash
curl "https://gateway.mapmetrics-atlas.net/osm-geocode/?token=YOUR_KEY&q=oxford%20street&proximity=-0.14,51.51"
```

## Limits (free tier)

| limit | value |
|-------|-------|
| Rate | 10 requests / second per key |
| Per key | 10,000 requests / day |
| Global | 2,000,000 requests / month (shared) |

Over a limit → HTTP `429`. Need more? **Self-host** (unlimited — see
[self-hosting.md](self-hosting.md)) or use the enriched **V2 geocoder** (millions
more POIs — see the main [README](../README.md)).

## Full API

See **[api.md](api.md)** for every endpoint, parameter, and response field.

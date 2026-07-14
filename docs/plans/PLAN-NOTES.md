Plan repo-context notes (2026-07-14):
- Repo root = this repo (atlas-osm-geocoder). All `extract/...` paths in
  2026-07-14-atlas-extract.md are relative to THIS root.
- The converter smoke steps invoke the private-repo binary during development
  via env var: CONVERT_BIN=/Volumes/T7/osm.pbfconverter/atlas-edge/converter/target/release/convert
  Tests must skip gracefully (eprintln) when CONVERT_BIN is unset/absent.
- pois_all.lua reference source: /Volumes/T7/osm.pbfconverter/pois_all.lua (read-only).

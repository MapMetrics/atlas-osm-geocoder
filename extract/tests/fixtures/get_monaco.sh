#!/usr/bin/env bash
# Re-download the Monaco test fixture (checked in; re-run only to refresh it).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
curl -L -o monaco.osm.pbf https://download.geofabrik.de/europe/monaco-latest.osm.pbf
ls -la monaco.osm.pbf

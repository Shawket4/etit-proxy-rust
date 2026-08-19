#!/usr/bin/env bash
# Build the Egypt truck OSRM dataset (MLD pipeline) with docker.
#
# Produces egypt-truck.osrm* files in $BUILD_DIR, ready for:
#   osrm-routed --algorithm mld /data/egypt-truck.osrm
#
# Usage:
#   ./build.sh                          # build into ./osrm-build
#   BUILD_DIR=/opt/osrm ./build.sh      # build elsewhere
#
# Env overrides:
#   OSRM_IMAGE  docker image        (default osrm/osrm-backend:latest — same as VPS)
#   BUILD_DIR   output directory    (default ./osrm-build)
#   PBF_URL     OSM extract source  (default geofabrik egypt-latest)
#
# RAM note: osrm-extract on the Egypt extract needs roughly 6-8 GB available
# to docker. If the docker VM/daemon has less, extract gets OOM-killed
# (exit 137).

set -euo pipefail

OSRM_IMAGE="${OSRM_IMAGE:-osrm/osrm-backend:latest}"
BUILD_DIR="${BUILD_DIR:-$(pwd)/osrm-build}"
PBF_URL="${PBF_URL:-https://download.geofabrik.de/africa/egypt-latest.osm.pbf}"
BASENAME="egypt-truck"   # distinct from the car dataset so both can coexist

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROFILE="$SCRIPT_DIR/truck.lua"

[ -f "$PROFILE" ] || { echo "truck.lua not found next to build.sh" >&2; exit 1; }
mkdir -p "$BUILD_DIR"

# 1. Download the Egypt extract (resumable). Geofabrik throttles per
#    connection at times; -C - lets you just re-run on interruption.
if [ ! -f "$BUILD_DIR/egypt-latest.osm.pbf" ]; then
  echo "== Downloading Egypt extract =="
  curl -fL --retry 5 --retry-all-errors -C - \
    -o "$BUILD_DIR/egypt-latest.osm.pbf.part" "$PBF_URL"
  mv "$BUILD_DIR/egypt-latest.osm.pbf.part" "$BUILD_DIR/egypt-latest.osm.pbf"
fi

# 2. Rename to the truck basename: osrm-extract derives all output names from
#    the input file name, so the copy is what makes egypt-truck.osrm*.
cp -f "$BUILD_DIR/egypt-latest.osm.pbf" "$BUILD_DIR/$BASENAME.osm.pbf"

# 3. The profile is mounted into /opt so its require('lib/...') statements
#    resolve against the stock profile libs shipped in the image (/opt/lib).
run() {
  docker run --rm -t \
    -v "$BUILD_DIR:/data" \
    -v "$PROFILE:/opt/truck.lua:ro" \
    "$OSRM_IMAGE" "$@"
}

echo "== osrm-extract =="
run osrm-extract -p /opt/truck.lua "/data/$BASENAME.osm.pbf"

echo "== osrm-partition =="
run osrm-partition "/data/$BASENAME.osrm"

echo "== osrm-customize =="
run osrm-customize "/data/$BASENAME.osrm"

echo "== Done =="
ls -lh "$BUILD_DIR/$BASENAME".osrm* | sed "s|$BUILD_DIR/||"
echo
echo "Serve locally with:"
echo "  docker run --rm -t -p 5001:5000 -v \"$BUILD_DIR:/data\" $OSRM_IMAGE \\"
echo "    osrm-routed --algorithm mld /data/$BASENAME.osrm"

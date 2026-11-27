#!/usr/bin/env bash
# Build Apex's Valhalla routing data on a workstation. Nothing is built on the
# VPS: it is a 2-core box that gets throttled under sustained load. deploy.sh
# ships the result.
#
#   DOCKER="sudo docker" BUILD_DIR=~/work/valhalla-build ./build.sh
#
# Produces in $BUILD_DIR/out/:
#   valhalla_tiles.tar     the routing graph, one file, memory-mapped by the service
#   admins.sqlite          country/admin areas (driving side, country access rules)
#   timezones.sqlite       so a departure time is read as Cairo local time
#   valhalla.json          service config, paths relative to /data
#   apex-time-bans.json    every way a time ban caught (etit-proxy flags their use)
#   apex-road-names.json   name:en by way id for the major roads (etit-proxy's road
#                          lists; Valhalla itself serves only the Arabic name)
#   BUILD_INFO             Valhalla version + image digest, extract date, rule checksum
#
# Steps:
#   1. Download the Egypt extract (FRESH=1 forces a new one).
#   2. apply_time_bans.py writes time-bans.json into it as hgv:conditional.
#   3. export_road_names.py writes the English road names from the same extract.
#   4. Valhalla builds timezones, admins and tiles from the result, then packs
#      the tiles into one tar.
#
# The names alone, into an existing out/ (the tiles unchanged, same extract):
#   $PYTHON export_road_names.py $BUILD_DIR/egypt-latest.osm.pbf $BUILD_DIR/out/apex-road-names.json
#
# Env:
#   VALHALLA_IMAGE  default ghcr.io/valhalla/valhalla:latest (BUILD_INFO pins the digest;
#                   pass an @sha256: reference to rebuild on the image already live)
#   PYTHON          a python with pyosmium (pip install osmium); default python3
#   PBF_URL         default geofabrik egypt-latest
#   DOCKER          default docker

set -euo pipefail

VALHALLA_IMAGE="${VALHALLA_IMAGE:-ghcr.io/valhalla/valhalla:latest}"
DOCKER="${DOCKER:-docker}"
PYTHON="${PYTHON:-python3}"
PBF_URL="${PBF_URL:-https://download.geofabrik.de/africa/egypt-latest.osm.pbf}"
BUILD_DIR="${BUILD_DIR:-$(pwd)/valhalla-build}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$BUILD_DIR/out"

"$PYTHON" -c "import osmium" 2>/dev/null \
  || { echo "need pyosmium: $PYTHON -m pip install osmium (or set PYTHON)" >&2; exit 1; }
mkdir -p "$BUILD_DIR"

# 1. Extract. -R keeps the server's timestamp, which BUILD_INFO reports.
[ "${FRESH:-0}" = "1" ] && rm -f "$BUILD_DIR/egypt-latest.osm.pbf"
if [ ! -f "$BUILD_DIR/egypt-latest.osm.pbf" ]; then
  echo "== Downloading the Egypt extract =="
  curl -fLR --retry 5 --retry-all-errors -C - -o "$BUILD_DIR/egypt-latest.osm.pbf.part" "$PBF_URL"
  mv "$BUILD_DIR/egypt-latest.osm.pbf.part" "$BUILD_DIR/egypt-latest.osm.pbf"
fi

# 2. Time bans.
echo "== Applying time bans =="
rm -rf "$OUT" && mkdir -p "$OUT"
"$PYTHON" "$SCRIPT_DIR/apply_time_bans.py" "$SCRIPT_DIR/time-bans.json" \
  "$BUILD_DIR/egypt-latest.osm.pbf" "$BUILD_DIR/egypt-apex.osm.pbf" "$OUT/apex-time-bans.json"

echo "== English road names =="
"$PYTHON" "$SCRIPT_DIR/export_road_names.py" "$BUILD_DIR/egypt-latest.osm.pbf" "$OUT/apex-road-names.json"

# 4. Valhalla. Everything runs in the container with $BUILD_DIR at /build.
v() { $DOCKER run --rm -t -v "$BUILD_DIR:/build" -w /build "$VALHALLA_IMAGE" "$@"; }
if ! $DOCKER pull -q "$VALHALLA_IMAGE" >/dev/null; then
  # Offline or the registry is slow: a copy already here is fine, since
  # BUILD_INFO records its digest and deploy.sh runs exactly that one.
  $DOCKER image inspect "$VALHALLA_IMAGE" >/dev/null 2>&1 \
    || { echo "cannot pull $VALHALLA_IMAGE and it is not here" >&2; exit 1; }
  echo "   pull failed; using the local copy of $VALHALLA_IMAGE"
fi

echo "== Config =="
# Paths are written for the service's view (/data). The build uses its own
# copy with /build paths.
v valhalla_build_config \
  --mjolnir-tile-dir /data/valhalla_tiles \
  --mjolnir-tile-extract /data/valhalla_tiles.tar \
  --mjolnir-traffic-extract "" \
  --mjolnir-admin /data/admins.sqlite \
  --mjolnir-timezone /data/timezones.sqlite \
  --mjolnir-include-construction false \
  --additional-data-elevation "" \
  --httpd-service-listen "tcp://*:8002" \
  --service-limits-trace-max-shape 20000 \
  --service-limits-trace-max-distance 2000000 \
  --service-limits-trace-max-search-radius 100 \
  --service-limits-truck-max-distance 5000000 \
  --service-limits-truck-max-locations 50 \
  --service-limits-truck-max-matrix-location-pairs 2500 \
  --meili-default-search-radius 40 \
  --meili-default-gps-accuracy 15 \
  --meili-default-breakage-distance 5000 \
  --meili-default-interpolation-distance 10 \
  --meili-customizable "mode,search_radius,turn_penalty_factor,gps_accuracy,interpolation_distance,sigma_z,beta,max_route_distance_factor,max_route_time_factor,breakage_distance" \
  > "$OUT/valhalla.json"
sed 's#"/data/#"/build/out/#g' "$OUT/valhalla.json" > "$BUILD_DIR/build.json"

echo "== Timezones =="
v sh -c 'valhalla_build_timezones > /build/out/timezones.sqlite'

echo "== Admins =="
v valhalla_build_admins -c /build/build.json /build/egypt-apex.osm.pbf

echo "== Tiles =="
v valhalla_build_tiles -c /build/build.json /build/egypt-apex.osm.pbf

echo "== Tar =="
v valhalla_build_extract -c /build/build.json -v
$DOCKER run --rm -v "$BUILD_DIR:/build" "$VALHALLA_IMAGE" rm -rf /build/out/valhalla_tiles /build/build.json

# 5. What built this. deploy.sh runs exactly this image digest on the VPS.
VERSION="$($DOCKER run --rm "$VALHALLA_IMAGE" valhalla_service --version 2>/dev/null | tr -d '\r' | head -1)"
DIGEST="$($DOCKER image inspect --format '{{index .RepoDigests 0}}' "$VALHALLA_IMAGE")"
{
  echo "valhalla_version=$VERSION"
  echo "image=$DIGEST"
  echo "extract=$PBF_URL"
  echo "extract_date=$(date -u -r "$BUILD_DIR/egypt-latest.osm.pbf" +%Y-%m-%dT%H:%M:%SZ)"
  echo "time_bans_sha256=$(sha256sum "$SCRIPT_DIR/time-bans.json" | cut -d' ' -f1)"
  echo "commit=$(git -C "$SCRIPT_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)$(git -C "$SCRIPT_DIR" diff --quiet -- . 2>/dev/null || echo '+uncommitted')"
  echo "built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$OUT/BUILD_INFO"
$DOCKER run --rm -v "$BUILD_DIR:/build" "$VALHALLA_IMAGE" chown -R "$(id -u):$(id -g)" /build/out

echo "== Done =="
cat "$OUT/BUILD_INFO"
ls -lh "$OUT"
echo
echo "Serve locally:"
echo "  $DOCKER run -d --rm --name apex-valhalla-local -p 127.0.0.1:18002:8002 -v $OUT:/data $DIGEST valhalla_service /data/valhalla.json 2"

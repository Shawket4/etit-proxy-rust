#!/usr/bin/env bash
# Ship a Valhalla build (from build.sh) to the VPS. Nothing is built there.
#
#   VPS=root@apextransport.ddns.net SSH_KEY=~/.ssh/apex_vps_osrm \
#     BUILD_DIR=~/work/valhalla-build ./deploy.sh
#   VPS=... SSH_KEY=... ./deploy.sh --rollback
#
# VPS layout (created on first deploy):
#   /opt/valhalla/releases/<stamp>/   one build: tiles tar, sqlite dbs, config, bans, names
#   /opt/valhalla/current -> releases/<stamp>
#   container apex-valhalla           127.0.0.1:8002, serves /opt/valhalla/current
#
# A deploy, in order. Nothing live changes before step 5; any failure stops:
#   1. Local: the build is complete; BUILD_INFO names the image digest.
#   2. VPS: tools present, disk free, the port is ours or free, pull the exact
#      image digest that built the tiles (a pull, not a build).
#   3. Upload into releases/<stamp>/. Files unchanged since the live release
#      (the 120 MB timezone db, usually) are hard-linked, not resent.
#      sha256 verified.
#   4. Start a throwaway container on the new release (127.0.0.1:8012) and
#      run test.sh against it.
#   5. Point `current` at the new release (one rename), restart (or, first
#      time, create) apex-valhalla, run test.sh against :8002. On failure,
#      point `current` back and restart.
#   6. Keep the three newest releases.
#
# etit-proxy reads VALHALLA_URL=http://127.0.0.1:8002,
# VALHALLA_TIME_BANS=/opt/valhalla/current/apex-time-bans.json and
# VALHALLA_ROAD_NAMES=/opt/valhalla/current/apex-road-names.json; it re-reads
# the bans at every scan and the names when the file changes, so no
# etit-proxy restart is needed.
#
# A restart drops routing for the second or two the service takes to open
# the tiles. Avoid the trip-audit scan time (TRIP_SCAN_TIME, 04:45 default).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DIR="${BUILD_DIR:-$SCRIPT_DIR/valhalla-build}"
OUT="$BUILD_DIR/out"
ROOT="${REMOTE_ROOT:-/opt/valhalla}"
PORT="${VALHALLA_PORT:-8002}"
CANDIDATE_PORT=8012
NAME=apex-valhalla
THREADS="${VALHALLA_THREADS:-2}"
SSH_PORT="${SSH_PORT:-22}"
MODE="${1:-deploy}"

die() { echo "deploy: $*" >&2; exit 1; }
[ -n "${VPS:-}" ] || die "set VPS=user@host"
SSH_OPTS=(-p "$SSH_PORT" -o BatchMode=yes -o ConnectTimeout=15)
[ -n "${SSH_KEY:-}" ] && SSH_OPTS+=(-i "$SSH_KEY" -o IdentitiesOnly=yes)
remote() { ssh "${SSH_OPTS[@]}" "$VPS" "$@"; }

# Helpers shared by the remote scripts.
LIB=$(cat <<'LIB'
set -euo pipefail
wait_ready() { # <port> <seconds>
  local i
  for i in $(seq 1 "$2"); do
    curl -s -m 3 "http://127.0.0.1:$1/status" | grep -q '"version"' && return 0
    sleep 1
  done
  return 1
}
run_service() { # <name> <host port> <release dir> <image> <threads> [extra docker args]
  local name=$1 port=$2 dir=$3 image=$4 threads=$5; shift 5
  docker run -d --name "$name" "$@" -p "127.0.0.1:$port:8002" -v "$dir:/data:ro" "$image" \
    valhalla_service /data/valhalla.json "$threads" >/dev/null
}
LIB
)

# ---------------------------------------------------------------- rollback --
if [ "$MODE" = "--rollback" ]; then
  remote "ROOT='$ROOT' PORT='$PORT' NAME='$NAME' bash -s" <<EOF
$LIB
cd "\$ROOT"
cur=\$(readlink current)
prev=\$(ls -d releases/* | sort | grep -B1 -x "\$cur" | head -1)
[ -n "\$prev" ] && [ "\$prev" != "\$cur" ] || { echo "no release before \$cur" >&2; exit 1; }
ln -sfn "\$prev" current.new && mv -T current.new current
docker restart "\$NAME" >/dev/null
wait_ready "\$PORT" 120 || { echo "apex-valhalla did not come back" >&2; exit 1; }
echo "current -> \$prev (was \$cur)"
VALHALLA_URL=http://127.0.0.1:\$PORT bash current/test.sh current/apex-time-bans.json current/apex-road-names.json
EOF
  exit 0
fi
[ "$MODE" = "deploy" ] || die "unknown argument: $MODE (use --rollback or nothing)"

# ---------------------------------------------------------------- 1. local --
echo "== 1. Local build in $OUT =="
for f in valhalla_tiles.tar admins.sqlite timezones.sqlite valhalla.json apex-time-bans.json apex-road-names.json BUILD_INFO; do
  [ -s "$OUT/$f" ] || die "missing $OUT/$f: run build.sh first"
done
IMAGE="$(sed -n 's/^image=//p' "$OUT/BUILD_INFO")"
[[ "$IMAGE" == *@sha256:* ]] || die "BUILD_INFO has no pinned image digest"
cp "$SCRIPT_DIR/test.sh" "$OUT/test.sh"
(cd "$OUT" && sha256sum valhalla_tiles.tar admins.sqlite timezones.sqlite valhalla.json apex-time-bans.json apex-road-names.json BUILD_INFO test.sh > SHA256SUMS)
BYTES="$(du -cb "$OUT"/* | tail -1 | cut -f1)"
echo "   $((BYTES / 1024 / 1024)) MiB, image $IMAGE"

# --------------------------------------------------------------- 2. remote --
echo "== 2. Checking $VPS =="
remote "ROOT='$ROOT' PORT='$PORT' NAME='$NAME' NEED='$((BYTES * 2))' IMAGE='$IMAGE' bash -s" <<'EOF'
set -euo pipefail
for t in docker curl python3 rsync sha256sum; do
  command -v "$t" >/dev/null || { echo "missing on the VPS: $t" >&2; exit 1; }
done
mkdir -p "$ROOT/releases"
free=$(df -B1 --output=avail "$ROOT" | tail -1)
[ "$free" -gt "$NEED" ] || { echo "only $((free/1024/1024)) MiB free under $ROOT, need $((NEED/1024/1024))" >&2; exit 1; }
owner=$(docker ps --filter "publish=$PORT" --format '{{.Names}}' | head -1)
if [ -n "$owner" ] && [ "$owner" != "$NAME" ]; then
  echo "port $PORT is taken by container $owner" >&2; exit 1
fi
if [ -z "$owner" ] && (ss -ltnH "sport = :$PORT" 2>/dev/null | grep -q .); then
  echo "port $PORT is taken by a non-docker process" >&2; exit 1
fi
docker pull -q "$IMAGE" >/dev/null
echo "   ok: $((free/1024/1024)) MiB free, port $PORT ${owner:+held by $owner}${owner:-free}, image pulled"
EOF

# --------------------------------------------------------------- 3. upload --
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
REL="$ROOT/releases/$STAMP"
echo "== 3. Uploading to $REL =="
LINK_DEST=()
if remote "test -e '$ROOT/current'"; then
  LINK_DEST=(--link-dest="$(remote "readlink -f '$ROOT/current'")")
fi
rsync -a --partial --compress --info=progress2 "${LINK_DEST[@]}" -e "ssh ${SSH_OPTS[*]}" "$OUT/" "$VPS:$REL/"
remote "cd '$REL' && sha256sum --quiet -c SHA256SUMS" || die "checksum mismatch after upload; nothing changed"
echo "   checksums verified"

# ------------------------------------------------------- 4. test beside live --
echo "== 4. Testing the new release on :$CANDIDATE_PORT =="
remote "REL='$REL' IMAGE='$IMAGE' CPORT='$CANDIDATE_PORT' bash -s" <<EOF
$LIB
docker rm -f apex-valhalla-candidate >/dev/null 2>&1 || true
run_service apex-valhalla-candidate "\$CPORT" "\$REL" "\$IMAGE" 1 --rm
ok=0
if wait_ready "\$CPORT" 120 && VALHALLA_URL=http://127.0.0.1:\$CPORT bash "\$REL/test.sh" "\$REL/apex-time-bans.json" "\$REL/apex-road-names.json"; then ok=1; fi
[ "\$ok" = 1 ] || docker logs --tail 30 apex-valhalla-candidate >&2 || true
docker rm -f apex-valhalla-candidate >/dev/null 2>&1 || true
[ "\$ok" = 1 ] || { echo "the new release failed its tests; nothing live changed (left in \$REL)" >&2; exit 1; }
EOF

# ----------------------------------------------------------------- 5. swap --
echo "== 5. Going live =="
remote "ROOT='$ROOT' REL='$REL' PORT='$PORT' NAME='$NAME' IMAGE='$IMAGE' THREADS='$THREADS' bash -s" <<EOF
$LIB
cd "\$ROOT"
prev=\$(readlink current 2>/dev/null || true)
ln -sfn "releases/\$(basename "\$REL")" current.new && mv -T current.new current
if docker inspect "\$NAME" >/dev/null 2>&1 \
   && [ "\$(docker inspect --format '{{.Config.Image}}' "\$NAME")" = "\$IMAGE" ]; then
  docker restart "\$NAME" >/dev/null
else
  # First install, or a new Valhalla version: (re)create on the build's image.
  docker rm -f "\$NAME" >/dev/null 2>&1 || true
  run_service "\$NAME" "\$PORT" "\$ROOT/current" "\$IMAGE" "\$THREADS" --restart unless-stopped
fi
if wait_ready "\$PORT" 120 && VALHALLA_URL=http://127.0.0.1:\$PORT bash current/test.sh current/apex-time-bans.json current/apex-road-names.json; then
  ls -d releases/* | sort | head -n -3 | xargs -r rm -rf
  echo "   live: current -> \$(readlink current)"
  cat current/BUILD_INFO
else
  echo "the live service failed after the swap" >&2
  if [ -n "\$prev" ]; then
    ln -sfn "\$prev" current.new && mv -T current.new current
    docker restart "\$NAME" >/dev/null && wait_ready "\$PORT" 120 && echo "restored \$prev" >&2
  fi
  exit 1
fi
EOF
echo "== Done =="

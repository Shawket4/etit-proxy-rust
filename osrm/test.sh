#!/usr/bin/env bash
# Test suite for the Egypt truck OSRM dataset (truck.lua profile).
#
# Usage:
#   OSRM_URL=http://localhost:5001 ./test.sh
#
# Exits non-zero if any check fails. Requires curl + python3 (JSON parsing).

set -uo pipefail

OSRM_URL="${OSRM_URL:-http://localhost:5001}"
EXCLUDE_CLASS="${EXCLUDE_CLASS:-daytimebanned}"

FAIL=0
pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1" >&2; FAIL=1; }

# route <name> <coords> [extra query params] -> prints "code distance duration"
route() {
  local coords="$1" extra="${2:-}"
  curl -s -m 30 "$OSRM_URL/route/v1/driving/$coords?overview=false${extra:+&$extra}" \
    | python3 -c '
import json,sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("PARSE_ERROR 0 0"); sys.exit(0)
code = d.get("code","?")
if code == "Ok" and d.get("routes"):
    r = d["routes"][0]
    print(code, r["distance"], r["duration"])
else:
    print(code, 0, 0)
'
}

http_code() {
  local coords="$1" extra="${2:-}"
  curl -s -m 30 -o /dev/null -w "%{http_code}" \
    "$OSRM_URL/route/v1/driving/$coords?overview=false${extra:+&$extra}"
}

# Coordinates are lon,lat (OSRM order)
SUEZ="32.5263,29.9737"        # Suez fuel terminal
BADR="31.72,30.135"           # Badr terminal area (east of Cairo)
GIZA="31.13,29.98"            # Giza / Mariouteya side (west of Cairo)
CAIRO_A="31.2357,30.0444"     # central Cairo (Tahrir area)
CAIRO_B="31.3260,30.0561"     # Heliopolis area
# Pairs whose natural fastest path rides the Cairo Ring Road (verified against
# the 2026-08 extract). Badr->Giza was tried first but its best route uses the
# Middle Ring Road / eastern axes, not the (banned) Ring Road, so excluding
# the class changes nothing there.
SHUBRA="31.25,30.128"         # Shubra El-Kheima, near northern Ring Road
KATAMEYA="31.32,29.96"        # Katameya, near southeastern Ring Road
RING_E1="31.435,30.079"       # on the eastern Ring Road arc (Nasr City)
RING_E2="31.417,30.135"       # on the northeastern Ring Road arc

echo "== OSRM truck profile tests against $OSRM_URL =="

# --- 1. Sanity: Suez -> Badr ----------------------------------------------
# Geodesic is ~79 km; via the Cairo-Suez desert road ~90 km is expected.
read -r code dist dur <<<"$(route "$SUEZ;$BADR")"
if [ "$code" = "Ok" ]; then
  km=$(python3 -c "print(round($dist/1000,1))")
  if python3 -c "exit(0 if 80 <= $dist/1000 <= 180 else 1)"; then
    pass "Suez->Badr routable, ${km} km (expected ~90-140)"
  else
    fail "Suez->Badr distance ${km} km outside 80-180 km sanity window"
  fi
else
  fail "Suez->Badr returned code=$code"
fi

# --- 2. Exclude class changes the route ----------------------------------
read -r ncode ndist ndur <<<"$(route "$SHUBRA;$KATAMEYA")"
read -r xcode xdist xdur <<<"$(route "$SHUBRA;$KATAMEYA" "exclude=$EXCLUDE_CLASS")"
if [ "$ncode" = "Ok" ] && [ "$xcode" = "Ok" ]; then
  nkm=$(python3 -c "print(round($ndist/1000,1))")
  xkm=$(python3 -c "print(round($xdist/1000,1))")
  # Normal path rides the eastern Ring Road arc; the exclude must force a
  # meaningfully different, longer route.
  if python3 -c "exit(0 if $xdist > $ndist + 500 else 1)"; then
    pass "exclude=$EXCLUDE_CLASS diverts Shubra->Katameya: ${nkm} km -> ${xkm} km"
  else
    fail "exclude=$EXCLUDE_CLASS did not change Shubra->Katameya (${nkm} km vs ${xkm} km) - class not applied to Ring Road?"
  fi
else
  fail "Shubra->Katameya codes: normal=$ncode excluded=$xcode"
fi

# --- 2b. On-ring segment must also divert ---------------------------------
read -r ncode ndist ndur <<<"$(route "$RING_E1;$RING_E2")"
read -r xcode xdist xdur <<<"$(route "$RING_E1;$RING_E2" "exclude=$EXCLUDE_CLASS")"
if [ "$ncode" = "Ok" ] && [ "$xcode" = "Ok" ] \
   && python3 -c "exit(0 if $xdist > $ndist + 500 else 1)"; then
  pass "exclude=$EXCLUDE_CLASS diverts on-ring arc: $(python3 -c "print(round($ndist/1000,1))") km -> $(python3 -c "print(round($xdist/1000,1))") km"
else
  fail "on-ring arc: normal=$ncode/$ndist excluded=$xcode/$xdist (expected longer detour)"
fi

# --- 3. HTTP status codes -------------------------------------------------
hc=$(http_code "$SHUBRA;$KATAMEYA" "exclude=$EXCLUDE_CLASS")
if [ "$hc" = "200" ]; then
  pass "exclude=$EXCLUDE_CLASS -> HTTP 200"
else
  fail "exclude=$EXCLUDE_CLASS -> HTTP $hc (expected 200; class missing from profile?)"
fi

hc=$(http_code "$BADR;$GIZA" "exclude=nosuchclass")
if [ "$hc" = "400" ]; then
  pass "exclude=nosuchclass -> HTTP 400 (negative control)"
else
  fail "exclude=nosuchclass -> HTTP $hc (expected 400)"
fi

# --- 4. Central Cairo truck sanity ---------------------------------------
read -r code dist dur <<<"$(route "$CAIRO_A;$CAIRO_B")"
if [ "$code" = "Ok" ] && python3 -c "exit(0 if $dist > 3000 else 1)"; then
  km=$(python3 -c "print(round($dist/1000,1))")
  pass "central Cairo route Ok (${km} km)"
else
  fail "central Cairo route: code=$code dist=$dist"
fi

echo
if [ "$FAIL" -eq 0 ]; then
  echo "ALL TESTS PASSED"
else
  echo "SOME TESTS FAILED" >&2
fi
exit "$FAIL"

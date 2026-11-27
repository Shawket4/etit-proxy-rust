#!/usr/bin/env bash
# Smoke test for an Apex Valhalla instance (tiles from build.sh).
#
#   VALHALLA_URL=http://127.0.0.1:8002 ./test.sh [apex-time-bans.json]
#
# Exits non-zero if any check fails. Needs curl + python3. deploy.sh runs it
# against the new tiles before they go live, and again after the swap.

set -uo pipefail
URL="${VALHALLA_URL:-http://127.0.0.1:8002}"
BANS="${1:-}"
FAIL=0
pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1" >&2; FAIL=1; }

# q <endpoint> <json> <python expr over d> → prints the expression's value
q() {
  curl -s -m 60 -H 'Content-Type: application/json' -d "$2" "$URL/$1" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print('PARSE_ERROR'); sys.exit(0)
try:
    print($3)
except Exception as e:
    print('ERR', d.get('error', e))
"
}

TRUCK='{"truck":{"height":4.0,"width":2.6,"length":16.5,"weight":40,"axle_load":10,"axle_count":5,"hazmat":true,"top_speed":90}}'
# route <from lat,lon> <to lat,lon> <local time or ""> → "km names..." (names joined by |)
route() {
  local dt=""
  [ -n "$3" ] && dt=",\"date_time\":{\"type\":1,\"value\":\"$3\"}"
  q route "{\"locations\":[{\"lat\":${1%,*},\"lon\":${1#*,}},{\"lat\":${2%,*},\"lon\":${2#*,}}],\"costing\":\"truck\",\"costing_options\":$TRUCK,\"units\":\"kilometers\",\"directions_type\":\"maneuvers\"$dt}" \
    "'%.1f ' % d['trip']['summary']['length'] + '|'.join(n for l in d['trip']['legs'] for m in l['maneuvers'] for n in m.get('street_names', []))"
}
# closed_driven <from> <to> <local time> -> "<km> <closed ways driven>": the
# truck route, map-matched back onto the graph, against every way the bans
# file lists. The one check that does not trust street names.
closed_driven() {
  python3 - "$URL" "$BANS" "$1" "$2" "$3" "$TRUCK" <<'PY'
import json, sys, urllib.request
url, bans, a, b, when, truck = sys.argv[1:]
def post(ep, body):
    req = urllib.request.Request(f"{url}/{ep}", json.dumps(body).encode(), {"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=60))
loc = lambda s: dict(zip(("lat", "lon"), map(float, s.split(","))))
try:
    r = post("route", {"locations": [loc(a), loc(b)], "costing": "truck", "costing_options": json.loads(truck),
                       "units": "kilometers", "date_time": {"type": 1, "value": when}})
    t = post("trace_attributes", {"encoded_polyline": r["trip"]["legs"][0]["shape"], "shape_match": "map_snap",
                                  "costing": "auto", "costing_options": {"auto": {"ignore_access": True, "ignore_restrictions": True}},
                                  "filters": {"attributes": ["edge.way_id"], "action": "include"}})
    closed = {w for x in json.load(open(bans))["bans"] for w in x["way_ids"]}
    print(f"{r['trip']['summary']['length']:.1f} {len({e['way_id'] for e in t['edges']} & closed)}")
except Exception as e:
    print(f"ERR {e}")
PY
}
DAY="$(date -d tomorrow +%Y-%m-%d)T13:00"
NIGHT="$(date -d tomorrow +%Y-%m-%d)T23:00"
SHEIKH_ZAYED="30.045,30.98"; MOHANDESSIN="30.056,31.20"
SUEZ="29.9737,32.5263"; BADR="30.135,31.72"; MOSTOROD="30.146,31.286"; OCTOBER="29.97,30.94"
RAS_SUDR="29.5907,32.7080"

echo "== Valhalla tests against $URL =="

v=$(curl -s -m 10 "$URL/status" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("version",""))' 2>/dev/null)
[ -n "$v" ] && pass "status: Valhalla $v" || fail "no /status answer"

read -r km _ <<<"$(route "$SUEZ" "$BADR" "$DAY")"
python3 -c "exit(0 if 80 <= float('$km') <= 180 else 1)" 2>/dev/null \
  && pass "Suez->Badr truck route ${km} km" || fail "Suez->Badr: $km"

# 26th of July: closed at every hour (owner, 2026-11-27).
read -r dkm dnames <<<"$(route "$SHEIKH_ZAYED" "$MOHANDESSIN" "$DAY")"
read -r nkm nnames <<<"$(route "$SHEIKH_ZAYED" "$MOHANDESSIN" "$NIGHT")"
for when in day night; do
  if [ "$when" = day ]; then km=$dkm names=$dnames; else km=$nkm names=$nnames; fi
  [[ "$names" != *"محور 26 يوليو"* ]] && pass "$when: Sheikh Zayed->Mohandessin avoids 26th of July (${km} km)" \
    || fail "$when: Sheikh Zayed->Mohandessin is on 26th of July (${km} km): is the closure in the tiles?"
done
python3 -c "exit(0 if abs(float('$dkm') - float('$nkm')) < 1 else 1)" 2>/dev/null \
  && pass "day and night routes agree (${dkm} / ${nkm} km): no night shortcut" \
  || fail "day ${dkm} km vs night ${nkm} km: something is still time-dependent"

# The Ahmed Hamdi tunnels: closed at every hour. Through them it is 84 km.
read -r km names <<<"$(route "$SUEZ" "$RAS_SUDR" "$NIGHT")"
if [[ "|$names|" != *"|نفق الشهيد احمد حمدى|"* && "|$names|" != *"|نفق الشهيد احمد حمدى الشمالى|"* ]] \
   && python3 -c "exit(0 if float('$km') > 100 else 1)" 2>/dev/null; then
  pass "night: Suez->Ras Sudr avoids the Ahmed Hamdi tunnels (${km} km)"
else fail "night: Suez->Ras Sudr ${km} km: is it still going through Ahmed Hamdi?"; fi

read -r km names <<<"$(route "$MOSTOROD" "$OCTOBER" "$DAY")"
[[ "$names" == *"الدائر"* ]] && pass "day: Mostorod->6th of October rides the Ring Road (${km} km)" \
  || fail "day: Mostorod->6th of October avoids the Ring Road (${km} km): is it banned?"

m=$(q sources_to_targets "{\"sources\":[{\"lat\":30.146,\"lon\":31.286},{\"lat\":30.7865,\"lon\":31.0004}],\"targets\":[{\"lat\":30.146,\"lon\":31.286},{\"lat\":30.7865,\"lon\":31.0004}],\"costing\":\"truck\",\"costing_options\":$TRUCK,\"units\":\"kilometers\"}" \
  "'%.1f %.1f' % (d['sources_to_targets'][0][1]['distance'], d['sources_to_targets'][1][0]['distance'])")
python3 -c "a,b=map(float,'$m'.split()); exit(0 if 60<a<140 and 60<b<140 else 1)" 2>/dev/null \
  && pass "matrix Mostorod<->Tanta: $m km" || fail "matrix: $m"

t=$(q trace_attributes '{"shape":[{"lat":30.0605,"lon":31.2185,"time":0},{"lat":30.0590,"lon":31.2110,"time":60},{"lat":30.0575,"lon":31.2040,"time":120}],"costing":"auto","shape_match":"map_snap","use_timestamps":true,"filters":{"attributes":["edge.way_id"],"action":"include"}}' \
  "len(d['edges'])")
[[ "$t" =~ ^[0-9]+$ ]] && [ "$t" -gt 0 ] && pass "trace_attributes matched $t edges" || fail "trace_attributes: $t"

if [ -n "$BANS" ]; then
  n=$(python3 -c "import json; print(sum(len(b['way_ids']) for b in json.load(open('$BANS'))['bans']))" 2>/dev/null)
  [ "${n:-0}" -gt 0 ] && pass "time-bans file lists $n ways" || fail "time-bans file $BANS lists no ways"
  for leg in "SHEIKH_ZAYED MOHANDESSIN NIGHT" "SUEZ RAS_SUDR DAY" "MOSTOROD OCTOBER NIGHT"; do
    read -r a b t <<<"$leg"
    read -r km hits <<<"$(closed_driven "${!a}" "${!b}" "${!t}")"
    [ "$hits" = "0" ] && pass "$a->$b ($t): no closed way on the route (${km} km)" \
      || fail "$a->$b ($t): route drives ${hits} closed ways (${km} km)"
  done
fi

echo
[ "$FAIL" -eq 0 ] && echo "ALL TESTS PASSED" || echo "SOME TESTS FAILED" >&2
exit "$FAIL"

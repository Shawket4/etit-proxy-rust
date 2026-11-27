#!/usr/bin/env python3
"""Write the truck bans in time-bans.json into an OSM extract.

    apply_time_bans.py <bans.json> <in.osm.pbf> <out.osm.pbf> <ways-out.json>

Every way that matches a ban (a name substring, a road class, any required
tags, and at least one node inside the ban's bounding box) is closed to trucks:

  "always": true             hgv=no: closed at every hour.
  banned_from / banned_to    hgv:conditional=no @ (FROM-TO): Valhalla avoids it
                             for trucks departing inside those hours and allows
                             it outside them; no request-time flag is needed.

The road class, the tags and the box are what keep a rule to the real roads.
A name match alone also catches side streets named after the corridor ("شارع
متفرع من شارع 26 يوليو"), same-named streets in other towns, and the approach
roads that share a tunnel's name.

<ways-out.json> lists every banned way (id, name, class, length) per ban.
etit-proxy reads it to flag a truck that drove one while it was closed, and it
is the thing to read when checking what a rule caught.
"""
import json
import math
import sys
from collections import defaultdict

import osmium


def load_bans(path):
    with open(path, encoding="utf-8") as f:
        bans = json.load(f)["bans"]
    for b in bans:
        b["_patterns"] = [p.lower() for p in b["name_contains"]]
        b["_highway"] = set(b["highway"])
        b["_tags"] = {k: set(v) for k, v in b.get("tags", {}).items()}
        always = b.get("always") is True
        timed = "banned_from" in b and "banned_to" in b
        if always == timed:
            sys.exit(f"ban {b['id']}: give either \"always\": true or banned_from + banned_to")
        b["_always"] = always
        b["_conditional"] = None if always else f"no @ ({b['banned_from']}-{b['banned_to']})"
        b["_hours"] = "all hours" if always else f"{b['banned_from']}-{b['banned_to']}"
    return bans


def way_length_m(way):
    total, prev = 0.0, None
    for n in way.nodes:
        if not n.location.valid():
            prev = None
            continue
        cur = (n.location.lat, n.location.lon)
        if prev:
            la1, lo1, la2, lo2 = map(math.radians, (*prev, *cur))
            h = math.sin((la2 - la1) / 2) ** 2 + math.cos(la1) * math.cos(la2) * math.sin((lo2 - lo1) / 2) ** 2
            total += 2 * 6371000 * math.asin(math.sqrt(h))
        prev = cur
    return total


def matches(ban, way):
    if way.tags.get("highway") not in ban["_highway"]:
        return False
    if any(way.tags.get(k) not in allowed for k, allowed in ban["_tags"].items()):
        return False
    names = [way.tags.get(k, "") for k in ban["name_keys"]]
    if not any(p in n.lower() for n in names if n for p in ban["_patterns"]):
        return False
    w, s, e, n = ban["within_bbox"]
    return any(
        nd.location.valid() and w <= nd.location.lon <= e and s <= nd.location.lat <= n
        for nd in way.nodes
    )


def main(bans_path, src, dst, ways_out):
    bans = load_bans(bans_path)
    caught = defaultdict(list)

    with osmium.SimpleWriter(dst, overwrite=True) as writer:
        for obj in osmium.FileProcessor(src).with_locations():
            if obj.is_way():
                hits = [b for b in bans if matches(b, obj)]
                if hits:
                    tags = dict(obj.tags)
                    if any(b["_always"] for b in hits):
                        # Closed at every hour: a time window on top means nothing.
                        tags["hgv"] = "no"
                        tags.pop("hgv:conditional", None)
                    else:
                        rules = [b["_conditional"] for b in hits]
                        if tags.get("hgv:conditional"):
                            rules.insert(0, tags["hgv:conditional"])
                        tags["hgv:conditional"] = "; ".join(rules)
                    for b in hits:
                        caught[b["id"]].append({
                            "way_id": obj.id,
                            "name": obj.tags.get("name", ""),
                            "highway": obj.tags.get("highway"),
                            "length_m": round(way_length_m(obj)),
                        })
                    writer.add_way(obj.replace(tags=tags))
                    continue
            writer.add(obj)

    out = {"bans": []}
    for b in bans:
        ways = sorted(caught[b["id"]], key=lambda w: w["way_id"])
        entry = {"id": b["id"], "label": b["label"], "label_ar": b.get("label_ar", "")}
        if b["_always"]:
            entry["always"] = True
        else:
            entry["banned_from"] = b["banned_from"]
            entry["banned_to"] = b["banned_to"]
        entry["way_ids"] = [w["way_id"] for w in ways]
        entry["ways"] = ways
        out["bans"].append(entry)
        if not ways:
            sys.exit(f"ban {b['id']} caught no ways: check its names, classes, tags and box")
        by_name = defaultdict(lambda: [0, 0.0, set()])
        for w in ways:
            agg = by_name[w["name"]]
            agg[0] += 1
            agg[1] += w["length_m"] / 1000
            agg[2].add(w["highway"])
        print(f"{b['id']}: {len(ways)} ways, {sum(w['length_m'] for w in ways) / 1000:.1f} km closed {b['_hours']}")
        for name, (count, km, classes) in sorted(by_name.items(), key=lambda kv: -kv[1][1]):
            print(f"  {km:7.2f} km  {count:4d} ways  {','.join(sorted(classes)):<40} {name}")

    with open(ways_out, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=1)


if __name__ == "__main__":
    if len(sys.argv) != 5:
        sys.exit(__doc__)
    main(*sys.argv[1:])

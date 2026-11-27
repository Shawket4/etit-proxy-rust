#!/usr/bin/env python3
"""Export the English names of Egypt's roads, by OSM way id.

    export_road_names.py <in.osm.pbf> <names-out.json>

Valhalla serves each road under its default-language name, which in Egypt is
the Arabic `name`; it has no request option for `name:en`. etit-proxy reads
this file to put the English name next to the Arabic one in the fee lookup's
road lists (VALHALLA_ROAD_NAMES).

Only the roads the list names are written: highway=motorway, trunk, primary,
secondary or tertiary (and their links) that carry `name:en`. Streets are
"local streets" in the list, nameless; including them would make the file
ten times bigger.

    {"tag": "name:en", "ways": 25936, "names": {"276379253": "Al Ein Al Sokhna Road", ...}}

The way ids are those of the extract the tiles were built from (build.sh runs
this on the same egypt-latest.osm.pbf), so they match the edges Valhalla
returns.
"""
import json
import sys

import osmium


ROADS = {"motorway", "trunk", "primary", "secondary", "tertiary"}
ROADS |= {f"{r}_link" for r in ROADS}


class Names(osmium.SimpleHandler):
    def __init__(self):
        super().__init__()
        self.names = {}

    def way(self, w):
        if w.tags.get("highway") not in ROADS:
            return
        en = w.tags.get("name:en")
        if en and en.strip():
            self.names[str(w.id)] = " ".join(en.split())


def main(src, dst):
    h = Names()
    h.apply_file(src, locations=False)
    if not h.names:
        sys.exit(f"no road in {src} has name:en: wrong extract?")
    out = {"tag": "name:en", "ways": len(h.names), "names": h.names}
    with open(dst, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    print(f"   {len(h.names)} roads with an English name -> {dst}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])

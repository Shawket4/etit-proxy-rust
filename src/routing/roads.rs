//! The major roads a route takes, in driving order: the fee lookup's
//! turn-by-turn-like list ("road 11 for 65.8 km, then road 244 for 168.3 km,
//! then the drop-off"). Not every turn: only the roads a driver would name.
//!
//! The input is the Valhalla edges the route drives, read by
//! `trace_attributes` on the route's own shape
//! ([`super::RoutingClient::roads`]). One code path summarises both of the fee
//! lookup's routes: Valhalla's truck route and the map-matched shape of our
//! trucks' best trip.
//!
//! 1. **Kinds.** An edge is a *road* (motorway to tertiary, named or not),
//!    *local* (residential, unclassified, service roads), or a *link* (a ramp,
//!    a turn channel, a roundabout, the inside of a junction).
//! 2. **Pieces.** Consecutive edges of the same road join. Same road means
//!    the same name, compared after normalising Arabic spelling (أ/ا, ى/ي,
//!    ة/ه, a letter or two on a long name), or, where one side has no name, a
//!    shared ref. Two differently named sections of one ref stay apart: on the
//!    coast, road 11 is "Ain Sokhna – Zaafarana", then "Zaafarana – Ras
//!    Ghareb", and drivers name them that way. A link joins the piece it
//!    leaves; local edges join each other.
//! 3. **Absorbing.** A link, or a piece shorter than [`min_km`] (2% of the
//!    route, kept between 3 and 10 km), is folded into the road before it
//!    (the one the truck is leaving), or the road after it at the very start.
//!    Its km go with it, so the list still adds up to the route. A short
//!    piece between two pieces of one road goes first, and the two join;
//!    then the shortest.
//!
//! Each stretch keeps the name, ref and class of the road it is, never of
//! what it absorbed, and the vertex range of the route's geometry it covers,
//! so the map can highlight it.
//!
//! [`compare`] says, by road, what one route takes that the other does not.

use serde::Serialize;

use crate::zones::geo::haversine_m;

/// Road classes that are roads in their own right; anything else is local.
const ROAD_CLASSES: [&str; 5] = ["motorway", "trunk", "primary", "secondary", "tertiary"];

/// A stretch shorter than this is folded into its neighbour.
pub fn min_km(total_km: f64) -> f64 {
    (total_km * 0.02).clamp(3.0, 10.0)
}

/// One edge of the route, as Valhalla's `trace_attributes` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct RoadEdge {
    /// Valhalla's names: refs ("11", "TAH 4") and names together.
    pub names: Vec<String>,
    /// `motorway` … `tertiary`, `unclassified`, `residential`, `service_other`.
    pub road_class: String,
    /// `road`, `ramp`, `turn_channel`, `service_road`, …
    pub road_use: String,
    /// A roundabout or the inside of a junction.
    pub junction: bool,
    pub km: f64,
    /// Vertex range of the traced shape this edge covers.
    pub begin: usize,
    pub end: usize,
}

/// One stretch of the list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoadStretch {
    /// OSM name as mapped (Arabic in Egypt); null for a road with no name and
    /// for local roads.
    pub name: Option<String>,
    /// Road number ("11", "244"); null when the road has none.
    #[serde(rename = "ref")]
    pub road_ref: Option<String>,
    /// Valhalla's class for most of the stretch (`trunk`, `residential`, …).
    pub road_class: String,
    pub km: f64,
    /// First and last vertex of the route's geometry on this stretch. The
    /// next stretch starts on this one's last vertex.
    pub from_index: usize,
    pub to_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Road,
    Local,
    Link,
}

/// Km per value (a spelling, a ref, a class), in first-seen order.
#[derive(Debug, Clone, Default)]
struct Tally(Vec<(String, f64)>);

impl Tally {
    fn add(&mut self, key: &str, km: f64) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v += km,
            None => self.0.push((key.to_string(), km)),
        }
    }
    fn merge(&mut self, other: &Tally) {
        for (k, v) in &other.0 {
            self.add(k, *v);
        }
    }
    /// The value with the most km; the first seen on a tie.
    fn top(&self) -> Option<&str> {
        let mut best: Option<&(String, f64)> = None;
        for e in &self.0 {
            if best.is_none_or(|b| e.1 > b.1) {
                best = Some(e);
            }
        }
        best.map(|(k, _)| k.as_str())
    }
    fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone)]
struct Piece {
    kind: Kind,
    names: Tally,
    refs: Tally,
    classes: Tally,
    km: f64,
    begin: usize,
    end: usize,
}

impl Piece {
    fn of(e: &RoadEdge) -> Self {
        let kind = if e.junction || e.road_use == "ramp" || e.road_use == "turn_channel" {
            Kind::Link
        } else if e.road_use == "road" && ROAD_CLASSES.contains(&e.road_class.as_str()) {
            Kind::Road
        } else {
            Kind::Local
        };
        let mut p = Piece {
            kind,
            names: Tally::default(),
            refs: Tally::default(),
            classes: Tally::default(),
            km: e.km.max(0.0),
            begin: e.begin,
            end: e.end.max(e.begin),
        };
        p.classes.add(&e.road_class, p.km);
        if kind == Kind::Road {
            // Valhalla lists refs and names together; the first of each is
            // the one shown (the national ref comes before "TAH 4").
            if let Some(r) = e.names.iter().map(|n| n.trim()).find(|n| is_ref(n)) {
                p.refs.add(r, p.km);
            }
            if let Some(n) = e.names.iter().map(|n| n.trim()).find(|n| !n.is_empty() && !is_ref(n)) {
                p.names.add(n, p.km);
            }
        }
        p
    }

    /// Another piece of the same road, after this one.
    fn join(&mut self, next: Piece) {
        self.names.merge(&next.names);
        self.refs.merge(&next.refs);
        self.classes.merge(&next.classes);
        self.km += next.km;
        self.end = next.end;
    }

    /// A short piece folded in: its km and vertices, not its name.
    fn absorb_after(&mut self, next: Piece) {
        self.km += next.km;
        self.end = next.end;
        if self.classes.is_empty() {
            self.classes = next.classes;
        }
    }

    fn absorb_before(&mut self, prev: Piece) {
        self.km += prev.km;
        self.begin = prev.begin;
        if self.classes.is_empty() {
            self.classes = prev.classes;
        }
    }
}

fn same_road(a: &Piece, b: &Piece) -> bool {
    if a.kind != b.kind {
        return false;
    }
    if a.kind != Kind::Road {
        return true;
    }
    match (a.names.is_empty(), b.names.is_empty()) {
        (false, false) => a.names.keys().any(|x| b.names.keys().any(|y| names_match(x, y))),
        _ if !a.refs.is_empty() || !b.refs.is_empty() => a.refs.keys().any(|x| b.refs.keys().any(|y| x == y)),
        // Two runs of unnamed, unnumbered road.
        (true, true) => true,
        _ => false,
    }
}

/// The route's major roads in driving order. Vertex ranges are those of the
/// traced shape; see [`reindex`] for the geometry the client draws.
pub fn summarise(edges: &[RoadEdge]) -> Vec<RoadStretch> {
    let total: f64 = edges.iter().map(|e| e.km.max(0.0)).sum();
    let floor = min_km(total);

    let mut pieces: Vec<Piece> = Vec::new();
    for e in edges {
        let p = Piece::of(e);
        match pieces.last_mut() {
            Some(last) if p.kind == Kind::Link => last.absorb_after(p),
            Some(last) if same_road(last, &p) => last.join(p),
            _ => pieces.push(p),
        }
    }

    loop {
        collapse(&mut pieces);
        if pieces.len() <= 1 {
            break;
        }
        // Links first, then a short piece between two pieces of one road
        // (folding it joins them), then the shortest under the floor.
        let rank = |i: usize| {
            let bridge = i > 0 && i + 1 < pieces.len() && same_road(&pieces[i - 1], &pieces[i + 1]);
            (pieces[i].kind != Kind::Link, !bridge)
        };
        let pick = (0..pieces.len())
            .filter(|&i| pieces[i].kind == Kind::Link || pieces[i].km < floor)
            .min_by(|&i, &j| rank(i).cmp(&rank(j)).then(pieces[i].km.total_cmp(&pieces[j].km)));
        let Some(i) = pick else { break };
        let p = pieces.remove(i);
        if i > 0 {
            pieces[i - 1].absorb_after(p);
        } else {
            pieces[0].absorb_before(p);
        }
    }

    pieces
        .into_iter()
        .map(|p| RoadStretch {
            name: p.names.top().map(str::to_string),
            road_ref: p.refs.top().map(str::to_string),
            road_class: p.classes.top().unwrap_or("").to_string(),
            km: round1(p.km),
            from_index: p.begin,
            to_index: p.end,
        })
        .collect()
}

fn collapse(pieces: &mut Vec<Piece>) {
    let mut i = 0;
    while i + 1 < pieces.len() {
        if same_road(&pieces[i], &pieces[i + 1]) {
            let next = pieces.remove(i + 1);
            pieces[i].join(next);
        } else {
            i += 1;
        }
    }
}

/// Moves the stretches' vertex ranges from the traced shape onto `served`,
/// the geometry the client draws. Both run along the same roads (the trace
/// was made from `served`), but vertex for vertex they may differ: `served`
/// is precision 5, and a map-snapped trace has its own vertices. Each
/// boundary goes to the nearest vertex of `served` from the previous
/// boundary on, so a route that passes a place twice keeps its order. The
/// first stretch starts at vertex 0 and the last ends on the last vertex.
pub fn reindex(stretches: &mut [RoadStretch], trace: &[(f64, f64)], served: &[(f64, f64)]) {
    let (Some(last_served), Some(last_trace)) = (served.len().checked_sub(1), trace.len().checked_sub(1)) else {
        return;
    };
    let n = stretches.len();
    let mut cursor = 0;
    for (k, s) in stretches.iter_mut().enumerate() {
        s.from_index = cursor;
        s.to_index = if k + 1 == n {
            last_served
        } else {
            locate(served, trace[s.to_index.min(last_trace)], cursor)
        };
        cursor = s.to_index;
    }
}

/// Index of the vertex of `path`, at or after `start`, nearest to `p`: the
/// first close pass, not a later one.
fn locate(path: &[(f64, f64)], p: (f64, f64), start: usize) -> usize {
    const NEAR_M: f64 = 50.0;
    const LEFT_M: f64 = 500.0;
    let mut best = (f64::INFINITY, start.min(path.len().saturating_sub(1)));
    for (i, q) in path.iter().enumerate().skip(start) {
        let d = haversine_m(q.0, q.1, p.0, p.1);
        if d < best.0 {
            best = (d, i);
        } else if best.0 <= NEAR_M && d > best.0 + LEFT_M {
            break;
        }
    }
    best.1
}

/// One road in [`RoadDifference`], with how much of it each route drives.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoadShare {
    /// The road's name; null for a numbered road whose sections carry
    /// several names (shown by its ref), or one with no name.
    pub name: Option<String>,
    #[serde(rename = "ref")]
    pub road_ref: Option<String>,
    pub best_km: f64,
    pub truck_km: f64,
}

/// Where the two routes differ in road terms.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoadDifference {
    /// Roads our trucks' best trip drives much more of than the truck route,
    /// most extra km first.
    pub best_more: Vec<RoadShare>,
    /// Roads the truck route drives much more of than our trucks do.
    pub truck_more: Vec<RoadShare>,
}

/// "Much more": at least this many km, and a quarter of the road's longer
/// share. Below that the two routes are on the same road, give or take
/// where a stretch was cut.
const DIFF_MIN_KM: f64 = 3.0;
const DIFF_MIN_SHARE: f64 = 0.25;

/// Compares the two routes' stretches road by road. A road is the set of
/// stretches, on either route, that share a ref or a name; unnamed roads and
/// local roads cannot be matched and are left out.
pub fn compare(best: &[RoadStretch], truck: &[RoadStretch]) -> RoadDifference {
    struct Road {
        names: Tally,
        refs: Tally,
        best_km: f64,
        truck_km: f64,
    }
    let mut roads: Vec<Road> = Vec::new();
    let tagged = best.iter().map(|s| (true, s)).chain(truck.iter().map(|s| (false, s)));
    for (is_best, s) in tagged {
        if s.name.is_none() && s.road_ref.is_none() {
            continue;
        }
        let belongs = |r: &Road| {
            s.road_ref.as_deref().is_some_and(|x| r.refs.keys().any(|y| x == y))
                || s.name.as_deref().is_some_and(|x| r.names.keys().any(|y| names_match(x, y)))
        };
        let mut hits = roads.iter().enumerate().filter(|(_, r)| belongs(r)).map(|(i, _)| i).collect::<Vec<_>>();
        let at = match hits.first().copied() {
            None => {
                roads.push(Road { names: Tally::default(), refs: Tally::default(), best_km: 0.0, truck_km: 0.0 });
                roads.len() - 1
            }
            Some(first) => {
                // One stretch can tie two roads together (one by ref, one
                // by name): they are one road.
                for j in hits.drain(1..).rev() {
                    let other = roads.remove(j);
                    let r = &mut roads[first];
                    r.names.merge(&other.names);
                    r.refs.merge(&other.refs);
                    r.best_km += other.best_km;
                    r.truck_km += other.truck_km;
                }
                first
            }
        };
        let r = &mut roads[at];
        if let Some(n) = &s.name {
            r.names.add(n, s.km);
        }
        if let Some(x) = &s.road_ref {
            r.refs.add(x, s.km);
        }
        if is_best {
            r.best_km += s.km;
        } else {
            r.truck_km += s.km;
        }
    }

    let share = |r: &Road| {
        let mut distinct: Vec<String> = Vec::new();
        for n in r.names.keys() {
            if !distinct.iter().any(|d| names_match(d, n)) {
                distinct.push(n.to_string());
            }
        }
        let road_ref = r.refs.top().map(str::to_string);
        RoadShare {
            // A numbered road with several section names is named by its ref.
            name: if distinct.len() > 1 && road_ref.is_some() { None } else { r.names.top().map(str::to_string) },
            road_ref,
            best_km: round1(r.best_km),
            truck_km: round1(r.truck_km),
        }
    };
    let more = |a: f64, b: f64| a - b >= DIFF_MIN_KM.max(DIFF_MIN_SHARE * a.max(b));
    let mut best_more: Vec<RoadShare> = roads.iter().filter(|r| more(r.best_km, r.truck_km)).map(share).collect();
    let mut truck_more: Vec<RoadShare> = roads.iter().filter(|r| more(r.truck_km, r.best_km)).map(share).collect();
    best_more.sort_by(|a, b| (b.best_km - b.truck_km).total_cmp(&(a.best_km - a.truck_km)));
    truck_more.sort_by(|a, b| (b.truck_km - b.best_km).total_cmp(&(a.truck_km - a.best_km)));
    RoadDifference { best_more, truck_more }
}

/// A road number as OSM refs are written in Egypt: "11", "244", "TAH 4",
/// "M1". Never a name.
pub fn is_ref(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > 10 {
        return false;
    }
    let mut chars = s.chars().peekable();
    let mut letters = 0;
    while chars.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
        chars.next();
        letters += 1;
    }
    if letters > 4 {
        return false;
    }
    if letters > 0 && chars.peek().is_some_and(|c| *c == ' ' || *c == '-') {
        chars.next();
    }
    let mut digits = 0;
    while chars.peek().is_some_and(|c| digit(*c).is_some()) {
        chars.next();
        digits += 1;
    }
    if digits == 0 || digits > 4 {
        return false;
    }
    if chars.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
        chars.next();
    }
    chars.next().is_none()
}

/// An ASCII or Arabic-Indic digit, as ASCII.
fn digit(c: char) -> Option<char> {
    match c {
        '0'..='9' => Some(c),
        '\u{0660}'..='\u{0669}' => char::from_digit(c as u32 - 0x0660, 10),
        '\u{06F0}'..='\u{06F9}' => char::from_digit(c as u32 - 0x06F0, 10),
        _ => None,
    }
}

/// A road name in one spelling: Arabic letter variants folded (أإآٱ → ا,
/// ى → ي, ة → ه, ؤ → و, ئ → ي), marks and tatweel dropped, digits ASCII, the
/// "X, Y" / "X – Y" separators one comma, spaces single, Latin lower case.
pub fn normalise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let c = match c {
            '\u{064B}'..='\u{0652}' | '\u{0670}' | '\u{0640}' => continue,
            'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
            'ى' | 'ئ' => 'ي',
            'ة' => 'ه',
            'ؤ' => 'و',
            '،' | ',' | '-' | '–' | '—' => ',',
            c if c.is_whitespace() => ' ',
            c => digit(c).unwrap_or(c),
        };
        if c == ' ' && (out.is_empty() || out.ends_with(' ') || out.ends_with(',')) {
            continue;
        }
        if c == ',' && out.ends_with(' ') {
            out.pop();
        }
        out.extend(c.to_lowercase());
    }
    out.trim_end().to_string()
}

/// Two OSM names for one road. Long names may differ by a letter or two
/// (الواسطى / الوسطي), never in their numbers.
pub fn names_match(a: &str, b: &str) -> bool {
    let (a, b) = (normalise(a), normalise(b));
    if a == b {
        return true;
    }
    let numbers = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    if numbers(&a) != numbers(&b) {
        return false;
    }
    let len = a.chars().count().min(b.chars().count());
    let allowed = match len {
        20.. => 2,
        10.. => 1,
        _ => 0,
    };
    allowed > 0 && edit_distance(&a, &b) <= allowed
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An edge of `km`, its vertices following on from the previous one.
    struct Edges {
        list: Vec<RoadEdge>,
        at: usize,
    }

    impl Edges {
        fn new() -> Self {
            Self { list: Vec::new(), at: 0 }
        }
        fn push(&mut self, names: &[&str], class: &str, road_use: &str, km: f64) -> &mut Self {
            // A vertex every ~500 m.
            let n = ((km * 2.0).ceil() as usize).max(1);
            self.list.push(RoadEdge {
                names: names.iter().map(|s| s.to_string()).collect(),
                road_class: class.into(),
                road_use: road_use.into(),
                junction: false,
                km,
                begin: self.at,
                end: self.at + n,
            });
            self.at += n;
            self
        }
        fn road(&mut self, names: &[&str], class: &str, km: f64) -> &mut Self {
            self.push(names, class, "road", km)
        }
    }

    fn labels(s: &[RoadStretch]) -> Vec<(String, f64)> {
        s.iter()
            .map(|x| {
                let label = match (&x.name, &x.road_ref) {
                    (Some(n), Some(r)) => format!("{n} [{r}]"),
                    (Some(n), None) => n.clone(),
                    (None, Some(r)) => format!("[{r}]"),
                    (None, None) => format!("({})", x.road_class),
                };
                (label, x.km)
            })
            .collect()
    }

    /// Somed -> Qena as our trucks drive it (edges trimmed from the real
    /// trace, 2026-11-27): the coast road 11 in its named sections, then 244.
    fn coast() -> Vec<RoadEdge> {
        let mut e = Edges::new();
        e.road(&["11", "طريق العين السخنه, الزعفرانه"], "trunk", 40.2)
            .push(&[], "trunk", "ramp", 0.3)
            .road(&["11"], "trunk", 1.1)
            .road(&["11", "طريق العين السخنه, الزعفرانه"], "trunk", 24.2)
            .road(&["11", "طريق الزعفرانه, رأس غارب"], "trunk", 101.9)
            .road(&["11", "طريق راس غارب, الغردقة"], "trunk", 151.8)
            .road(&["11", "طريق الغردقة, سفاجا"], "trunk", 12.9)
            .road(&[], "trunk", 0.4)
            .road(&["11", "طريق الغردقة, سفاجا"], "trunk", 31.9)
            .push(&[], "trunk", "turn_channel", 0.2)
            .road(&["11", "طريق الغردقة, سفاجا"], "trunk", 8.8)
            .road(&["244", "طريق قنا, سفاجا"], "trunk", 41.7)
            .road(&["244", "طريق قنا, سفاجا"], "trunk", 123.5)
            .road(&["25", "TAH 4", "طريق اسيوط, قنا الزراعى"], "trunk", 2.6)
            .push(&["شارع الجمهورية"], "residential", "road", 0.8);
        e.list
    }

    #[test]
    fn the_coast_road_reads_section_by_section_then_244_to_the_drop_off() {
        let s = summarise(&coast());
        assert_eq!(
            labels(&s),
            vec![
                ("طريق العين السخنه, الزعفرانه [11]".to_string(), 65.8),
                ("طريق الزعفرانه, رأس غارب [11]".to_string(), 101.9),
                ("طريق راس غارب, الغردقة [11]".to_string(), 151.8),
                ("طريق الغردقة, سفاجا [11]".to_string(), 54.2),
                // The last 3.4 km of town streets fold into the road that
                // brings the truck in.
                ("طريق قنا, سفاجا [244]".to_string(), 168.6),
            ]
        );
        let total: f64 = coast().iter().map(|e| e.km).sum();
        let listed: f64 = s.iter().map(|x| x.km).sum();
        assert!((listed - total).abs() < 0.2, "{listed} vs {total}: the list adds up to the route");
        // Contiguous, in order, from the first vertex to the last.
        assert_eq!(s[0].from_index, 0);
        for w in s.windows(2) {
            assert_eq!(w[0].to_index, w[1].from_index);
        }
        assert_eq!(s.last().unwrap().to_index, coast().last().unwrap().end);
    }

    #[test]
    fn short_links_and_town_streets_at_the_start_join_the_first_road() {
        let mut e = Edges::new();
        e.push(&["شارع المحطة"], "residential", "road", 0.6)
            .push(&[], "service_other", "service_road", 0.4)
            .push(&[], "primary", "ramp", 0.5)
            .road(&["16", "طريق القاهرة, السويس"], "trunk", 40.0);
        let s = summarise(&e.list);
        assert_eq!(labels(&s), vec![("طريق القاهرة, السويس [16]".to_string(), 41.5)]);
        assert_eq!((s[0].from_index, s[0].road_class.as_str()), (0, "trunk"));
    }

    #[test]
    fn a_short_road_between_two_pieces_of_one_road_disappears_into_it() {
        let mut e = Edges::new();
        e.road(&["طريق القاهرة, اسيوط الصحراوي"], "primary", 49.6)
            .road(&[], "primary", 2.7)
            .road(&["طريق القاهرة, اسيوط الصحراوي"], "motorway", 2.3)
            .road(&["طريق المنيا, الواسطى"], "primary", 100.0);
        let s = summarise(&e.list);
        assert_eq!(
            labels(&s),
            vec![("طريق القاهرة, اسيوط الصحراوي".to_string(), 54.6), ("طريق المنيا, الواسطى".to_string(), 100.0)]
        );
    }

    #[test]
    fn spellings_of_one_name_are_one_road() {
        let mut e = Edges::new();
        e.road(&["25", "TAH 4", "طريق اسيوط, قنا الزراعي"], "trunk", 84.0)
            .road(&["25", "TAH 4", "طريق اسيوط, قنا الزراعى"], "trunk", 58.7)
            .road(&["طريق المنيا, الواسطى"], "primary", 100.0)
            .road(&["طريق المنيا, الوسطي"], "primary", 9.9);
        let s = summarise(&e.list);
        assert_eq!(s.len(), 2, "{:?}", labels(&s));
        assert_eq!(s[0].road_ref.as_deref(), Some("25"), "the national ref, not TAH 4");
        assert_eq!(s[0].name.as_deref(), Some("طريق اسيوط, قنا الزراعي"), "the spelling with the most km");
        assert_eq!(s[1].km, 109.9);
    }

    #[test]
    fn long_unnamed_road_and_long_local_runs_stay_as_their_own_stretches() {
        let mut e = Edges::new();
        e.road(&["طريق العين السخنه"], "trunk", 36.9)
            .road(&[], "trunk", 30.0)
            .road(&[], "primary", 11.4)
            .push(&["شارع 1"], "residential", "road", 6.0)
            .push(&["شارع 2"], "unclassified", "road", 7.0)
            .road(&["515", "الطريق الدائري الإقليمي"], "motorway", 33.1);
        let s = summarise(&e.list);
        assert_eq!(
            labels(&s),
            vec![
                ("طريق العين السخنه".to_string(), 36.9),
                ("(trunk)".to_string(), 41.4),
                // Local roads are a stretch, not a street name.
                ("(unclassified)".to_string(), 13.0),
                ("الطريق الدائري الإقليمي [515]".to_string(), 33.1),
            ]
        );
    }

    #[test]
    fn a_ref_joins_an_unnamed_piece_to_its_named_road_but_never_two_names() {
        let mut e = Edges::new();
        e.road(&["11"], "trunk", 20.0)
            .road(&["11", "طريق العين السخنه, الزعفرانه"], "trunk", 40.0)
            .road(&["11", "طريق الزعفرانه, رأس غارب"], "trunk", 100.0);
        let s = summarise(&e.list);
        assert_eq!(
            labels(&s),
            vec![
                ("طريق العين السخنه, الزعفرانه [11]".to_string(), 60.0),
                ("طريق الزعفرانه, رأس غارب [11]".to_string(), 100.0),
            ]
        );
    }

    #[test]
    fn the_floor_scales_with_the_route() {
        assert_eq!(min_km(50.0), 3.0);
        assert_eq!(min_km(300.0), 6.0);
        assert_eq!(min_km(700.0), 10.0);
        // On a short route a 4 km road is a road.
        let mut e = Edges::new();
        e.road(&["طريق أ"], "primary", 10.0).road(&["طريق ب"], "primary", 4.0).road(&["طريق ج"], "primary", 12.0);
        assert_eq!(summarise(&e.list).len(), 3);
        // On a long one it is folded in.
        let mut e = Edges::new();
        e.road(&["طريق أ"], "primary", 300.0).road(&["طريق ب"], "primary", 4.0).road(&["طريق ج"], "primary", 300.0);
        let s = summarise(&e.list);
        assert_eq!(labels(&s), vec![("طريق أ".to_string(), 304.0), ("طريق ج".to_string(), 300.0)]);
    }

    #[test]
    fn nothing_and_one_edge_are_fine() {
        assert!(summarise(&[]).is_empty());
        let mut e = Edges::new();
        e.push(&[], "primary", "ramp", 0.4);
        let s = summarise(&e.list);
        assert_eq!((s.len(), s[0].name.clone(), s[0].road_class.as_str()), (1, None, "primary"));
    }

    #[test]
    fn refs_are_numbers_and_names_are_not() {
        for r in ["11", "244", "TAH 4", "M1", "N-3", "١١", "25A"] {
            assert!(is_ref(r), "{r}");
        }
        for n in ["طريق 11", "شارع 26 يوليو", "Ring Road", "", "12345", "ABCDE 1"] {
            assert!(!is_ref(n), "{n}");
        }
    }

    #[test]
    fn names_compare_across_spellings_but_not_across_numbers() {
        assert_eq!(normalise("طريق القاهرة،  أسيوط"), "طريق القاهره,اسيوط");
        assert_eq!(normalise("Cairo - Suez Road"), "cairo,suez road");
        assert!(names_match("طريق اسيوط, قنا الزراعي", "طريق أسيوط - قنا الزراعى"));
        assert!(names_match("طريق المنيا, الواسطى", "طريق المنيا, الوسطي"));
        assert!(!names_match("شارع 26 يوليو", "شارع 23 يوليو"));
        assert!(!names_match("طريق اسيوط, قنا", "طريق اسوان, قنا"));
        assert!(!names_match("شارع أ", "شارع ب"), "short names must match exactly");
    }

    #[test]
    fn ranges_move_onto_the_served_geometry() {
        // The trace has extra vertices (map-snapped) the served line lacks.
        let served: Vec<(f64, f64)> = (0..=100).map(|i| (30.0, 31.0 + i as f64 * 0.01)).collect();
        let trace: Vec<(f64, f64)> = (0..=200).map(|i| (30.0, 31.0 + i as f64 * 0.005)).collect();
        let mut s = vec![
            RoadStretch { name: None, road_ref: Some("1".into()), road_class: "trunk".into(), km: 1.0, from_index: 0, to_index: 60 },
            RoadStretch { name: None, road_ref: Some("2".into()), road_class: "trunk".into(), km: 1.0, from_index: 60, to_index: 140 },
            RoadStretch { name: None, road_ref: Some("3".into()), road_class: "trunk".into(), km: 1.0, from_index: 140, to_index: 199 },
        ];
        reindex(&mut s, &trace, &served);
        let ranges: Vec<(usize, usize)> = s.iter().map(|x| (x.from_index, x.to_index)).collect();
        assert_eq!(ranges, vec![(0, 30), (30, 70), (70, 100)]);
    }

    #[test]
    fn a_route_through_one_place_twice_keeps_its_order() {
        // Out east and back west along the same road, then north.
        let mut served: Vec<(f64, f64)> = (0..=50).map(|i| (30.0, 31.0 + i as f64 * 0.01)).collect();
        served.extend((0..50).rev().map(|i| (30.0, 31.0 + i as f64 * 0.01)));
        served.extend((1..=20).map(|i| (30.0 + i as f64 * 0.01, 31.0)));
        // The boundary is the turn-back point, then the start again.
        let trace = served.clone();
        let mut s = vec![
            RoadStretch { name: None, road_ref: Some("1".into()), road_class: "trunk".into(), km: 1.0, from_index: 0, to_index: 50 },
            RoadStretch { name: None, road_ref: Some("2".into()), road_class: "trunk".into(), km: 1.0, from_index: 50, to_index: 100 },
            RoadStretch { name: None, road_ref: Some("3".into()), road_class: "trunk".into(), km: 1.0, from_index: 100, to_index: 120 },
        ];
        reindex(&mut s, &trace, &served);
        let ranges: Vec<(usize, usize)> = s.iter().map(|x| (x.from_index, x.to_index)).collect();
        assert_eq!(ranges, vec![(0, 50), (50, 100), (100, 120)]);
    }

    fn stretch(name: Option<&str>, road_ref: Option<&str>, km: f64) -> RoadStretch {
        RoadStretch {
            name: name.map(str::to_string),
            road_ref: road_ref.map(str::to_string),
            road_class: "trunk".into(),
            km,
            from_index: 0,
            to_index: 0,
        }
    }

    #[test]
    fn the_coast_against_the_inland_route_reads_road_by_road() {
        let best = summarise(&coast());
        // Somed -> Qena on Valhalla's time-based truck route (2026-11-27).
        let truck = vec![
            stretch(Some("طريق السويس, العين السخنه الجانبي"), Some("11"), 18.2),
            stretch(Some("طريق العين السخنه"), None, 36.9),
            stretch(None, None, 41.4),
            stretch(Some("الطريق الدائري الإقليمي"), Some("515"), 33.6),
            stretch(Some("طريق القاهرة, اسيوط الصحراوي"), None, 50.7),
            stretch(Some("طريق المنيا, الواسطى"), None, 127.6),
            stretch(Some("طريق اسيوط, قنا الزراعي"), Some("25"), 144.9),
        ];
        let d = compare(&best, &truck);
        let refs = |v: &[RoadShare]| v.iter().map(|r| (r.name.clone(), r.road_ref.clone())).collect::<Vec<_>>();
        assert_eq!(
            refs(&d.best_more),
            vec![
                // Four section names on one number: named by the number.
                (None, Some("11".to_string())),
                (Some("طريق قنا, سفاجا".to_string()), Some("244".to_string())),
            ]
        );
        assert_eq!((d.best_more[0].best_km, d.best_more[0].truck_km), (373.7, 18.2));
        assert_eq!(d.truck_more[0].road_ref.as_deref(), Some("25"));
        assert_eq!(d.truck_more[1].name.as_deref(), Some("طريق المنيا, الواسطى"));
        assert_eq!(d.truck_more.len(), 5, "{:?}", refs(&d.truck_more));
        assert!(d.truck_more.iter().all(|r| r.name.is_some() || r.road_ref.is_some()), "unnamed road cannot be matched");
    }

    #[test]
    fn the_same_roads_cut_a_little_differently_are_no_difference() {
        let a = vec![stretch(Some("طريق القاهرة, السويس"), Some("16"), 40.0), stretch(Some("طريق السويس, العين السخنه"), None, 60.0)];
        let b = vec![stretch(None, Some("16"), 42.5), stretch(Some("طريق السويس - العين السخنة"), None, 57.0)];
        assert_eq!(compare(&a, &b), RoadDifference { best_more: vec![], truck_more: vec![] });
        assert_eq!(compare(&[], &[]), RoadDifference { best_more: vec![], truck_more: vec![] });
    }

    #[test]
    fn a_stretch_matched_by_ref_and_another_by_name_ties_them_into_one_road() {
        let best = vec![stretch(None, Some("15"), 20.0), stretch(Some("طريق القاهرة, الاسماعيليه الصحراوي"), None, 10.0)];
        let truck = vec![stretch(Some("طريق القاهرة, الاسماعيليه الصحراوي"), Some("15"), 30.0)];
        let d = compare(&best, &truck);
        assert!(d.best_more.is_empty() && d.truck_more.is_empty(), "{d:?}");
    }

    #[test]
    fn stretches_serialise_with_ref_and_nulls() {
        let v = serde_json::to_value(stretch(None, Some("11"), 65.8)).unwrap();
        assert_eq!(v["ref"], "11");
        assert!(v["name"].is_null());
        for k in ["road_class", "km", "from_index", "to_index"] {
            assert!(v.get(k).is_some(), "{k}");
        }
    }
}

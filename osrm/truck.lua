-- Truck profile for Egypt (OSRM 5.5+ / api_version 4)
-- Based on car.lua, modified for trucks >3.5t, >4m height
-- Blocks: hgv=no, narrow roads penalized, 26th July Corridor blocked

api_version = 4

Set = require('lib/set')
Sequence = require('lib/sequence')
Handlers = require("lib/way_handlers")
Relations = require("lib/relations")
find_access_tag = require("lib/access").find_access_tag
limit = require("lib/maxspeed").limit
Utils = require("lib/utils")
Measure = require("lib/measure")

-- Egypt blocked route names
local egypt_blocked_names = {
  "26th july", "26 يوليو", "26 july", "26th of july",
  "طريق 26 يوليو", "كورنيش 26 يوليو"
}

local function is_egypt_blocked(way)
  local name = way:get_value_by_key("name")
  local name_ar = way:get_value_by_key("name:ar")

  local function check_name(n)
    if not n or n == "" then return false end
    local lower = string.lower(n)
    for _, blocked in ipairs(egypt_blocked_names) do
      if string.find(lower, string.lower(blocked)) then
        return true
      end
    end
    return false
  end

  return check_name(name) or check_name(name_ar)
end

-- ---------------------------------------------------------------------------
-- Cairo Ring Road: heavy trucks (>=4t) banned 06:00-24:00 (PM decree 1721/2018),
-- allowed 00:00-06:00. We tag its ways with the excludable class
-- 'daytimebanned' so the application layer can send exclude=daytimebanned
-- during banned hours. The Regional Ring Road (الطريق الدائري الإقليمي) is the
-- designated 24/7 truck bypass and must NEVER carry this class — its name also
-- contains "الدائري", so exclusion patterns below are checked first.
--
-- LIMITATION: Lua profiles have no geometry access at process_way time, so a
-- greater-Cairo bounding box filter is impossible; we rely on ref/name only.
--
-- Matching rules, verified against egypt-latest.osm.pbf (2026-08-18):
--   * The Cairo Ring Road carries ref=516 on the ENTIRE corridor: mainline
--     (motorway + trunk sections, name "الطريق الدائري" / "The Ring Road"),
--     all its motorway_link/trunk_link ramps, and bridge segments (Mounib
--     Axis Bridge, Adly Mansour Bridge). ref=516 appears nowhere else in
--     Egypt -> matching the ref token "516" is exact: zero false positives.
--   * The Regional Ring Road is ref=515 ("الطريق الدائري الإقليمي") and the
--     Middle Ring Road ("الطريق الدائري الأوسطى") has no ref; neither carries
--     516, so neither is tagged. Exclusion patterns guard them anyway in case
--     a way ever carries a combined ref like "515;516" or gets renamed.
--   * Generic substring matching on "الطريق الدائري"/"Ring Road" was tested
--     against the extract and REJECTED: it caught ~250 ways of other cities'
--     ring roads (Hurghada, Assiut, Sharm El-Sheikh, Ismailia, Tanta,
--     Mansoura, Luxor, Damietta...). The name-based fallback below therefore
--     only matches names that explicitly say "Cairo ring" (none in current
--     data; kept for robustness against future retagging).
-- ---------------------------------------------------------------------------

-- Ways matching any of these are never tagged, even if a ref matches
-- (plain substring match on name, name:ar, name:en, ref):
local ring_road_exclude_patterns = {
  "الإقليمي",   -- Regional Ring Road (with hamza)
  "الاقليمي",   -- Regional (hamza-less spelling)
  "regional",
  "الأوسط",     -- Middle Ring Road (الأوسطى/الأوسطي)
  "الاوسط",
  "middle",
}

-- Name-based fallback patterns (must explicitly reference Cairo):
local cairo_ring_name_patterns = {
  "دائري القاهرة",
  "دائرى القاهرة",
  "cairo ring",
}

local CAIRO_RING_REF = "516"

local function is_daytimebanned_ring_road(way)
  local fields = {
    way:get_value_by_key("name"),
    way:get_value_by_key("name:ar"),
    way:get_value_by_key("name:en"),
    way:get_value_by_key("ref"),
  }

  local matched = false

  -- exclusions first: Regional / Middle ring roads are never tagged
  for _, f in ipairs(fields) do
    if f and f ~= "" then
      local lower = string.lower(f)
      for _, pat in ipairs(ring_road_exclude_patterns) do
        if string.find(lower, pat, 1, true) then
          return false
        end
      end
      for _, pat in ipairs(cairo_ring_name_patterns) do
        if string.find(lower, pat, 1, true) then
          matched = true
        end
      end
    end
  end

  -- primary rule: ref token equals 516 (handles "516" and lists "516;xxx")
  local ref = way:get_value_by_key("ref")
  if ref and ref ~= "" then
    for token in string.gmatch(ref, "[^;,]+") do
      token = token:match("^%s*(.-)%s*$")  -- trim whitespace
      if token == CAIRO_RING_REF then
        matched = true
      end
    end
  end

  return matched
end

function setup()
  return {
    properties = {
      max_speed_for_map_matching      = 120/3.6, -- trucks max 120kmph
      weight_name                     = 'routability',
      process_call_tagless_node       = false,
      u_turn_penalty                  = 180,  -- trucks hate U-turns
      continue_straight_at_waypoint   = true,
      use_turn_restrictions           = true,
      left_hand_driving               = false,
      traffic_light_penalty           = 3,    -- trucks slower at lights
    },

    default_mode              = mode.driving,
    default_speed             = 10,
    oneway_handling           = true,
    side_road_multiplier      = 0.4,  -- penalize side roads hard for trucks
    turn_penalty              = 15,   -- trucks take longer to turn
    speed_reduction           = 0.8,
    turn_bias                 = 1.075,
    cardinal_directions       = false,

    -- Truck dimensions
    vehicle_height = 4.0,     -- meters
    vehicle_width  = 2.6,     -- meters
    vehicle_length = 16.5,    -- meters (truck + trailer)
    vehicle_weight = 40000,   -- kilograms (40 tonnes loaded tanker)

    suffix_list = {
      'N', 'NE', 'E', 'SE', 'S', 'SW', 'W', 'NW', 'North', 'South', 'West', 'East'
    },

    barrier_whitelist = Set {
      'cattle_grid',
      'border_control',
      'toll_booth',
      'sally_port',
      'gate',
      'lift_gate',
      'no',
      'entrance'
    },

    access_tag_whitelist = Set {
      'yes',
      'motorcar',
      'motor_vehicle',
      'vehicle',
      'permissive',
      'designated',
      'hgv'
    },

    access_tag_blacklist = Set {
      'no',
      'agricultural',
      'forestry',
      'emergency',
      'psv',
      'customers',
      'private',
      'delivery',
      'destination'
    },

    service_access_tag_blacklist = Set {
      'private'
    },

    restricted_access_tag_list = Set {
      'private',
      'delivery',
      'destination',
      'customers',
    },

    access_tags_hierarchy = Sequence {
      'hgv',           -- check hgv first for trucks
      'motorcar',
      'motor_vehicle',
      'vehicle',
      'access'
    },

    service_tag_forbidden = Set {
      'emergency_access'
    },

    restrictions = Sequence {
      'hgv',           -- truck-specific restrictions first
      'motorcar',
      'motor_vehicle',
      'vehicle'
    },

    -- 'daytimebanned' marks the Cairo Ring Road (truck ban 06:00-24:00,
    -- PM decree 1721/2018). The app layer sends exclude=daytimebanned during
    -- banned hours; at night (00:00-06:00) it queries without the exclude.
    classes = Sequence {
      'toll', 'motorway', 'ferry', 'restricted', 'tunnel', 'daytimebanned'
    },

    excludable = Sequence {
      Set {'toll'},
      Set {'motorway'},
      Set {'ferry'},
      Set {'daytimebanned'}
    },

    avoid = Set {
      'area',
      'reversible',
      'impassable',
      'hov_lanes',
      'steps',
      'construction',
      'proposed'
    },

    -- Truck-optimized speeds (km/h) - slower than cars
    speeds = Sequence {
      highway = {
        motorway        = 90,
        motorway_link   = 45,
        trunk           = 80,
        trunk_link      = 40,
        primary         = 70,
        primary_link    = 30,
        secondary       = 55,
        secondary_link  = 25,
        tertiary        = 45,
        tertiary_link   = 20,
        unclassified    = 20,
        residential     = 25,
        living_street   = 10,
        service         = 15
      }
    },

    service_penalties = {
      alley             = 0.3,  -- heavier penalty for trucks
      parking           = 0.3,
      parking_aisle     = 0.3,
      driveway          = 0.3,
      ["drive-through"] = 0.3,
      ["drive-thru"]    = 0.3
    },

    restricted_highway_whitelist = Set {
      'motorway',
      'motorway_link',
      'trunk',
      'trunk_link',
      'primary',
      'primary_link',
      'secondary',
      'secondary_link',
      'tertiary',
      'tertiary_link',
      'residential',
      'living_street',
      'unclassified',
      'service'
    },

    construction_whitelist = Set {
      'no',
      'widening',
      'minor',
    },

    route_speeds = {
      ferry = 5,
      shuttle_train = 10
    },

    bridge_speeds = {
      movable = 5
    },

    surface_speeds = {
      asphalt = nil,
      concrete = nil,
      ["concrete:plates"] = nil,
      ["concrete:lanes"] = nil,
      paved = nil,

      cement = 70,
      compacted = 70,
      fine_gravel = 60,

      paving_stones = 50,
      metal = 50,
      bricks = 50,

      grass = 20,       -- trucks much slower on grass
      wood = 30,
      sett = 30,
      grass_paver = 20,
      gravel = 30,
      unpaved = 25,
      ground = 20,
      dirt = 20,
      pebblestone = 25,
      tartan = 30,

      cobblestone = 20,
      clay = 15,

      earth = 10,
      stone = 10,
      rocky = 10,
      sand = 5,         -- trucks avoid sand

      mud = 5
    },

    tracktype_speeds = {
      -- Hard-blocked above; kept empty so no side channel re-enables tracks.
    },

    smoothness_speeds = {
      intermediate    = 60,
      bad             = 30,
      very_bad        = 15,
      horrible        = 5,
      very_horrible   = 3,
      impassable      = 0
    },

    -- Egypt-specific maxspeed defaults
    maxspeed_table_default = {
      urban = 50,
      rural = 80,       -- trucks slower on rural
      trunk = 90,
      motorway = 100    -- truck speed limit on motorways
    },

    maxspeed_table = {
      ["eg:urban"] = 50,
      ["eg:rural"] = 80,
      ["eg:motorway"] = 100,
      ["none"] = 100
    },

    relation_types = Sequence {
      "route"
    },

    highway_turn_classification = {
    },

    access_turn_classification = {
    }
  }
end

function process_node(profile, node, result, relations)
  local access = find_access_tag(node, profile.access_tags_hierarchy)
  if access then
    if profile.access_tag_blacklist[access] and not profile.restricted_access_tag_list[access] then
      result.barrier = true
    end
  else
    local barrier = node:get_value_by_key("barrier")
    if barrier then
      local restricted_by_height = false
      if barrier == 'height_restrictor' then
        local maxheight = Measure.get_max_height(node:get_value_by_key("maxheight"), node)
        restricted_by_height = maxheight and maxheight < profile.vehicle_height
      end

      local bollard = node:get_value_by_key("bollard")
      local rising_bollard = bollard and "rising" == bollard

      local kerb = node:get_value_by_key("kerb")
      local highway = node:get_value_by_key("highway")
      local flat_kerb = kerb and ("lowered" == kerb or "flush" == kerb)
      local highway_crossing_kerb = barrier == "kerb" and highway and highway == "crossing"

      if not profile.barrier_whitelist[barrier]
        and not rising_bollard
        and not flat_kerb
        and not highway_crossing_kerb
        or restricted_by_height then
        result.barrier = true
      end
    end
  end

  local tag = node:get_value_by_key("highway")
  if "traffic_signals" == tag then
    result.traffic_lights = true
  end
end

function process_way(profile, way, result, relations)
  -- Block Egypt-specific routes first
  if is_egypt_blocked(way) then
    return
  end

  -- Block hgv=no tagged ways
  local hgv = way:get_value_by_key("hgv")
  if hgv == "no" then
    return
  end

  -- A 40t tanker has no business on agricultural tracks, paths, or other
  -- unpaved farm roads regardless of tags — hard-block them so the optimal
  -- never routes through fields even when OSM says a track is "drivable".
  local hw = way:get_value_by_key("highway")
  if hw == "track" or hw == "path" or hw == "bridleway" or hw == "escape" then
    return
  end
  local surface = way:get_value_by_key("surface")
  if surface == "dirt" or surface == "sand" or surface == "mud" or surface == "earth"
      or surface == "grass" or surface == "ground" then
    return
  end

  -- Block truck=no tagged ways
  local truck_tag = way:get_value_by_key("truck")
  if truck_tag == "no" then
    return
  end

  local data = {
    highway = way:get_value_by_key('highway'),
    bridge = way:get_value_by_key('bridge'),
    route = way:get_value_by_key('route')
  }

  if (not data.highway or data.highway == '') and
     (not data.route or data.route == '')
  then
    return
  end

  local handlers = Sequence {
    WayHandlers.default_mode,
    WayHandlers.blocked_ways,
    WayHandlers.avoid_ways,
    WayHandlers.handle_height,
    WayHandlers.handle_width,
    WayHandlers.handle_length,
    WayHandlers.handle_weight,
    WayHandlers.access,
    WayHandlers.oneway,
    WayHandlers.destinations,
    WayHandlers.ferries,
    WayHandlers.movables,
    WayHandlers.service,
    WayHandlers.hov,
    WayHandlers.speed,
    WayHandlers.maxspeed,
    WayHandlers.surface,
    WayHandlers.penalties,
    WayHandlers.classes,
    WayHandlers.turn_lanes,
    WayHandlers.classification,
    WayHandlers.roundabouts,
    WayHandlers.startpoint,
    WayHandlers.driving_side,
    WayHandlers.names,
    WayHandlers.weights,
    WayHandlers.way_classification_for_turn
  }

  WayHandlers.run(profile, way, result, data, handlers, relations)

  -- Undivided (bidirectional) inter-city roads are slow going for a 40t
  -- tanker: oncoming traffic, town crossings, speed bumps. Egyptian desert
  -- highways are mapped as oneway dual-carriageway pairs while Nile-valley
  -- town roads are not — without this, OSRM routes tankers down the valley
  -- although the whole fleet (correctly) takes the coast road. Scale both
  -- speed and rate: routability weighs selection by rate.
  local ic = way:get_value_by_key("highway")
  if ic == "trunk" or ic == "primary" or ic == "secondary" then
    local oneway = way:get_value_by_key("oneway")
    if oneway ~= "yes" and oneway ~= "1" and oneway ~= "-1" then
      if result.forward_speed and result.forward_speed > 0 then
        result.forward_speed = result.forward_speed * 0.6
      end
      if result.backward_speed and result.backward_speed > 0 then
        result.backward_speed = result.backward_speed * 0.6
      end
      if result.forward_rate and result.forward_rate > 0 then
        result.forward_rate = result.forward_rate * 0.6
      end
      if result.backward_rate and result.backward_rate > 0 then
        result.backward_rate = result.backward_rate * 0.6
      end
    end
  end

  -- Tag Cairo Ring Road ways with the excludable 'daytimebanned' class
  -- (see comment block above is_daytimebanned_ring_road for the legal basis
  -- and the name-matching limitations).
  if (result.forward_mode ~= mode.inaccessible or result.backward_mode ~= mode.inaccessible)
     and is_daytimebanned_ring_road(way) then
    result.forward_classes["daytimebanned"] = true
    result.backward_classes["daytimebanned"] = true
  end

  -- Post-processing: penalize narrow roads
  if result.forward_speed > 0 or result.backward_speed > 0 then
    local lanes = way:get_value_by_key("lanes")
    if lanes then
      local lane_count = tonumber(lanes)
      if lane_count and lane_count < 2 then
        -- Heavy penalty for single-lane roads
        result.forward_speed = result.forward_speed * 0.3
        result.backward_speed = result.backward_speed * 0.3
        result.forward_rate = result.forward_rate * 0.3
        result.backward_rate = result.backward_rate * 0.3
      end
    end
  end

  if profile.cardinal_directions then
    Relations.process_way_refs(way, relations, result)
  end
end

function process_turn(profile, turn)
  local turn_penalty = profile.turn_penalty
  local turn_bias = turn.is_left_hand_driving and 1. / profile.turn_bias or profile.turn_bias

  if turn.has_traffic_light then
    turn.duration = profile.properties.traffic_light_penalty
  end

  if turn.number_of_roads > 2 or turn.source_mode ~= turn.target_mode or turn.is_u_turn then
    if turn.angle >= 0 then
      turn.duration = turn.duration + turn_penalty / (1 + math.exp( -((13 / turn_bias) *  turn.angle/180 - 6.5*turn_bias)))
    else
      turn.duration = turn.duration + turn_penalty / (1 + math.exp( -((13 * turn_bias) * -turn.angle/180 - 6.5/turn_bias)))
    end

    if turn.is_u_turn then
      turn.duration = turn.duration + profile.properties.u_turn_penalty
    end
  end

  if profile.properties.weight_name == 'distance' then
    turn.weight = 0
  else
    turn.weight = turn.duration
  end

  if profile.properties.weight_name == 'routability' then
    if not turn.source_restricted and turn.target_restricted then
      turn.weight = constants.max_turn_weight
    end
  end
end

return {
  setup = setup,
  process_way = process_way,
  process_node = process_node,
  process_turn = process_turn
}

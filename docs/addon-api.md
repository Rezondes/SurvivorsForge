# Azeroth Survivors: Addon API

> This is the addon API of Azeroth Survivors as **Aethus** published it, as of
> **1 November 2026**. The game may have changed since. Ask questions and look for a newer version in the
> [official addon channel on Discord](https://discord.com/channels/1557821819032051852/1564599899469914203).

Azeroth Survivors runs inside the World of Warcraft 1.12.1 client, so addons are ordinary 1.12
addons (`## Interface: 11200`, Lua, XML, the stock frame API). The game also lets addons read
the run's state: the clock, the phase, the bosses (alive and still to come), guardians and
elites, the hero and where it stands, spells, buffs, summons, totems, the meters, the banners and
the run's end. It gives them each of your hits with a place in the world to draw it, and the
device's local time. It also lets them hide the game's own HUD so they can draw their own, and keep
their frames over the level-up cards.

The data covers the current run only. Out of a run, the tables are empty or still hold the
previous run's values, so check `AZS_GetPhase()` first.

---

## Contents

1. [Installing an addon](#installing-an-addon)
2. [Quick start](#quick-start)
3. [Functions](#functions)
4. [Tables](#tables)
5. [The meter API (version 1)](#the-meter-api-version-1)
6. [World-anchored combat text (version 1)](#world-anchored-combat-text-version-1)
7. [Hiding the game's HUD](#hiding-the-games-hud)
8. [Raw addon messages](#raw-addon-messages)
9. [Reference values](#reference-values)
10. [Limits and tips](#limits-and-tips)
11. [Examples](#examples)

---

## Installing an addon

| build | where addons go |
|---|---|
| Windows / Linux | `benilla-config/AddOns/<AddonName>/` next to the game's executable |
| Browser | Options → AddOns → install a `.zip` of the addon folder |

An addon loads at the next world entry. Its `.toc` needs `## Interface: 11200` (other versions
load only with "Load out of date AddOns" on).

---

## Quick start

`AZSHello/AZSHello.toc`:

```
## Interface: 11200
## Title: AZS Hello
## Notes: Prints the run's clock and the next boss.
AZSHello.lua
```

`AZSHello/AZSHello.lua`:

```lua
local f = CreateFrame("Frame");
local elapsed = 0;
f:SetScript("OnUpdate", function()
	elapsed = elapsed + arg1;
	if ( elapsed < 5 ) then return; end
	elapsed = 0;
	if ( AZS_GetPhase() ~= "playing" ) then return; end
	local t = AZS_GetRunTime();
	local msg = string.format("%d:%02d", math.floor(t / 60), math.mod(t, 60));
	local next = AZS_GetUpcomingBosses()[1];
	if ( next ) then
		msg = msg .. string.format(" - %s in %ds", next.name, next.left);
	end
	DEFAULT_CHAT_FRAME:AddMessage(msg);
end);
```

Nothing has to be registered: the game fills the `AZS_*` globals before any addon loads and
keeps them current.

---

## Functions

### `AZS_GetRunTime()` → `seconds` or `nil`
How long the run has been going, in seconds, as a number. The server sends the clock five
times a second, and between its updates this keeps counting while the phase is `playing`, so it
reads smoothly in an `OnUpdate`. The clock stops while you choose a card or pause. `nil` before
the first update of a run.

### `AZS_GetPhase()` → `phase` or `nil`
One of `ready` (in the world, the run not started), `playing`, `choosing` (level-up or jewelry
cards on screen), `paused`, `dead`, `victory`.

### `AZS_GetRunLength()` → `seconds`
The second the zone's final boss comes: how long the zone lasts. It differs by zone (Elwynn
Forest is 960, 16:00). After the final boss dies the run can continue in endless mode.

### `AZS_GetBosses()` → `list`
Every boss alive right now (several can be up at once), bosses first in spawn order:

| field | type | meaning |
|---|---|---|
| `id` | number | the creature's spawn id, unique within the run |
| `name` | string | its name, as `UnitName` gives it |
| `hp`, `hpm` | number | current and maximum health |
| `final` | boolean | the zone's final boss |
| `enraged` | boolean | enraged: dropped below 30% health, and uses its abilities more often |
| `debuffs` | table | its debuffs, as `AZS_GetDebuffs(id)` returns them |

### `AZS_GetUpcomingBosses()` → `list`
The bosses still to come, in order:

| field | type | meaning |
|---|---|---|
| `index` | number | its place in the zone's boss order (1 = first) |
| `name` | string | its name |
| `at` | number | the run second it comes |
| `left` | number | seconds from now (from `AZS_GetRunTime()`) |
| `final` | boolean | the final boss |

Empty in endless mode, where bosses return on a timer: see `AZS_RUN.nb` and `AZS_RUN.nbn`.

### `AZS_GetGuardians()` → `list`
Every guardian alive. Guardians are elites that hold lairs on the map and wake when you come
near:

| field | type | meaning |
|---|---|---|
| `id` | number | spawn id |
| `name` | string | name |
| `hp`, `hpm` | number | current and maximum health |
| `awake` | boolean | fighting (woken up) |
| `debuffs` | table | its debuffs, as `AZS_GetDebuffs(id)` returns them |

### `AZS_GetDebuffs(id)` → `list`
The debuffs on a boss or guardian, by its `id` (from `AZS_GetBosses()` or `AZS_GetGuardians()`).
Unit frames can't read these for a unit that isn't your target or mouseover, so this is the way
to track them on boss and guardian frames:

| field | type | meaning |
|---|---|---|
| `spell` | number | spell id |
| `name` | string | display name |
| `icon` | string | texture path, ready for `SetTexture()` |
| `stacks` | number | stack count; 1 for an unstacked effect |
| `duration` | number or nil | total duration in seconds, when known |
| `remaining` | number or nil | seconds left, counted off the run's clock (`AZS_GetRunTime()`), when known |
| `permanent` | boolean or nil | `true` only for an effect with no expiration (Hunter's Mark and Faerie Fire stay while their target lives and stays in range); `nil` can mean unknown timing |
| `dispelType` | string or nil | `Magic`, `Curse`, `Disease` or `Poison` |

Call it each frame (or `OnUpdate`) for a smooth countdown: the list only changes when a debuff
comes, goes or is refreshed, but `remaining` is worked out on every call.

### `AZS_GetMeter()` → `lines, total, dps`
The damage meter. `lines` holds one entry per damage source, the most damage first:

| field | type | meaning |
|---|---|---|
| `name` | string | the spell, pet, talent or item that dealt it |
| `dmg` | number | damage dealt this run |
| `dps` | number | damage per second over the run's clock |
| `share` | number | its part of all the run's damage, 0 to 1 |
| `proc` | number | 1 for a talent's or amulet's own damage (shown in blue in the game's meter) |

`total` is all the run's damage and `dps` the run's damage per second. The numbers cover the
whole run: the Reset button on the game's meter window affects only that window.

### `AZS_GetHealing()` → `lines, total, hps`
The meter window's **Healing Done**: the same fields as `AZS_GetMeter()`, one line per source of
healing (a spell, regeneration, leech, healing on kill…), where `dmg` is the health it gave you
(overhealing left out) and `dps` that per second over the run's clock. `total` is all the run's
healing and `hps` per second. Covers the whole run, like the damage meter.

### `AZS_GetBossMeter()` → `lines, total, dps`
The meter window's **Boss Damage**: the damage dealt to bosses in the last boss fight, the same
fields as `AZS_GetMeter()`. A fight runs from the first hit on a boss while none was being
fought until the last boss up falls; `dps` is per second over that fight (so far, while it is
on). Empty before the first boss fight.

### `AZS_GetPartyMeter()` → `lines, total, dps`
The meter window's **Party DPS**: one line per hero, the most damage first. Empty out of a party.

| field | type | meaning |
|---|---|---|
| `name` | string | the hero's name |
| `class` | string | its class key (see [Reference values](#reference-values)) |
| `dmg` | number | damage dealt this run |
| `dps` | number | damage per second over the run's clock |
| `share` | number | its part of the party's damage, 0 to 1 |
| `top` | string | the hero's spell that dealt the most, in your language; empty for none |
| `topdps` | number | that spell's damage per second over the run's clock |

`total` is the party's damage and `dps` the party's per second.

### `AZS_GetTimers([name])` → `list`
Your talents' (and jewelry's) own clocks: the countdowns the build shows (Pact of the
Nathrezim's and Butcher's next demon) and every wait a talent keeps between two goes. Each:

| field | meaning |
|---|---|
| `name` | the talent's (or amulet's) name in your language |
| `icon` | its icon's path (`Interface\Icons\...`), for a texture |
| `kind` | `summon` (Pact of the Nathrezim's or Butcher's next demon, Legion Gate's next wave) or `cooldown` (a proc's, a heal's or a second life's wait between two goes) |
| `duration` | the whole wait, in seconds, with your cooldown reduction |
| `remaining` | seconds left, counted down off the run's clock; 0 when it may go now |
| `ready` | `true` when `remaining` is 0 |

With `name`, only that talent's: `AZS_GetTimers("Butcher")`.

### `AZS_GetRevive()` → `revive`
A party's revives: `{ down, share, by, reviving, rshare }`. `down` is 1 while you are down,
`share` how far (0 to 1) a friend has brought you back and `by` that friend's name; `reviving` is
the downed friend you stand over and `rshare` how far it is. Empty out of a party. To draw your
own revive bar, hide the game's with `AZS_SetHudShown("revive", false)`.

### `AZS_GetLairs()` → `lairs`
The guardians' lairs: `AZS_LAIRS` (below), each entry with `active` set: `true` while a guardian
holds the lair (asleep or fighting), `false` while it is empty.

### `AZS_GetFlaggedLair()` → `lair` or `nil`
The lair the minimap's red flag points at (the nearest held lair, else the one that fills
soonest), as `AZS_GetLairs()` gives it. It answers even with the flag hidden
(`AZS_SetHudShown("flag", false)`), so an addon can draw its own. `nil` before the first lair.

### `AZS_GetPlayerPosition()` → `position, nil` or `nil, err`
Your hero's position as the client renders it this frame (the pose the minimap centres on), on
every map, dungeons and raids included, where `GetPlayerMapPosition("player")` gives `0, 0`. Call
it from `OnUpdate`: two calls in one frame give the same `sampleId`, the next frame a new one.
It never answers from the run's packets (`AZS_RUN.px` changes only as the client reports its movement, about twice a second while running).

| field | meaning |
|---|---|
| `apiVersion` | 1 |
| `mapId` | the map you are on; 0 is a map like any other |
| `spaceId` | the coordinate space: the map and the run (`m<map>:<runId>`, `m<map>:none` outside a run). Every `AZS_CHESTS` and `AZS_LAIRS` entry carries one: use an entry's `x`, `y` only when its `spaceId` is the position's |
| `sampleId` | this frame's sample |
| `x`, `y`, `z` | world coordinates in yards: +x north, +y west, +z up, the same as `AZS_CHESTS`' and `AZS_LAIRS`'; 0 is a real position |
| `facing` | radians in `[0, 2π)`, 0 north, growing counterclockwise toward west |
| `runId` | the run's id, absent outside a run |

Errors (see [the meter API](#errors)): `NO_WORLD` outside the world, `NO_PLAYER` before your
hero is placed, `NOT_READY` while a teleport or loading screen settles (never a position from
the old place), `INVALID_ARGUMENT` for any argument.

```lua
local p = AZS_GetPlayerPosition();
if ( p ) then
	for i = 1, table.getn(AZS_CHESTS) do
		local c = AZS_CHESTS[i];
		if ( c.spaceId == p.spaceId ) then
			local dx, dy = c.x - p.x, c.y - p.y; -- +x is north, +y is west
		end
	end
end
```

### `AZS_GetRunSummary()` → `summary` or `nil`
The run's end, as the end screen shows it, once you fell, left or won: the `AZS_END` table
(below). `nil` while the run goes on, endless after a win too.

### `AZS_GetStoredLevels()` → `n`
The level-ups stored by **Store Level-Ups** (Options → Controls), waiting to be opened; 0 with
the setting off.

### `AZS_OpenStoredLevels()`
Opens the stored level-ups, as the HUD's Level Up! button does: each is offered in turn until
none are left. Nothing happens while another choice is open or the run is paused. The **Open
Level-Ups** key binding (Key Bindings → Azeroth Survivors, `Y` by default, a gamepad's Y) does
the same.

### `AZS_IsStoringLevels()` → `boolean` / `AZS_SetStoringLevels(on)`
Whether Store Level-Ups is on (off by default), and turning it on or off. It is the Controls
page's setting (the `storeLevels` CVar), so it is saved. Turned off, the levels stored so far
are offered at once.

### `AZS_RegisterBannerHandler(handler)` / `AZS_UnregisterBannerHandler(handler)`
Calls `handler(banner)` for every banner from then on, the moment it comes, with `banner` as in
`AZS_BANNERS` (below). It is called with the game's banners hidden too
(`AZS_SetHudShown("banners", false)`), so an addon can draw its own in their place. A handler
registered twice is still called once; an error in one goes to the error handler and the others
still run. Unregistering stops the calls.

### `AZS_GetSummons([which])` → `list`
Your summons out right now, for every class and spec: a Hunter's beast, a Druid's Treants, a
Shaman's Spirit Wolves, a Mage's Water Elemental and all of a Warlock's demons (the Imp and
companion, Legion Gate's lesser demons, the Felguard, Dread Lord, Infernal and Doomguard). In the
order they came, each:

| field | meaning |
|---|---|
| `id` | its own number, unique among your summons in the run |
| `entry` | its creature entry |
| `key` | what it is, its English name in lower case, never translated: `wolf`, `raptor`, `white_devilsaur`, `treant`, `spirit_wolf`, `water_elemental`, `imp`, `felhunter`, `voidwalker`, `succubus`, `felguard`, `infernal`, `doomguard`, `dread_lord` |
| `kind` | `pet` for a beast or elemental, else the demon's kind: `imp`, `felhunter`, `voidwalker`, `succubus`, `felguard`, `infernal`, `doomguard`, `pitlord` |
| `name` | its name in your language |
| `icon` | its icon's path (`Interface\Icons\...`), for a texture |
| `permanent` | `true` for one that stays for good (nil otherwise) |
| `duration` | how long a temporary one stays, in seconds (nil for a permanent one) |
| `remaining` | seconds left before it goes, counted down off the run's clock (nil for a permanent one) |

With `which`, only the summons whose `key` or `kind` it is: `AZS_GetSummons("infernal")`.
Your companion (the look on the Rewards page that only follows you) is not a summon.

### `AZS_GetSummonCount([which])` → `n`
How many summons are out, all of them or those of `which` as above.

### `AZS_GetSummonGroups()` → `groups, total`
Your summons by creature, in the order the first of each came: `{ key, kind, name, icon,
count, permanent, temporary, remaining }` each, where `permanent` and `temporary` say how many of
`count` stay for good or go, and `remaining` is the time left of the one that goes first (nil
if none goes). `total` counts them all.

### `AZS_GetTotems([key])` → `list`
Your totems standing, one entry per kind in the order you first dropped them: `{ key, spell, name,
icon, count, cap, duration, remaining }`, as [`AZS_TOTEMS`](#azs_totems-your-totems) describes
them, where `duration` is how long one stands and `remaining` the seconds until the soonest of
that kind goes. With `key`, only that kind: `AZS_GetTotems("searing_totem")`.

### `AZS_GetTotemCount([key])` → `n`
How many totems stand, all of them or those of `key`.

### `AZS_UnitKind(unit)` → `kind` or `nil`
What an enemy unit (`"target"`, `"mouseover"`, …) is to the run: `final`, `boss`, `guardian`,
`elite` or `normal`. `nil` for players, pets and units that don't exist.

1.12 has no unit GUIDs, and a guardian is the same creature as the ordinary elites of its type.
So `guardian` is reported only for your **target**, which the server knows; on other units a
guardian reads as `elite`.

### `AZS_SetHudShown(part, shown)` / `AZS_IsHudShown(part)`
Hide or show a part of the game's own HUD: see [Hiding the game's HUD](#hiding-the-games-hud).

### `AZS_SetOnTopOfScreens(frame, on)` / `AZS_IsOnTopOfScreens(frame)`
Keeps `frame` (a frame or its global name) and every frame under it over the level-up and jewelry
cards while they are up: drawn over them and taking its own clicks there, which the cards under it
never see. `on` false puts it back under. `AZS_IsOnTopOfScreens` says whether a frame is kept on
top.

A frame with no global name raises an error. The setting lasts until the interface loads again,
which is each run, so call it where you make your frames. A tooltip your frame shows is the stock
`GameTooltip`, which stays under the cards unless you keep it on top too:

```lua
local bar = CreateFrame("Frame", "MyCooldownBar", UIParent);
AZS_SetOnTopOfScreens(bar, true);
AZS_SetOnTopOfScreens("GameTooltip", true); -- its tooltips too
```

### `AZS_GetLocalTime()` → `clock` or `nil`
The device's local time, read when you call it, exactly as the clock under the minimap shows it.
It works with that clock or the minimap hidden, out of a run, and while the game is paused or you
choose a card. Each call returns a new table:

| field | type | meaning |
|---|---|---|
| `text` | string | the clock's own text: `10:42 PM` in English, `22:42` in every other language |
| `hour` | number | 0 to 23, whatever the format |
| `minute` | number | 0 to 59 |
| `is24Hour` | boolean | `false` in English, `true` in every other language |

The format follows the language chosen in the game, not the device's. `nil` means the time can't
be read now, and it is also the answer in the first frame after a `/reload`, so read again rather
than giving up. For a minute display, reading once a second is plenty.

Older versions don't have this function: check that `AZS_GetLocalTime` exists before using the
`localClock` HUD part, because `AZS_SetHudShown` and `AZS_IsHudShown` exist there without it.

```lua
if ( AZS_GetLocalTime ) then
	local clock = AZS_GetLocalTime();
	if ( clock ) then
		MyClockText:SetText(clock.text);
		AZS_SetHudShown("localClock", false); -- only once yours has a time to show
	end
end
```

---

## Tables

All are globals. Numbers arrive as Lua numbers and flags as `0` or `1`. Each table has a
matching `*_UPDATED` global (`AZS_RUN_UPDATED`, `AZS_UNITS_UPDATED`, …): the `GetTime()` of its
last change.

### `AZS_ZONE`: the run's fixed facts
Sent once at the start of a run.

| field | meaning |
|---|---|
| `zone` | zone key (see [Reference values](#reference-values)) |
| `zname` | zone name |
| `zsub` | the zone's subtitle (its threat, e.g. "The Defias Uprising") |
| `diff` | the zone's difficulty multiplier |
| `len` | the second the final boss comes (= `AZS_GetRunLength()`) |
| `bt` | number of bosses in the zone |
| `class` | class key (`mage`, `warrior`, …) |
| `cname` | class name |
| `race`, `rname` | race (English, then in the player's language) |
| `hero` | the hero's name |
| `gender` | 0 male, 1 female |
| `map` | the map id |
| `heroic` | the Heroic level, 1 to 5; 0 for a normal run |
| `run` | the run's id (the meter API's `runId`) |
| `affixes` | the run's Heroic affixes, `,` between them, empty for none (Heroic III and IV have one, V two): `bolstering`, `sanguine`, `explosive`. Always these English keys, whatever the game's language |

### `AZS_RUN`: the run right now

| field | meaning |
|---|---|
| `t` | run clock, seconds (prefer `AZS_GetRunTime()`) |
| `ph` | phase (prefer `AZS_GetPhase()`) |
| `lvl`, `xp`, `xpn` | level, experience, experience needed for the next level |
| `hp`, `hpm`, `sh` | health, maximum health, absorb shield |
| `gold` | gold |
| `kills` | enemies killed |
| `en` | enemies alive |
| `el` | ordinary elites alive (not bosses or guardians) |
| `bu` | bosses alive |
| `gu` | guardians holding lairs (alive) |
| `ga` | guardians awake |
| `lairs` | lairs on the map |
| `bs` | bosses that have spawned so far |
| `bt` | bosses in the zone |
| `nb` | seconds to the next boss, also in endless mode; `-1` for none |
| `nbn` | the next boss's name (empty in endless) |
| `fin` | the second the final boss comes |
| `left` | seconds until the final boss; `-1` in endless |
| `endl` | 1 in endless mode |
| `won` | 1 once the final boss died |
| `lives` | extra lives left |
| `rr`, `rrc` | rerolls left, gold for the next bought one |
| `chest` | gold the next cache costs |
| `dealt` | damage dealt this run |
| `taken` | damage taken this run |
| `ge` | gold earned this run |
| `bk`, `gk`, `ek` | bosses, guardians and elites killed |
| `ck` | caches opened |
| `owed` | choices waiting (level-ups and caches not yet picked) |
| `sl` | level-ups stored (= `AZS_GetStoredLevels()`) |
| `proc` | the class proc with a meter you hold: `bloodrage`, `seal_fate`, `soul_fire` or `lightwell`; empty for none |
| `pv`, `pm` | that meter and its full: rage 0 to 100, combo points or soul shards 0 to 5, the Lightwell's turn 0 to 1 (a standing well's time left, else its cooldown come). Bloodrage, Seal Fate and Soulfire go off at full |
| `px`, `py`, `pz` | the hero's world position, in yards: the same space as `x`, `y`, `z` in `AZS_CHESTS` and `AZS_LAIRS`, on every map, instances included (where `GetPlayerMapPosition` gives 0, 0); as the server last heard it from the client's movement, about twice a second while running: for every frame use `AZS_GetPlayerPosition()` |
| `pf` | the hero's facing, radians from 0 to 2π: 0 faces +`x`, π/2 faces +`y` (as the client's orientation) |

### `AZS_SCHEDULE`: the zone's bosses, in order
A list of `{ index, at, name, final, state }`:

- `at` is the run second the boss comes.
- `final` is 1 for the final boss.
- `state` is `due` (still to come), `up` (alive now) or `dead`.

### `AZS_UNITS`: special enemies alive
A list of `{ kind, id, name, hp, hpm, state }`: final boss, then bosses, guardians and elites,
at most 40.

- `kind` is `final`, `boss`, `guardian` or `elite`.
- `state` is 1 for an enraged boss or an awake guardian.

`AZS_GetBosses()` and `AZS_GetGuardians()` are filtered views of this list.

### `AZS_DEBUFFS`: debuffs on bosses and guardians
A list of `{ id, spell, stacks, duration, ends, permanent, dispel, icon, name }`, the raw form of
`AZS_GetDebuffs()`: `id` the unit's spawn id, `ends` the run second it ends, `duration` and
`ends` -1 when unknown, `permanent` 1 for a mark, `dispel` empty for none.

### `AZS_TARGET`: your target

| field | meaning |
|---|---|
| `kind` | `none`, `player` (yourself), `other` (not an enemy), `dead`, or an enemy's kind: `final`, `boss`, `guardian`, `elite`, `normal` |
| `id` | spawn id (enemies, also `dead`) |
| `name`, `hp`, `hpm` | name and health |
| `state` | 1 for an enraged boss or an awake guardian |
| `elite` | 1 for any elite creature (elites, guardians and bosses) |

### `AZS_SPELLS`: your spells
A list of `{ key, spell, rank, cd, cdmax }`, your racial last:

- `key` is the game's ability key (stable, not localized) and `spell` its spell id (the real
  1.12 spell the game casts).
- `cd` is seconds of cooldown left and `cdmax` the full cooldown.

Spells cast themselves; a passive "proc" ability's `cdmax` is its fastest interval.

### `AZS_TIMERS`: your talents' clocks
A list of `{ name, icon, kind, ready, wait }`: `ready` is the run second it is ready again,
`wait` the whole wait. Use `AZS_GetTimers()` for the time left.

### `AZS_BUFFS`: your buffs
A list of `{ spell, left }`: spell id and seconds left.

### `AZS_PETS`: your summons
A list of `{ id, entry, key, kind, ends, dur, icon, name }`, as `AZS_GetSummons()` describes
them; `ends` is the run second a temporary one goes and `dur` how long it stays, both -1 for a
permanent one. Use `AZS_GetSummons()` for the time left.

### `AZS_TOTEMS`: your totems
A list of the totems you have standing, one entry per kind in the order you first dropped them:

| field | meaning |
|---|---|
| `key` | the totem's spell key (`searing_totem`, `magma_totem`, …) |
| `spell` | its spell id |
| `count` | how many of that kind stand now |
| `cap` | the most of that kind that may stand; dropping one more replaces the oldest |
| `ends` | the run second the soonest of them goes |
| `dur` | how long one stands |
| `icon`, `name` | its icon path and its name, in the player's language |

`AZS_GetTotems([key])` returns the same with `duration` and `remaining` (seconds until the soonest
goes); `AZS_GetTotemCount([key])` is how many stand, of that kind or of all.

### `AZS_METER`: the damage meter
The lines `AZS_GetMeter()` returns.

### `AZS_HEALING`, `AZS_BOSSMETER`: the healing and boss meters
The lines `AZS_GetHealing()` and `AZS_GetBossMeter()` return.

### `AZS_PARTY`: a party's meter
The lines `AZS_GetPartyMeter()` returns.

### `AZS_REVIVE`: a party's revives
What `AZS_GetRevive()` returns.

### `AZS_CHESTS`: the caches on the field
Every cache waiting to be opened (the ones at the start, the ones elites drop and the rest), in
the order they appeared. What the next one costs is `AZS_RUN.chest`.

| field | type | meaning |
|---|---|---|
| `id` | number | unique in the run |
| `x`, `y`, `z` | number | world position, in yards; yours live is `AZS_GetPlayerPosition()` |
| `mx`, `my` | number | position on the zone's map, 0 to 1, as `GetPlayerMapPosition` gives yours; -1 if the map data is missing |
| `runId`, `mapId`, `spaceId` | string, number, string | the run, map and coordinate space it is in, as `AZS_GetPlayerPosition()`'s: project it only when all three match yours. A new run or map clears the old entries before new ones come |

### `AZS_LAIRS`: the guardians' lairs
One entry per lair, in a fixed order:

| field | type | meaning |
|---|---|---|
| `index` | number | 1, 2, … |
| `state` | string | `asleep` (a guardian waits in it), `awake` (its guardian is fighting) or `empty` (slain, or none has come yet) |
| `name` | string | the guardian's name, or the one that will come |
| `id` | number | the guardian's id, as in `AZS_UNITS`; 0 when empty |
| `left` | number | seconds until an empty lair fills; 0 once due, -1 while held |
| `x`, `y`, `z`, `mx`, `my` | number | where the lair is, as in `AZS_CHESTS` |
| `flag` | number | 1 on the lair the minimap's red flag points at (`AZS_GetFlaggedLair()`) |
| `runId`, `mapId`, `spaceId` | string, number, string | as in `AZS_CHESTS` |

A guardian fights where it is, not at its lair, and walks back home if you lead it too far.

### `AZS_TAKEN`: damage you took, by source
A list of `{ source, dmg }`, the most first. `source` is one of `boss melee`, `boss spell`,
`elite`, `mob melee`, `mob spell`, `area` (ground effects).

### `AZS_END`: the run's end
Empty while the run goes on; filled once when you fall, leave or win (prefer
`AZS_GetRunSummary()`), and emptied when the next run starts or the run goes on endless.

| field | meaning |
|---|---|
| `result` | `victory`, `defeat` or `left` |
| `t` | seconds survived |
| `lvl` | the level reached |
| `kills`, `elites`, `guardians`, `bosses` | enemies, elites, guardians and bosses slain |
| `dmg` | damage dealt (overkill left out, as the meter does) |
| `taken` | damage taken |
| `gold` | gold earned |
| `chests` | caches opened |
| `magnets` | magnets taken |
| `endl` | 1 when the run ended in endless mode |
| `hero`, `class`, `cname`, `race`, `rname`, `zone`, `zname` | as in `AZS_ZONE` |

### `AZS_BANNERS`: the run's banners
The last 20 banners (the messages in the middle of the screen), the newest last; `AZS_BANNER`
is the last one (nil before the first). Each:

| field | type | meaning |
|---|---|---|
| `id` | number | unique while the game runs, counting up across runs |
| `kind` | string | `boss` (a boss approaches), `finalboss` (the final boss has come), `bossdown`, `guardian` (slain), `achievement`, `unlocked` (a win opened a zone), `magnet` (one appeared, or was taken), `cache` (how to buy one, or the gold it needs), `rune` (taken), or in a party `downed` (you fell), `frienddown` (a friend fell) or `revived` (you stand again) |
| `boss` | boolean | true for the ones the game draws large and red |
| `name` | string | what it is about: the boss's, guardian's, achievement's, zone's or rune's name, or the friend's for `frienddown`; empty for a magnet, a cache, `downed` or `revived` |
| `text` | string | the line the banner shows, in the player's language |
| `time` | number | the `GetTime()` it came |

To hear each one as it comes, use `AZS_RegisterBannerHandler()`.

### `AZS_STATS`: your stats
The numbers the character pane (C) shows: `hp` (max health), `dmg` (damage multiplier), `cd`
(cooldown multiplier), `crit`, `cm` (crit multiplier), `arm` (damage reduction), `reg` (health
per second), `area`, `cnt` (extra targets), `echo`, `spd`, `pick` (pickup range), `xp`, `luck`,
`gold`, `heal` (heal per kill), `lch` (leech), `thn` (thorns), `liv` (lives), and more. Keys
starting with `g` (`ghp`, `gdmg`, …) are what worn gear adds.

---

## The meter API (version 1)

`AZS_GetMeter()` and its siblings give the meter window's lines by name; they stay as they are.
The meter API gives the same counters complete, by stable keys: every source of damage or
healing, who dealt it (you, each of your summons and totems, each hero in a party), how (a hit or
a tick, reflected or not, its school), what triggered it, the boss fight it was in, with the
game's own names, icons and portraits in the game's language. It covers every credit the game's
meters count, including those of summons that are gone and items you no longer wear, and it
holds for the whole run through `/reload`.

It follows the addon author's "Version 1 implementation contract" exactly: the eight functions
below, their tables and their errors.

```lua
local caps = AZS_GetMeterCapabilities();
local context = AZS_GetMeterContext();
if ( context ) then
	local damage, err = AZS_GetMeterSnapshot({ apiVersion = 1, runId = context.runId,
		metric = "damage", scope = "self", period = "run", groupBy = "source", windowSeconds = 10 });
	if ( damage ) then
		for i = 1, table.getn(damage.rows) do
			local row = damage.rows[i];
			local src = damage.sources[row.sourceKey];
			-- src.name, src.icon.path, row.displayAmount, row.share, row.displayRate.value
		end
	elseif ( err.retryable ) then
		-- try again after err.retryAfterSeconds
	end
end
```

### Calling rules

Every function returns exactly two values: `result, nil` on success, `nil, err` on failure. None
throws for a bad argument or a missing state, none changes the game's counters, its meter
window or another addon's statistics, and every table returned is yours: changing it changes
nothing else. Requests are plain tables (no metatable), read raw, never changed. Unknown keys,
extra arguments, wrong types and numeric strings (`"10"` is not `10`) are errors; a NaN or an
infinity is never an integer. IDs, revisions and tokens are opaque strings: compare them, never
parse them.

| function | arguments | result |
|---|---|---|
| `AZS_GetMeterCapabilities()` | none | Capabilities (any time, outside a world too) |
| `AZS_GetMeterContext()` | none | Context |
| `AZS_GetPlayerPosition()` | none | PlayerPosition ([above](#azs_getplayerposition--position-nil-or-nil-err)) |
| `AZS_GetMeterSnapshot(query)` | a MeterQuery | MeterSnapshot |
| `AZS_GetMeterLoadout(query)` | a LoadoutQuery | MeterLoadout |
| `AZS_CreateMeterCheckpoint(runId)` | the current run's id | Checkpoint |
| `AZS_ReleaseMeterCheckpoint(checkpointId)` | a token from the above | `true` |
| `AZS_SetMeterPortrait(texture, runId, portraitKey)` | a Texture object, the run's id, a portrait key | `true` |

### Errors

`err` has `code`, an English `message` (do not parse it), `retryable`, `retryAfterSeconds` (real
seconds, when retryable), `argument` (the field at fault, as `query.windowSeconds`, for argument
and version errors) and `reason` (for an invalidated checkpoint).

| code | retry | when |
|---|---|---|
| `INVALID_ARGUMENT` | no | wrong arity, type, enum or range; an unknown key; a forbidden combination |
| `UNSUPPORTED_VERSION` | no | `apiVersion` is an integer other than 1 |
| `UNSUPPORTED_QUERY` | no | never in this game: every feature is supplied |
| `NO_WORLD` | 1 s | no world loaded |
| `NO_PLAYER` | 0.2 s | the world, but no hero yet |
| `NO_RUN` | 1 s | no run retained |
| `RUN_MISMATCH` | no | `runId` is not the current run: read the context again |
| `NOT_READY` | 0.2 s | the ledger is still arriving (after a `/reload`, a missed update, a party's first read) |
| `NO_PARTY` | 1 s | `scope = "party"` in a run alone |
| `NO_ENCOUNTER` | 0.2 s | `period = "encounter"` before the run's first boss fight |
| `UNKNOWN_OWNER` | no | not a hero of this run |
| `OWNER_NOT_PRESENT` | 1 s | that hero left: its history stays, its loadout does not |
| `INVALID_CHECKPOINT` | no | a token this game did not issue (or another game process's) |
| `CHECKPOINT_EXPIRED` | no | released, or its UI session (`/reload`) or world ended |
| `CHECKPOINT_INVALIDATED` | no | `reason` `run_changed`, `authority_changed` or `counter_epoch_changed` |
| `RESOURCE_LIMIT` | no | 64 checkpoints active: release one |
| `UNKNOWN_PORTRAIT` | no | not a portrait key of this run |
| `ART_NOT_READY` | 0.2 s | the portrait is being prepared |
| `ART_UNAVAILABLE` | no | that actor has no portrait: use your own art |
| `INTERNAL_ERROR` | 1 s | the game failed; nothing was written |

Checks come in this order: arguments (fields in the order the tables below list them, then
unknown keys, bytewise), version, features, world and player, run, owner, party and encounter,
checkpoint, readiness.

### Capabilities

`apiVersion` 1, `apiMinor` 0, `uiSessionId` (new at every `/reload`), `languageCode` and
`languageRevision` (the game's language, known before any run), `metrics` `{ "damage",
"healing" }`, `healingPerspectives` `{ "received", "performed" }`, `scopes` `{ "self", "party",
"owner" }`, `periods` `{ "run", "encounter" }`, `groupings` `{ "source", "owner", "actor_source",
"detail" }`, `minWindowSeconds` 5, `maxWindowSeconds` 120, `historyResolutionSeconds` 0.2,
`historyRetentionSeconds` 120, `maxCheckpoints` 64, `languageCodes` (`en`, `pl`, `cs`, `de`,
`pt`, `es`, `fr`, `ro`, `tr`, `ru`, `uk`, `zh`, `ja`, `ko`) and `features`, every one `true`:
`liveInstancePosition`, `completeSourceBreakdowns`, `partySourceBreakdowns`, `partyHealing`,
`performedHealing`, `nativeSourceIcons`, `currentLoadouts`, `partyLoadouts`,
`retainedPortraits`, `atomicCheckpoints`, `nativeRecentWindows`, `localizedMetadata`.

### Context

Every snapshot and loadout carries its own `context`; use that one, not one read earlier.

| field | meaning |
|---|---|
| `apiVersion`, `uiSessionId` | 1; this UI's session |
| `runId` | one per run, kept through pauses, language changes and `/reload` |
| `authorityId` | the accounting's owner: the host's in a party |
| `counterEpoch` | changes if the counters' clock ever goes back (it invalidates checkpoints and recent history) |
| `snapshotId` | this capture, clock and names included |
| `counterRevision`, `metadataRevision` | change when a counter, or a name, icon or loadout, does |
| `languageCode`, `languageRevision` | the game's language |
| `phase` | `ready`, `playing`, `choosing`, `paused`, `dead` or `victory` (a party's run as a whole) |
| `asOfRunSeconds` | the run clock the counters are at; it only moves while `playing` |
| `localOwnerId` | your hero's actor id |
| `partyState`, `partyId` | `solo`, `active` or `ended` (every friend left: their history stays); the party's id when not solo |
| `encounter` | the current boss fight, else the last: `encounterId`, `state` (`active` or `completed`), `startRunSeconds`, `endRunSeconds` (completed), `bossActorIds`, `startFenceId`, `endFenceId` (completed). Only bosses open a fight; guardians and elites never count |

### MeterQuery

| field | type | meaning |
|---|---|---|
| `apiVersion` | integer | 1 |
| `runId` | string | the context's `runId` |
| `metric` | `"damage"` or `"healing"` | |
| `healingPerspective` | `"received"` (default) or `"performed"` | healing only: by the hero healed, or by the hero whose spell (or pet) it was |
| `scope` | `"self"`, `"party"` or `"owner"` | you (with your pets and totems), every hero of the party (those who left too), or one hero |
| `ownerId` | string | for `scope = "owner"` only: a hero's actor id |
| `period` | `"run"` or `"encounter"` | the whole run, or the current or last boss fight (damage to its bosses only; healing while it was on) |
| `groupBy` | `"source"`, `"owner"`, `"actor_source"` or `"detail"` | the rows |
| `windowSeconds` | integer | 0 (All), or 5 to 120: the recent rate |
| `checkpointId` | string | optional: amounts since your checkpoint |

### MeterSnapshot

`apiVersion`, `status` (`ready`, or `ready_empty` when there is no row), `complete` (`true`),
`context`, `query` (yours, normalized: healing gets its `healingPerspective`), `ownerIds` (the
heroes selected, in the order they joined), `amountBasis` (`period`, or `checkpoint` with one),
`period` (`kind`, `startRunSeconds`, `endRunSeconds`, `elapsedSeconds`, and the `encounter`),
`checkpoint` (with one), `totals`, `contributions`, `rows`, `maxRowAmount`, and `sources` and
`actors`: the descriptor of every key the result names, by key.

**Stats** (`totals`, every contribution and row): `periodAmount`; `sinceCheckpointAmount` (with a
checkpoint); `windowAmount` (with a window, while its history is there); `displayAmount` (the
since-checkpoint amount, else the period's); `periodAverage`; `checkpointAverage` (with a
checkpoint); `recentRate`; `displayRate` (the recent rate with a window, else the checkpoint's
average, else the period's).

**Rate**: `status` `ready` (`amount`, `startRunSeconds`, `endRunSeconds`, `elapsedSeconds`,
`denominatorSeconds`, `value`, `resolutionSeconds`, `approximate`), `warming_up` (the same less
`value`, under a second in), `unavailable` (the times and `reason` `history_gap`,
`history_not_retained` or `authority_changed`) or `not_requested` (no window). Averages divide by
at least a second; a recent rate by its elapsed seconds. A recent window starts at the latest of
the period's start, now minus `windowSeconds`, and your checkpoint; it counts credits from its
start on, exactly (`resolutionSeconds` 0) when the start falls on a time the game recorded
credits at, else interpolated between two such times at most 0.2 s apart (`approximate`).

**Contributions** are the ledger's leaves, each credited once, ordered by `tieOrder` (the game's
first-credit order): the Stats and `rowId`, `ownerId` (the hero it counts for), `actorId` (who
dealt or healed), `sourceKey`, `sourceOwnerState` (`known`, `unowned`: the game's own, as a
level-up's heal; `unknown`) and `sourceOwnerId`, for healing `recipientActorId`,
`recipientOwnerState` and `recipientOwnerId`, `nativeProc` (the meter window's blue proc bar),
`periodicity` (`periodic` or `non_periodic`), `reflection` (`reflected` or `not_reflected`),
`schoolState` (`known`, `unknown`, `not_applicable`) and `schoolMask` (physical 1, holy 2, fire 4,
nature 8, frost 16, shadow 32, arcane 64), `creditKind` (`health_damage`, `health_restored` or
`absorb_consumed`: Soul Link's soaked damage), `originSourceKeys` (the talent or item that cast
it), `tieOrder` and `share`.

**Rows** group them: `detail` one a contribution (with all its fields), `actor_source` by
(hero, actor, source, proc), `source` by (hero, source, proc), `owner` one a hero. Each row: the
Stats, `rowId`, `ownerId`, `memberIds` (its contributions), `tieOrder`, `rank`, `share` (of the
totals), `procClassification` (`proc`, `non_proc`, or `mixed` for a hero's row), and `sourceKey`,
`actorId`, `nativeProc` where its grouping has them. Only rows with a `displayAmount` above 0
appear, the largest first. Rows, contributions and totals are the same credit at three levels:
never add them together.

**Amounts.** Damage is health taken off enemies after every multiplier, overkill left out, the
moment it lands. Healing is health gained, overhealing left out, plus Soul Link's consumed
absorbs. In the `received` view a heal counts for the hero healed; in `performed`, for the hero
whose spell or pet it was (a heal by the game itself, as a level-up's, is no hero's). The two
views are the same heals seen twice: never add them. Amounts are the game's own numbers,
unrounded.

### Checkpoints

`AZS_CreateMeterCheckpoint(runId)` takes your own reset point over every scope at once (you,
the party, healing, the boss fight): `checkpointId`, `runId`, `uiSessionId`, `authorityId`,
`counterEpoch`, `atRunSeconds`, `fenceId`. Pass its `checkpointId` in a query to read amounts
since it: a credit admitted after it counts even at the same run second, a fight begun after it
counts whole, one ended before it counts nothing, and a source or friend that first appears after
it counts from its first credit. It clears nothing. It lasts until you release it, the UI
reloads or the world ends (`CHECKPOINT_EXPIRED`), or the run, authority or counter epoch changes
(`CHECKPOINT_INVALIDATED`); 64 at once at most, never evicted. In a party, the first checkpoint
waits (`NOT_READY`, 0.2 s) for the party's ledgers. Release with
`AZS_ReleaseMeterCheckpoint(id)`: `true` every time for a token the game issued, released or
expired or not. Keep tokens for the session; never save them.

### MeterLoadout

`AZS_GetMeterLoadout({ apiVersion = 1, runId = …, ownerId = … })` (`ownerId` optional, you by
default): `apiVersion`, `complete`, `context`, `ownerId`, `loadoutRevision`, `sources`, `actors`,
and four lists, all by the same keys as the meter's sources:

| list | entries |
|---|---|
| `equipment` | 20 slots, index `slotId + 1`: 0 ammo (`supported` false: the run has none), 1 head … 19 tabard: `slotId`, `supported`, `occupied`, `effectSourceKeys`, and `itemInstanceKey`, `sourceKey` when occupied |
| `abilities` | your abilities in their slots, the racial, and spells a talent grants: `sourceKey`, `learned`, `selected`, `passive`, `rank`, `slotId`, `effectSourceKeys` (the ability and its pulse variant) |
| `talents` | the talents learned: `sourceKey`, `rank`, `treeKey`, `effectSourceKeys`, `modifiedSourceKeys` |
| `runJewelry` | the run's rings, trinkets and amulet: `itemInstanceKey`, `sourceKey`, `category`, `effectSourceKeys` |

### Descriptors

**Source**: `sourceKey`; `kind` (`ability`, `effect`, `talent`, `item`, `basic_attack`, `system`,
`generic`); `attribution` (`known`, or `unknown` for `talents` and `other`, credits whose origin
the game did not record); `name` and `nameLanguageCode`; `icon` (`{ status = "ready", path }`, or
`{ status = "missing", reason }`); `spellIds`; `originSourceKeys`; `relatedSourceKeys`; and where
they exist `localizationKey`, `abilityKey`, `talentKey`, `itemKey`, `itemId`, `itemInstanceKey`,
`variantKey` (`pulse`, `empowered`), `rank`. Keys: `ability:<key>:r<rank>[:pulse]`,
`talent:<class>.<key>:r<rank>`, `amulet:<key>:r<rank>`, `jewel:<key>:r<rank>`, `item:<entry>`,
`set:<class>.<rarity>[.empowered]`, `demon:<attack>`, `spell:<id>`, and `melee`, `regen`, `leech`,
`heal_on_kill`, `level_up`, `soul_link`, `restoration`, `soulstone`, `thorns`, `talents`,
`other`. A new rank is a new key: `abilityKey` and `talentKey` join a source's ranks.

**Actor**: `actorId` (`H…` a hero, `P…` a summon, `T…` a totem, `D…:<summon>` a summon whose very
one is not known, `E…` a boss, `S:<key>` the game itself); `kind` (`hero`, `pet`, `summon`,
`totem`, `enemy`, `system`); `name` and `nameLanguageCode` (`und` for a hero's own name); `state`
(`present`, `departed`, `despawned`); `portrait` (`portraitKey` and `status`, `reason` when
missing); `classKey` for a hero; and where they exist `parentActorId`, `currentOwnerId`,
`creatureEntry`, `petFamilyKey`, `summonKey`, `totemKey`. Summons and totems stay after they are
gone.

### Portraits

`AZS_SetMeterPortrait(texture, runId, portraitKey)` sets one of your Textures (a real one: a
frame, a font string or a look-alike table is `INVALID_ARGUMENT`) to an actor's portrait: the
client's own 2D portraits (a hero's race and sex, a summon's, a boss's), kept for the whole run,
departed heroes' too. It keeps the texture's anchors, size, layer, alpha, tint and texture
coordinates; your mask and rim are yours. On any error the texture is left as it was.

```lua
local party = AZS_GetMeterSnapshot({ apiVersion = 1, runId = context.runId, metric = "damage",
	scope = "party", period = "run", groupBy = "owner", windowSeconds = 0 });
if ( party and party.rows[1] ) then
	local portrait = party.actors[party.rows[1].ownerId].portrait;
	if ( portrait.status == "ready" ) then
		AZS_SetMeterPortrait(MyPortrait, party.context.runId, portrait.portraitKey);
	end
end
```

### The four tabs

Damage Done: `metric = "damage", scope = "self", period = "run", groupBy = "source"` with a
`windowSeconds` for its recent rate; Healing Done: `metric = "healing"`, `windowSeconds = 0`;
Boss Damage: `period = "encounter"`, `windowSeconds = 0`; Party Damage: `scope = "party",
groupBy = "owner"`, `windowSeconds = 0`. A party hero's own breakdown: `scope = "owner"` with its
`ownerId` and `groupBy = "actor_source"` or `"detail"`. Each call is its own capture: check each
one's error.

**After a `/reload`**: the run, its counters, history and descriptors are back within a moment
(`NOT_READY` until then, with the game asking the server); checkpoints are gone
(`CHECKPOINT_EXPIRED`), as the contract says. Polling is enough: the API sends no events.

---

## World-anchored combat text (version 1)

For an addon that draws its own floating combat text. It gives every outgoing hit exactly as the
game knows it (no chat parsing, no guessing the target by name) and a place in the world for that
hit, kept after the enemy's nameplate and body are gone. Numbers stay over the spot as the camera
moves, including over where an enemy died, and hide when that spot leaves the view.

All calls return `result, nil` or `nil, err`, with `err = { code, message }`, and never raise.
Tables are yours; ids and cursors are strings to compare for equality only and never to save.

| `code` | meaning |
|---|---|
| `INVALID_ARGUMENT` | a bad argument |
| `NOT_IN_WORLD` | not in the world; capabilities still answer |
| `NOT_READY` | the first frame after a `/reload`: try again |
| `STREAM_CHANGED` | your cursor belongs to a stream that ended (a new world, a new run, a `/reload`): clear your labels and read with `nil` |

### `AZS_GetCombatTextCapabilities()`
`apiVersion` 1; `impactAnchors`, `followAnchors`, `frameProjection`, `distanceMetrics`, `frameHandler`
all `true`; `maxEventsPerRead` and `maxAnchorsPerRead` 256; `eventCapacity` and `anchorCapacity` 4096;
`eventRetentionSeconds` and `anchorRetentionSeconds` 5. It works outside a run. Older versions don't
have this function: check that it exists.

### `AZS_GetCombatTextEvents(cursor, limit)`
`AZS_GetCombatTextEvents(nil)` subscribes at the current tail and returns no history; pass the
`cursor` it gives to read what came after. Reads consume nothing, so every addon keeps its own
cursor. `limit` is 1 to 256, 256 if left out. The game keeps hits only while someone reads: stop for
5 seconds and what passed is counted in `droppedCount`, not kept.

| batch field | meaning |
|---|---|
| `apiVersion` | 1 |
| `streamId` | this stream; a new world, a new run or a `/reload` starts another |
| `worldId` | the coordinate space and its epoch |
| `clockSeconds` | `GetTime()` |
| `cursor` | where to read from next; always advance to it |
| `events` | the hits in order; equal hits stay separate |
| `hasMore` | more hits wait: read again |
| `droppedCount` | hits lost before these, usually 0 |

| event field | meaning |
|---|---|
| `eventId` | this hit |
| `presentedAt` | `GetTime()` when it landed. Check its age: don't show a delayed batch's old hits |
| `targetId`, `targetName` | this one life of the target (a revive is a new one), and its name for display. `targetId` is absent if the game never held the target |
| `sourceId`, `sourceKind` | the attacker: `player`, `pet`, `summon`, `totem` or `ownedUnknown` |
| `kind` | `damage` or `miss` |
| `amount` | `damage` only: the number the game would float |
| `result` | `miss` only: `{ key, text }`, with `key` one of `miss`, `dodge`, `parry`, `block`, `resist`, `immune`, `evade`, `absorb`, `reflect`, `deflect`, and `text` the game's own word (absent if the language has none) |
| `critical`, `periodic`, `reflected` | booleans; `reflected` is a damage shield's hit or a reflected spell's word |
| `damageType` | `melee`, `ranged` or `spell`; absent if unknown |
| `schoolMask` | Physical 1, Holy 2, Fire 4, Nature 8, Frost 16, Shadow 32, Arcane 64; absent if unknown |
| `spellId`, `abilityName`, `icon` | the spell, when there is one; a melee swing or a shield has none |
| `anchorId`, `anchorStatus` | `ready` with its handle, or `unavailable` and no `anchorId`: never a place the game does not know |

It covers your hits and your pets', summons' and totems' on others, not hits on you, and not
healing. It works with `CombatDamage` off, the pet text options off, nameplates hidden and past the
game's own limit of 4 numbers per enemy, so an addon can turn the game's numbers off and still get
them. Reading changes no setting. Use the meter API for totals, not this.

### `AZS_ProjectCombatTextAnchors(anchorIds, mode)`
Puts up to 256 anchors through the camera. `mode` applies to the whole call:

- `"follow"` tracks that exact enemy while it lives. **At its death the anchor stays where it died**,
  whatever the corpse then does, for the rest of the anchor's life. An enemy that disappears with no
  death seen keeps the last place it was, with `targetState = "unavailable"`.
- `"impact"` stays where the hit landed.

Anchors last 5 seconds from the hit, and reading them doesn't extend that.

The answer has `apiVersion`, `worldId`, `streamId`, `cameraSampleId` (the same for one frame, new the
next, even with a still camera), `clockSeconds`, `frameCurrent` (see below) and `entries`, one per
handle, in order:

| entry field | meaning |
|---|---|
| `anchorId` | the handle you asked for |
| `state` | `onScreen`, `offscreen`, `behindCamera`, `clipped` (in front, past the near or far plane), `notReady` (no camera now, or a loading screen), `expired`, `unknownAnchor` or `worldChanged` |
| `targetState` | `alive`, `dead` or `unavailable`; absent for `expired`, `unknownAnchor` and `worldChanged` |
| `x`, `y` | `onScreen` only: offsets from `WorldFrame`'s bottom left, for `SetPoint("CENTER", WorldFrame, "BOTTOMLEFT", x, y)` |
| `cameraDistance`, `cameraDepth` | `onScreen` only: yards from the camera, straight and along its view |
| `uiUnitsPerYard` | `onScreen` only: what one yard measures at that depth, for sizing text by distance |
| `reason` | `expired` only: `age`, or `resourceLimit` if over 4096 anchors pushed it out early |

Anything but `onScreen` has no coordinates: hide the label and keep counting down its lifetime, don't
restart it. This is the camera's view only; a wall in front of the spot doesn't hide it.

### `AZS_RegisterCombatTextFrameHandler(handler)` / `AZS_UnregisterCombatTextFrameHandler(handler)`
**Move your labels here, not in `OnUpdate`.** `OnUpdate` runs before the camera moves for the frame, so
what it reads is the last frame's view and your numbers trail by a frame when you turn. This handler
runs once every frame in the world, after the camera moved and before anything is drawn, so a
`SetPoint` here draws in step with the world, as nameplates do. `frameCurrent` says which you got: `true`
in the handler, `false` from anywhere else. Errors go to the error handler and leave other addons' handlers
running.

```lua
local labels = {}; -- yours: each with anchorId, born, holder
local cursor;

if ( AZS_GetCombatTextCapabilities ) then
	AZS_RegisterCombatTextFrameHandler(function()
		local batch, err = AZS_GetCombatTextEvents(cursor);
		if ( err and err.code == "STREAM_CHANGED" ) then
			MyAddon_ClearLabels(labels);
			batch = AZS_GetCombatTextEvents(nil);
		end
		if ( not batch ) then
			return;
		end
		cursor = batch.cursor;
		for i = 1, table.getn(batch.events) do
			MyAddon_AddLabel(labels, batch.events[i]); -- if it has an anchorId
		end

		local ids = {};
		for i = 1, table.getn(labels) do
			ids[i] = labels[i].anchorId;
		end
		local projection = AZS_ProjectCombatTextAnchors(ids, "follow");
		if ( not projection ) then
			return;
		end
		for i = 1, table.getn(projection.entries) do
			local point, label = projection.entries[i], labels[i];
			if ( point.state == "onScreen" ) then
				label.holder:SetPoint("CENTER", WorldFrame, "BOTTOMLEFT", point.x, point.y + MyAddon_Rise(label));
				label.holder:Show();
			else
				label.holder:Hide();
			end
		end
	end);
end
```

With more than 256 labels, split them into several calls; every call in a frame shares one camera.

---

## Hiding the game's HUD

Each part of the game's own HUD can be turned off. An addon can then draw its own version in
its place:

| part | what it hides | CVar |
|---|---|---|
| `clock` | the run clock and the "next boss" line | `hudClock` |
| `bosses` | boss and guardian health bars | `hudBosses` |
| `tally` | the kills and gold box | `hudTally` |
| `build` | the spells and jewelry rows | `hudBuild` |
| `banners` | messages in the middle of the screen | `hudBanners` |
| `loot` | the boss loot notice | `hudLoot` |
| `meter` | the damage meter window and its minimap button | `hudMeter` |
| `levels` | the stored level-ups' Level Up! button | `hudLevels` |
| `flag` | the minimap's red flag on the nearest guardian's lair (`AZS_GetFlaggedLair()` still says where it would be) | `hudFlag` |
| `revive` | a party's revive bar and the "you are down" line under the clock (`AZS_GetRevive()` still says it all) | `hudRevive` |
| `localClock` | the local time under the minimap; the minimap, the zone name and the run clock stay (`AZS_GetLocalTime()` still reads it) | `hudLocalClock` |

```lua
AZS_SetHudShown("meter", false);   -- hide one part
AZS_SetHudShown("all", false);     -- hide every part (nil works too)
AZS_SetHudShown("clock", true);    -- show it again
if ( AZS_IsHudShown("bosses") ) then ... end
```

These are the same settings as the "Show …" checkboxes in Options → Interface (`levels`,
`flag` and `revive` have no checkbox there). They are saved, and players can switch them back there. The stock `SetCVar("hudMeter", "0")` /
`GetCVar("hudMeter")` work as well (`"1"` shown, `"0"` hidden).

`AZS_IsHudShown` says what the part is set to, not whether it's on screen: `localClock` set to show
still hides with the minimap, and setting it never brings a hidden minimap back.

Good practice: hide only the parts your addon replaces, and only while it's enabled.

---

## Raw addon messages

The tables come from `CHAT_MSG_ADDON` messages the game's server whispers to the client. An
addon that wants to react the moment something changes can listen itself:

```lua
local f = CreateFrame("Frame");
f:RegisterEvent("CHAT_MSG_ADDON");
f:SetScript("OnEvent", function()
	if ( arg1 == "AZSUNITS" ) then
		-- AZS_UNITS already holds the new list; arg2 is the raw line
	end
end);
```

The game's own parser registers before any addon, so the tables are normally updated by the
time your handler runs.

| prefix (`arg1`) | table | sent |
|---|---|---|
| `AZSZONE` | `AZS_ZONE` | once a run |
| `AZSRUN` | `AZS_RUN` | on change (every 0.2 s while playing) |
| `AZSBOSSES` | `AZS_SCHEDULE` | on change |
| `AZSUNITS` | `AZS_UNITS` | on change |
| `AZSDEBUFFS` | `AZS_DEBUFFS` | on change (a debuff comes, goes or is refreshed) |
| `AZSTGT` | `AZS_TARGET` | on change |
| `AZSSPELLS` | `AZS_SPELLS` | on change |
| `AZSTIMERS` | `AZS_TIMERS` | on change (a talent's clock starts again) |
| `AZSREVIVE` | `AZS_REVIVE` | on change (in a party) |
| `AZSBUFFS` | `AZS_BUFFS` | on change |
| `AZSPETS` | `AZS_PETS` | on change (a summon comes or goes) |
| `AZSTOTEMS` | `AZS_TOTEMS` | on change (a totem goes down or one goes) |
| `AZSMETER` | `AZS_METER` | on change |
| `AZSHEAL` | `AZS_HEALING` | on change |
| `AZSBOSSDMG` | `AZS_BOSSMETER` | on change |
| `AZSPARTY` | `AZS_PARTY` | on change (in a party) |
| `AZSCHESTS` | `AZS_CHESTS` | on change |
| `AZSLAIRS` | `AZS_LAIRS` | on change (every second while a lair waits to fill) |
| `AZSTAKEN` | `AZS_TAKEN` | on change |
| `AZS` | `AZS_STATS` | on change |
| `AZSEND` | `AZS_END` | once when the run ends; empty when the next starts or the run goes on endless |
| `AZSBANNER` | `AZS_BANNERS`, `AZS_BANNER` | one line per banner, as it comes (two alike arrive as two) |
| `AZSM` | the meter API's ledger, in numbered frames | on change (every 0.2 s while playing) |

`arg2` formats:

- **`AZSZONE`, `AZSRUN`, `AZSTGT`, `AZSREVIVE`, `AZS`:** `key=value` pairs separated by `;`, e.g.
  `t=250.50;ph=playing;lvl=9;…`.
- **The list prefixes:** entries separated by `;`, fields by `,`, in the order the tables above
  list them, e.g. `AZSUNITS`: `final,812,Edwin VanCleef,52000,52500,0;guardian,90,Kobold Geomancer,300,900,1`.
  An empty `arg2` means an empty list.
- **`AZSEND`:** `key=value` pairs, as `AZSRUN`.
- **`AZSBANNER`:** `id=…;kind=…;boss=0|1;name=…;text=…`, the text last and whole: it may hold
  `;`, `,` and `=`.

Names never contain `;`, `,` or `=` (a banner's text may). Updates come at most five times a second, and only when
something changed.

`AZSM` is not meant to be parsed by hand: it is a stream of numbered frames, each what changed
since the one before, that the game applies whole and in order (asking the server again,
`AZSMREQ`, after a `/reload` or a missed frame). Read it through the functions in
[The meter API](#the-meter-api-version-1).

---

## Reference values

**Zones** (`AZS_ZONE.zone`), in the order they unlock:

| key | zone |
|---|---|
| `elwynn` | Elwynn Forest |
| `duskwood` | Duskwood |
| `wsg` | Warsong Gulch |
| `ungoro` | Un'Goro Crater |
| `ab` | Arathi Basin |
| `feralas` | Feralas |
| `arena` | Gurubashi Arena |
| `sm` | Scarlet Monastery |
| `av` | Alterac Valley |
| `brm` | Blackrock Mountain |
| `aq` | Gates of Ahn'Qiraj |
| `naxx` | Naxxramas |

**Classes** (`AZS_ZONE.class`): `warrior`, `paladin`, `hunter`, `rogue`, `priest`, `shaman`,
`mage`, `warlock`, `druid`.

**Phases:** `ready`, `playing`, `choosing`, `paused`, `dead`, `victory`.

**Unit kinds:** `final`, `boss`, `guardian`, `elite`, `normal`.

---

## Limits and tips

- **Guardians on non-target units:** only your target can be told apart as a guardian;
  `AZS_GetGuardians()` lists them all by id and name.
- **No GUIDs:** spawn `id`s are unique within a run but there is no 1.12 API to map a unit token
  to one. Match by name (and target) instead; boss names are unique within a zone. For your own
  hits, [`AZS_GetCombatTextEvents`](#azs_getcombattexteventscursor-limit) names the exact target
  and an anchor to draw at, with no name matching.
- **Names are localized:** they come in the player's language, the same text `UnitName` returns.
- **Zone, class and phase keys are not localized,** so use those for logic.
- **Run time:** use `AZS_GetRunTime()` rather than `AZS_RUN.t` for a smooth clock.
- **Lua:** write for 1.12 conventions: `this`, `event`, `arg1`…, `table.getn`, `math.mod`,
  `string.find` (there's no `string.gmatch` in 1.12 addons).
- **Out of a run:** the tables may keep the last run's values, so check `AZS_GetPhase()`.
- **Read-only:** the API only reads the run. Nothing an addon does changes the run, except the
  stored level-ups (turning Store Level-Ups on or off, opening them). Hiding the HUD and keeping
  frames over the cards change only what is on screen.
- **Numbers that follow the camera:** move them in
  [`AZS_RegisterCombatTextFrameHandler`](#azs_registercombattextframehandlerhandler--azs_unregistercombattextframehandlerhandler),
  not `OnUpdate`, which runs before the camera moves for the frame.

---

## Examples

### Boss timer bar

```lua
local bar = CreateFrame("Frame", "MyBossTimer", UIParent);
bar:SetWidth(200); bar:SetHeight(20);
bar:SetPoint("TOP", UIParent, "TOP", 0, -120);
local text = bar:CreateFontString(nil, "OVERLAY", "GameFontNormal");
text:SetPoint("CENTER", bar, "CENTER");

bar:SetScript("OnUpdate", function()
	local next = AZS_GetUpcomingBosses()[1];
	if ( AZS_GetPhase() == nil or not next ) then
		text:SetText("");
		return;
	end
	local left = math.max(0, next.left);
	text:SetText(string.format("%s in %d:%02d", next.name, math.floor(left / 60), math.mod(left, 60)));
end);
```

### Every boss alive, with health

```lua
local bosses = AZS_GetBosses();
for i = 1, table.getn(bosses) do
	local b = bosses[i];
	local tag = b.final and " (final)" or "";
	if ( b.enraged ) then tag = tag .. " ENRAGED"; end
	DEFAULT_CHAT_FRAME:AddMessage(string.format("#%d %s%s: %d/%d (%.0f%%)",
		b.id, b.name, tag, b.hp, b.hpm, 100 * b.hp / b.hpm));
end
```

### Replacing the damage meter

```lua
AZS_SetHudShown("meter", false);

local f = CreateFrame("Frame");
f:SetScript("OnUpdate", function()
	local lines, total, dps = AZS_GetMeter();
	-- draw `lines` your way: lines[i].name, .dmg, .dps, .share, .proc
end);
```

### Your summons and their time left

```lua
local f = CreateFrame("Frame");
f:SetScript("OnUpdate", function()
	local groups, total = AZS_GetSummonGroups();
	for i = 1, table.getn(groups) do
		local g = groups[i];
		local left = g.remaining and string.format(" (next goes in %.0fs)", g.remaining) or "";
		-- e.g. "Infernal x2 (next goes in 12s)", "Imp x1"
		local line = string.format("%s x%d%s", g.name, g.count, left);
	end
end);
```

### Your own banners

```lua
AZS_SetHudShown("banners", false);
AZS_RegisterBannerHandler(function(b)
	local r, g, bl = 1, 0.82, 0;
	if ( b.boss ) then r, g, bl = 1, 0.1, 0.1; end
	UIErrorsFrame:AddMessage(b.text, r, g, bl);
end);
```

### Open stored level-ups when no boss is up

```lua
AZS_SetStoringLevels(true);
local f = CreateFrame("Frame");
f:SetScript("OnUpdate", function()
	if ( AZS_GetPhase() == "playing" and AZS_GetStoredLevels() > 0
			and table.getn(AZS_GetBosses()) == 0 ) then
		AZS_OpenStoredLevels();
	end
end);
```

### Warn when the target is a guardian

```lua
local f = CreateFrame("Frame");
f:RegisterEvent("CHAT_MSG_ADDON");
f:SetScript("OnEvent", function()
	if ( arg1 == "AZSTGT" and AZS_TARGET.kind == "guardian" ) then
		UIErrorsFrame:AddMessage("Guardian: " .. AZS_TARGET.name, 1, 0.5, 0);
	end
end);
```

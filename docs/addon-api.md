# Azeroth Survivors: Addon API

> This is the addon API of Azeroth Survivors as **Aethus** published it on **27 October 2026 at
> 12:21**. The game may have changed since. Ask questions and look for a newer version in the
> [official addon channel on Discord](https://discord.com/channels/1557821819032051852/1564599899469914203).

Azeroth Survivors runs inside the World of Warcraft 1.12.1 client, so addons are ordinary 1.12
addons (`## Interface: 11200`, Lua, XML, the stock frame API). The game also lets addons read
the run's state: the clock, the phase, the bosses (alive and still to come), guardians and
elites, the hero, spells, buffs, summons, the meters, the banners and the run's end. It also
lets them hide the game's own HUD so they can draw their own.

The data covers the current run only. Out of a run, the tables are empty or still hold the
previous run's values, so check `AZS_GetPhase()` first.

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

### `AZS_GetTimers([name])` → `list`
Your talents' (and jewelry's) own clocks: the countdowns the build shows (Pact of the
Annihilan's and Butcher's next demon) and every wait a talent keeps between two goes. Each:

| field | meaning |
|---|---|
| `name` | the talent's (or amulet's) name in your language |
| `icon` | its icon's path (`Interface\Icons\...`), for a texture |
| `kind` | `summon` (Pact of the Annihilan's or Butcher's next demon, Legion Gate's next wave) or `cooldown` (a proc's, a heal's or a second life's wait between two goes) |
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
companion, Legion Gate's lesser demons, the Felguard, Pit Lord, Infernal and Doomguard). In the
order they came, each:

| field | meaning |
|---|---|
| `id` | its own number, unique among your summons in the run |
| `entry` | its creature entry |
| `key` | what it is, its English name in lower case, never translated: `wolf`, `raptor`, `treant`, `spirit_wolf`, `water_elemental`, `imp`, `felhunter`, `voidwalker`, `succubus`, `felguard`, `infernal`, `doomguard`, `pit_lord` |
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

### `AZS_UnitKind(unit)` → `kind` or `nil`
What an enemy unit (`"target"`, `"mouseover"`, …) is to the run: `final`, `boss`, `guardian`,
`elite` or `normal`. `nil` for players, pets and units that don't exist.

1.12 has no unit GUIDs, and a guardian is the same creature as the ordinary elites of its type.
So `guardian` is reported only for your **target**, which the server knows; on other units a
guardian reads as `elite`.

### `AZS_SetHudShown(part, shown)` / `AZS_IsHudShown(part)`
Hide or show a part of the game's own HUD: see [Hiding the game's HUD](#hiding-the-games-hud).

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
| `mods` | the run modifiers in percent, space-separated: enemy density, health, damage, speed (`"100 100 100 100"` is a normal run) |

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
| `px`, `py`, `pz` | the hero's world position, in yards: the same space as `x`, `y`, `z` in `AZS_CHESTS` and `AZS_LAIRS`, on every map, instances included (where `GetPlayerMapPosition` gives 0, 0) |
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

### `AZS_METER`: the damage meter
The lines `AZS_GetMeter()` returns.

### `AZS_HEALING`, `AZS_BOSSMETER`: the healing and boss meters
The lines `AZS_GetHealing()` and `AZS_GetBossMeter()` return.

### `AZS_CHESTS`: the caches on the field
Every cache waiting to be opened (the ones at the start, the ones elites drop and the rest), in
the order they appeared. What the next one costs is `AZS_RUN.chest`.

| field | type | meaning |
|---|---|---|
| `id` | number | unique in the run |
| `x`, `y`, `z` | number | world position |
| `mx`, `my` | number | position on the zone's map, 0 to 1, as `GetPlayerMapPosition` gives yours; -1 if the map data is missing |

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
| `kind` | string | `boss` (a boss approaches), `finalboss` (the final boss has come), `bossdown`, `guardian` (slain), `achievement`, `unlocked` (a win opened a zone), `magnet` (one appeared, or was taken), `cache` (how to buy one, or the gold it needs) or `rune` (taken) |
| `boss` | boolean | true for the ones the game draws large and red |
| `name` | string | what it is about: the boss's, guardian's, achievement's, zone's or rune's name; empty for a magnet or a cache |
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

```lua
AZS_SetHudShown("meter", false);   -- hide one part
AZS_SetHudShown("all", false);     -- hide every part (nil works too)
AZS_SetHudShown("clock", true);    -- show it again
if ( AZS_IsHudShown("bosses") ) then ... end
```

These are the same settings as the "Show …" checkboxes in Options → Interface (`levels`,
`flag` and `revive` have no checkbox there). They are saved, and players can switch them back there. The stock `SetCVar("hudMeter", "0")` /
`GetCVar("hudMeter")` work as well (`"1"` shown, `"0"` hidden).

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
| `AZSMETER` | `AZS_METER` | on change |
| `AZSHEAL` | `AZS_HEALING` | on change |
| `AZSBOSSDMG` | `AZS_BOSSMETER` | on change |
| `AZSCHESTS` | `AZS_CHESTS` | on change |
| `AZSLAIRS` | `AZS_LAIRS` | on change (every second while a lair waits to fill) |
| `AZSTAKEN` | `AZS_TAKEN` | on change |
| `AZS` | `AZS_STATS` | on change |
| `AZSEND` | `AZS_END` | once when the run ends; empty when the next starts or the run goes on endless |
| `AZSBANNER` | `AZS_BANNERS`, `AZS_BANNER` | one line per banner, as it comes (two alike arrive as two) |

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
  to one. Match by name (and target) instead; boss names are unique within a zone.
- **Names are localized:** they come in the player's language, the same text `UnitName` returns.
- **Zone, class and phase keys are not localized,** so use those for logic.
- **Run time:** use `AZS_GetRunTime()` rather than `AZS_RUN.t` for a smooth clock.
- **Lua:** write for 1.12 conventions: `this`, `event`, `arg1`…, `table.getn`, `math.mod`,
  `string.find` (there's no `string.gmatch` in 1.12 addons).
- **Out of a run:** the tables may keep the last run's values, so check `AZS_GetPhase()`.
- **Read-only:** the API only reads the run. Nothing an addon does changes the run, except
  hiding the HUD and the stored level-ups (turning Store Level-Ups on or off, opening them).

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

# Cafe-sim: drink-making minigames — technical plan

Status (2026-11-11): **phases 1–4 are built; phase 5 (parallel steps) is not started.** Every drink
has a recipe and the player's timer fallback is gone. None of phase 4 has been played yet. Scope:
`plugins/cafe-sim` only — no engine-api change, no ABI bump. Storage/stock comes later and plugs in
through the same quality number (§9).

## 0. Where we are

`cargo test -p cafe-sim`: 57 tests. The sections after this one are the original plan; this one
records what was built and where it departs from them.

| Piece | Where |
|---|---|
| Data shape: `Drink { steps, difficulty }`, `Step { label, hint, game, weight }`, `drink(name)`, `Drink::quality`; shared steps (`TAMP`, `PULL_SHOT`, `STEAM`, `scoop()`, `pour()`) | `drinks.rs` |
| Minigame core: `Minigame`/`MinigameState`/`MinigameInput`/`Outcome`, `band_score`, `shifted`, `MeterLayout`/`MeterShape` | `minigames/mod.rs` |
| A Pressure, B Scoop, C Pour, D Stir, E Steep, F Froth, G Trace, each with scripted-input tests | `minigames/*.rs` |
| `BrewSession`, `ActiveMenu::Brew`, `MachineInUse`, overlay (meters, result flash, step dots, Done / Throw away) | `drink_station.rs`, `menus.rs` |
| Drink card → `BrewSession::start` → `Brewing::finished(quality, perfect)`; staff still use `Brewing::timed` | `coffee_menu.rs`, `staff_agent.rs` |
| `payout()` for both the player's delivery and staff `serve`; perfect = "+$N  PERFECT" popup in gold + a gold drink cube (machine, hand, counter); ordinary cubes are cream | `drinks.rs`, `delivery.rs`, `balance.rs` |
| Staff `skill` (rolled at hire, in the wage, "Skill" line), `brew_quality` = centre ± `SPREAD` | `staff.rs` |
| `SAVE_VERSION` 3, no legacy reader (old cafe saves need recreating) | `saves.rs` |

Recipes (step weights in brackets):

| Drink | Steps |
|---|---|
| Espresso | Tamp (0.6) → Pull the shot (0.4) |
| Coffee | Scoop the grounds (0.5) → Pour the water (0.5) |
| Tea | Scoop the leaves (0.3) → Pour the water (0.3) → Steep (0.4) |
| Hot Chocolate | Scoop the cocoa (0.2) → Pour the milk (0.2) → Steam (0.3) → Whisk (0.3) |
| Latte | Tamp (0.6) → Pull the shot (0.4) → Steam (0.6) → Pour a heart (0.8) |
| Cappuccino | Tamp (0.6) → Pull the shot (0.4) → Steam, stiff foam (0.6) → Dust the cocoa, spiral (0.6) |

If a drink shows a timer, the engine is loading an old dylib: `cargo run` does not build plugins —
run `cargo build-plugins` (the host hot-reloads on mtime).

**Departures from the plan as written**, all deliberate:

- **ASCII only on screen.** bevy's default font (`FiraMono-subset.ttf`) has 96 codepoints, nothing
  past `~`, so `★`, `✕`, `●○` and `—` never rendered. The perfect marker is the word PERFECT in gold,
  the step dots are round UI nodes, and `steps_are_well_formed` asserts every label and hint is ASCII.
  Shipping a fuller font is the alternative if we want symbols.
- `MinigameInput` is `held`/`pressed`/`cursor`/`confirm`/`dt`. `released` was dropped (games end on
  `!held`, which also survives a missed release event). `cursor` is measured over one meter of the
  game's choosing (`MeterLayout::cursor`, bevy's `RelativeCursorPosition`), 0..1 with y up.
  `confirm` is the step's Done button or Enter, offered only by games that return a
  `MinigameState::confirm_label` (Scoop).
- **Targets move.** Every band has a `jitter`: `MinigameState::new` takes an RNG and shifts the band
  per attempt, so a step can't be done from muscle memory. The game state owns its shifted band, so
  meters come from the *state* (`meters` once per step, `readings` per frame), not the params.
- **Meters are `MeterShape::{Bar, Dish, Canvas}`**, not a play area per game: a bar with a needle and
  optional fill; a round dish (Stir's cursor surface, nothing moves); a square with a dotted path and
  a marker at the reading's fraction along it (Trace). Each game is one to three meters side by side.
- **Scoop was redesigned after play** (the first version's band was narrower than a scoop, so landing
  in it was luck, and its levelling drag was unclear). Now: hold to fill the scoop, let go to tip it
  in, press Done when the cup looks right. The scoop has its **own bar beside the cup's** at its own
  scale: a full scoop is `capacity` of a cup, rolled ± 30 % per attempt, so the scoop bar never says how
  far it will lift the cup. The first scoop shows the ratio; the partial last one is the judgement.
  Nothing comes back out, so over the band falls off twice as fast as under it. No levelling.
- **Pour is one pour**: after release the stream decays with `settle_secs` (adds ≈ speed × settle);
  the step ends when it stops. Re-pressing isn't allowed, or tapping would trivialise it.
- **Froth keeps a foam target**: "hold the band until the foam fills" gives the step an end and a
  reason to pulse the heat. Warm-up isn't waste, only time out of the band after first reaching it:
  `foam × (0.5 + 0.5 × useful/(useful+wasted))`; scalding multiplies by 0.3 and ends the step.
- **Stir measures unsigned angle swept** around the dish centre (dead zone in the middle, nothing
  outside the rim, ≤ π/2 per frame so cursor jumps aren't stirring). Slosh grows with the *square* of
  the speed over `safe_speed` and decays below it; a spill caps the score at 0.3, drops slosh to 0.5,
  and the stir carries on.
- **Trace**: press on the gold start dot, then hold and follow the dots. Progress only moves forward,
  to path within `REACH` (0.12 canvas widths) ahead, within twice the tolerance — so cutting across
  the shape skips nothing. Letting go or running out of time (`max_secs` from the first press) ends it
  with what's drawn: `fraction drawn × (1 − mean distance / tolerance)`.
- `Step.hint` is data on the step, not derived from the game. Pressure, Pour and Froth wait for a
  *fresh* press, so the click that picked the drink card doesn't start them.
- `TIP_FROM = 0.5`: quality where a tip starts (was `UNSCORED_QUALITY`, the timer drinks' fixed
  quality; renamed once those went). An average drink pays exactly its price.
- `BrewSession::start` returns `false` for a drink with no steps (and logs it), so the Brew menu
  can't open empty.

**Tuning pass 1 (2026-11-13, from play):**

- Trace draws what you actually drew: a trail of dots graded green/yellow/red by distance to the
  path, plus a draining "milk" bar and a status line (accuracy so far, seconds left). Limits cut
  to 5 s (heart, 2.5 canvas widths) and 5.5 s (spiral, 2.7).
- Scoop has a budget: the full scoops this attempt's band needs plus `spare_scoops` (1), shown as
  "Scoops left: N"; the last scoop ends the step. Spamming tiny scoops no longer works.
- Froth's band drifts once you first reach it (two sines, random direction, `drift` 0.08,
  `drift_period_secs` 3.5), so tapping to park the needle doesn't work. Band lowered and jitter
  cut so jitter + drift stays under the scald line (the data test enforces it).
- Stir is scored on **speed**: a speed meter with a green band (`good_from` × `safe_speed` up to
  `safe_speed`), score = share of the stirring spent in the green, slosh/spill above it as
  before; status line shows the live share. The clock starts at the first sweep. Timing out
  scales the score by how far it mixed.
- Overlay plumbing for these: `MeterReading { value, zone }` (moving good zone on a bar),
  `MinigameState::status` (live line under the meters), `MinigameState::trail` (canvas dots).

**Not played yet:** Froth, Stir, the new Scoop, Trace, the Latte and Cappuccino recipes, the
perfect presentation. **Known gaps:** a customer's `unhappy` flag only picks the angry face; the
tuning numbers are all first guesses.

Remaining: a tuning pass on everything once played → phase 5 (parallel steps).

## 1. Where we start from

- `drinks.rs`: `Drink { name, brew_secs, price }`, six entries; drinks are identified by
  `&'static str` name everywhere (`Customer.order`, `Brewing.drink`, `CarriedDrink.drink`).
- `coffee_menu.rs`: clicking a `CoffeeMachine` opens `ActiveMenu::Coffee` (a card carousel);
  picking a card inserts `Brewing { drink, total_secs, elapsed }` and `tick_brewing` counts it up.
- `delivery.rs`: a ready machine is clicked to fill `CarriedDrink`; clicking the waiting customer's
  counter pays `price_for(drink)` if the order matches.
- `staff_agent.rs`: baristas insert `Brewing` with `brew_secs × (1 + drink_time_mod)`, then `serve`
  pays `price + price × friendliness × TIP_FRACTION`.
- Any open `ActiveMenu` already frees the cursor (`cursor.rs`) and blocks placement/clicks, and
  pausing closes menus (`close_menus_when_paused`). A minigame overlay gets both for free by being
  a menu.
- There is no quality concept and no customer patience yet.

## 2. The flow after this change

```
click idle machine ─► drink carousel (unchanged) ─► pick drink
   ─► ActiveMenu::Brew: overlay runs the recipe's steps one after another
   ─► last step done ─► machine holds a finished drink with a quality (no extra wait)
   ─► pick up / deliver as today ─► payout scales with quality
```

- The player's `brew_secs` wait goes away: the minigames *are* the time cost. `brew_secs` stays as
  the staff brew time.
- Cancelling (the overlay's ✕ button, or Escape → pause, which closes menus) throws the drink away:
  the machine goes back to idle, nothing is recorded.
- The world keeps running while you play (customers walk, staff work). Pausing closes every
  gameplay menu (`close_menus_when_paused`), so it cancels the drink like ✕ does.
- Between steps there is a short result flash ("Good! 82%", ~0.6 s) so each step reads on its own.

## 3. Data shape (`drinks.rs`)

```rust
pub struct Drink {
    pub name: &'static str,
    pub price: i64,
    /// Staff brew time. The player's time is however long the steps take.
    pub brew_secs: f32,
    /// How hard the drink is for staff (0 = trivial). Lowers their quality (§7).
    pub difficulty: f32,
    pub steps: &'static [Step],
}

pub struct Step {
    pub label: &'static str,       // "Tamp", "Pull the shot"
    pub game: Minigame,            // which game + its tuning
    pub weight: f32,               // share of the drink's quality
}

pub enum Minigame {
    Pressure(PressureParams),      // A
    Scoop(ScoopParams),            // B
    Pour(PourParams),              // C
    Stir(StirParams),              // D
    Steep(SteepParams),            // E
    Froth(FrothParams),            // F
    Trace(TraceParams),            // G
}
```

All of it is `const`, so a recipe is a data edit. `drink(name) -> Option<&'static Drink>` replaces
`price_for`/`brew_secs_for` (one lookup instead of one helper per field).

Example, Espresso:

```rust
steps: &[
    Step { label: "Tamp", weight: 0.6, game: Minigame::Pressure(PressureParams {
        rise_per_sec: 0.9, accel: 0.6, band: (0.62, 0.74), cap: 0.92 }) },
    Step { label: "Pull the shot", weight: 0.4, game: Minigame::Steep(SteepParams {
        duration: 3.0, window: (0.55, 0.75) }) },
]
```

Numbers here are placeholders; tuning happens in play.

## 4. Minigame module (`src/minigames/`)

```
minigames/
  mod.rs       # MinigameState enum, MinigameInput, Outcome, dispatch, shared widgets
  pressure.rs  # A   scoop.rs  # B   pour.rs   # C   stir.rs  # D
  steep.rs     # E   froth.rs  # F   trace.rs  # G
```

Each game has a **pure core** (state + `update` + `score`), with no ECS or UI in it:

```rust
pub struct MinigameInput {
    pub held: bool, pub pressed: bool, pub released: bool,   // LMB (or Space)
    pub cursor: Option<Vec2>,      // in the overlay's play area, 0..1 both axes
    pub dt: f32,
}
pub enum Outcome { Running, Done { score: f32 } }   // score in 0..=1

impl MinigameState {
    pub fn new(game: &Minigame) -> Self;
    pub fn update(&mut self, game: &Minigame, input: &MinigameInput) -> Outcome;
}
```

Dispatch is a `match` over the enum. That fits `const` data, avoids trait objects, and makes the
set of games explicit. Keeping the core pure means each game's scoring can be tested
deterministically by feeding it scripted input (§10), and the UI is just a view of that state.

### Per game: state and score

| Game | State | Ends when | Score |
|---|---|---|---|
| **A Pressure** | needle `v`, holding speed | release, or `v ≥ cap` | `1 − |v − centre| / half_band` inside the band, falling to 0 over one band-width outside; 0 at the cap |
| **B Scoop** | grams (each click adds a scoop with a small random wobble), then a level-off drag | the level drag reaches the far edge | closeness to target × leveling evenness (drag speed variance) |
| **C Pour** | fill `f`, pour speed (ramps while held) | released and settled, or `f > 1` (spill) | closeness of `f` to the line; overshoot is penalised harder than undershoot |
| **D Stir** | mixed `m`, slosh `s`, elapsed | `m ≥ 1` | `time_score × (1 − s_peak)`; hitting `s = 1` is a spill: the score is capped at 0.3 and the stir carries on |
| **E Steep** | elapsed / duration | click, or the timer runs out | 1 inside the window, falling off linearly to 0 by the ends of the bar |
| **F Froth** | temp `t` (drifts up, hold raises it), foam `φ` | `φ ≥ target`, or a time limit | fraction of time in the band × foam accuracy; `t` at the top = scald, score × 0.3 |
| **G Trace** | path progress, accumulated deviation | progress reaches the end of the path | `1 − mean_deviation / tolerance`, clamped |

Angular speed for **D** comes from the cursor: we accumulate the signed angle swept around the cup
centre, and `slosh += k × speed²` with a slow decay, so steady stirring beats frantic stirring.

## 5. Session and UI (`src/drink_station.rs`)

```rust
#[derive(Resource, Default)]
pub struct BrewSession(Option<Session>);
struct Session {
    machine: Entity,
    drink: &'static Drink,
    step: usize,
    state: MinigameState,
    scores: Vec<f32>,
    flash: Option<f32>,          // result-flash timer between steps
}
```

- `handle_drink_click` starts a session and sets `ActiveMenu::Brew` (a new variant), instead of
  inserting `Brewing`. While the session is open, the machine is marked `Brewing`-occupied
  (`MachineInUse`) so staff don't claim it.
- `read_minigame_input` builds a `MinigameInput` from the mouse/keyboard and the play-area node's
  `ComputedNode` + `UiGlobalTransform` (cursor → 0..1). Clicks inside the overlay are swallowed
  with `clear_just_pressed`, like the other click consumers.
- `tick_brew_session` runs `update`; on `Done` it pushes the score, flashes the result and either
  advances to the next step or finishes (§6). It returns early when `GamePaused` is set.
- `sync_brew_overlay` rebuilds the play-area children when the step changes and updates node
  sizes/positions every frame otherwise. When the menu closes without finishing, the session is
  dropped and `MachineInUse` removed (`cleanup_brew_session_when_inactive`, the same pattern as
  `cleanup_shop_overview_when_inactive`).

**Shared widgets** keep the UI small: A, C, E and F are all a *meter*, meaning a bar with coloured
zones and a needle or fill (`spawn_meter(zones) / set_meter(value)`). B is a meter plus a scoop
counter. D is a cup circle plus two meters. G is a dotted path: bevy_ui has no lines, so the path
is drawn as ~40 small round nodes, with a cursor dot. So the overlay needs three widgets and no new
rendering.

The overlay is a centred panel styled with `UiTheme`, with a header "Espresso — step 1/2: Tamp",
a one-line instruction ("Hold to tamp, release in the green"), the play area and a ✕ button.

## 6. Quality, finished drinks and payout

- `quality = Σ wᵢ·sᵢ / Σ wᵢ`. `perfect = all sᵢ ≥ 0.9`.
- `Brewing` gains `quality: f32` and `perfect: bool`. A finished player drink is inserted as
  `Brewing { elapsed == total_secs, … }`, so it is immediately `ready()`, and pickup and the
  ready-cube code stay unchanged. `CarriedDrink` gets `quality` and `perfect` too, and pickup copies
  them across.
- **One payout function** in `drinks.rs`, used by both `handle_delivery_click` and staff `serve`:

  ```rust
  pub fn payout(price: i64, quality: f32, perfect: bool, friendliness: f32) -> Payout
  // base  = price            (quality ≥ REFUSE_BELOW = 0.3, else price / 2 + angry face)
  // tip   = price × TIP_FRACTION × (quality_tip(quality) + friendliness·FRIENDLY_TIP)
  //         + PERFECT_BONUS if perfect
  ```

  The player has no friendliness, so they pass `0` and earn the tip by playing well. Staff earn
  both, but their quality is lower (§7). `TIP_FRACTION` moves from `staff_agent.rs` into this
  function.
- The balance popup shows "+$7 ★" for a perfect drink. The ready cube and the carried cube use a
  gold material when the drink is perfect: the cheapest sparkle.

## 7. Staff quality: a range based on their stats

- `StaffMember` gains `skill: f32` (0..1, rolled at hire). Wage includes it: `quality` in
  `generate_candidate` averages four terms instead of three. `SAVE_VERSION` 2 → 3 with no legacy
  reader, so **old cafe saves need recreating** (format bumps are free).
- Per staff drink: `centre = skill × (1 − 0.35 × drink.difficulty)`, then
  `quality = clamp(centre + U(−SPREAD, +SPREAD), 0, 1)`, with `SPREAD = 0.1`. So a good barista
  lands in a small band and complex drinks pull it down. `perfect` stays player-only, which gives
  the player a reason to make the hard drinks themselves.
- `drink_time_mod` keeps scaling staff brew time. A small speed/quality trade-off is possible later
  (`centre −= 0.1 × max(0, −drink_time_mod)`), but it isn't in this plan.
- The hire menu's `stat_lines` shows a "Skill" line.

## 8. Phasing

Each phase ends with something you can play.

1. ✅ **Framework + Espresso.** `drinks.rs` data shape, `minigames/{mod, pressure, steep}`,
   `drink_station.rs`, meter widget, quality through to `payout`, staff `skill` + range,
   save v3. Drinks without steps yet keep the old timer path (`steps.is_empty()`), so nothing
   breaks mid-way.
2. ✅ **Tea + Coffee.** `cursor` on `MinigameInput`, Scoop (B) and Pour (C), target jitter.
3. ✅ **Hot Chocolate.** Froth (F) and Stir (D). Froth kept a foam target after all: "hold the band
   until the foam fills" gives the step a clear end and a reason to pulse the heat.
4. ✅ **Latte + Cappuccino.** Trace (G) with the heart and spiral paths, the perfect presentation
   (gold popup + gold cube), the timer fallback removed. The Scoop redesign landed alongside.
5. **Parallel steps.** `Step.parallel_with_next`: the overlay shows both games side by side (for
   example, the shot pull E running while you froth F). Input goes to the game under the cursor and
   steep keeps ticking unattended. Deferred because it complicates input routing and layout, and is
   only worth it once the single-step games feel right.

**Later, not in this plan:** customer preferences (a per-order offset to a step's target, shown in
the order popup), customer patience (time waited lowering the tip), quality stats.

## 9. Hook for storage

Ingredient freshness will become a **cap or multiplier on individual steps**: for example, stale
milk caps the Froth score, and old beans shift the Pressure band narrower. To support that,
`Session` will carry an optional `StepModifiers` per step, filled from stock when the session
starts. Nothing in this plan needs to change for it.

## 10. Testing

- Unit tests per game on the pure core: a scripted perfect input scores ≥ 0.95, a bad input scores
  ≤ 0.2, timeouts end with `Done`, the spill and scald paths cap the score, and the stir's slosh
  behaves monotonically with speed.
- `payout` table tests (refuse threshold, perfect bonus, staff vs player).
- Staff quality: over many samples it stays within `[centre − SPREAD, centre + SPREAD]`.
- Data sanity test: every `Drink` has ≥ 2 steps (once phase 4 lands), the weights are positive and
  every band lies inside 0..1.
- Feel is tested by playing: you run the game and I adjust the numbers.

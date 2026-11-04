# How the game is drawn: the short version

A plain-language tour of the graphics, as of November 2026. The detail,
with file and line references, is in [`GRAPHICS.md`](GRAPHICS.md);
lighting has its own deep dive in [`LIGHTING.md`](LIGHTING.md).

## The look in one sentence

Flat, banded "toon" shading from **pre-computed** light, with black
outlines drawn on top.

## The three big ideas

**1. Light is baked, not calculated live.** When you press Shift+B, the
engine works out how much light reaches every surface: sun, sky, placed
lamps, light bouncing off nearby surfaces, soft shadow in corners. It saves
the answer to files. While playing, the game just looks those numbers up.
That's why lighting is cheap and looks the same on every machine. It's also
why moving a lamp does nothing until you bake again.

- *Things that don't move* (terrain, placed props, the cafe shell) store
  their light in **lightmaps**, images wrapped over the surface.
- *Things that move* (the player, cafe furniture) can't use a lightmap,
  because their light changes as they move. Instead the bake also fills the
  space around the level with a 3D grid of light samples, one per metre:
  the **probe volume**. A moving object asks the grid "how much light is
  here, from which direction?" every frame. It also gets a soft dark disc
  underneath it, the **blob shadow**, since nothing else casts shadows at
  runtime.

**2. Light becomes bands.** The brightness from the bake doesn't go straight
to the screen. It goes through the **toon curve**, which snaps it into a few
flat levels (5 by default) and multiplies that by the object's colour. That
is the whole cartoon look. It's set per world in the inspector (click empty
space → Shading): how many bands, how dark the darkest is, the exposure,
and how the bands are spaced.

**3. Outlines come from the picture, not the models.** After the scene is
drawn, one pass looks at two hidden by-products of drawing: how far away
each pixel is, and which way its surface faces. Wherever neighbouring pixels
disagree sharply, it draws a line. It's tuned in the inspector → Edges, and
explained in [`EDGE_DETECTION.md`](EDGE_DETECTION.md).

## One frame, step by step

1. **Bring in the world.** Nearby terrain chunks and their saved light load
   in the background as you move. Far ones are dropped.
2. **Draw the scene** into an off-screen picture:
   - Terrain: a sandy base with a stone pattern scattered so it never
     visibly repeats, lit by its lightmap.
   - Props and characters, lit by lightmaps or probes.
   - A big sky sphere that follows the camera, coloured from the world's sun
     and sky settings.
   - Blob shadows and editor helpers drawn see-through on top.
3. **Finish the picture:**
   - Clip any negative colours (there's no film-style tone curve, because
     the toon curve already decided the brightness).
   - Draw the outlines.
   - Smooth jagged edges (FXAA).
4. **Show it.** In play mode the picture is a fixed 1920×1080 image
   stretched to the window. In the editor it's resized to fit the viewport
   panel and shown inside the editor UI.

## What's deliberately *not* there

- No live lights or live shadows.
- No bloom, glow, screen-space reflections or ambient occlusion passes.
- No WebGL. The web build needs a WebGPU browser.

## The editor's extra layers

- The transform arrows draw over everything.
- Lights are drawn as wireframe spheres and cones (F8 hides them).
- Collision shapes can be shown in green (Shift+P).
- The object you're about to place follows the cursor as a see-through
  ghost.
- The asset browser's thumbnails come from a tiny hidden photo studio far
  below the world.
- Shift+L switches baked light off, to check colours flat. F2 turns the
  outlines off.

## Native vs browser

Same picture, same shaders. The differences are about *when* work happens:
- In the browser, everything runs on one thread. Baking and loading pause
  the page in a way they don't on the desktop.
- The browser's shader compiler is stricter about one class of shader code.
  Some mistakes only show up as a black screen on the web.

## Odd things worth knowing

These were found while writing this up. Details are in `GRAPHICS.md` §17.

- **The "volumetric fog" setting does nothing.** The fog clouds it was for
  were removed long ago.
- **The distance fog doesn't fog the world.** It only affects editor helper
  objects, because our own shaders don't apply it. The haze on the horizon,
  if you see one, is the sky's colour, not fog. Neither fog setting costs
  anything while it's doing nothing; adding real distance fog would be
  nearly free, but Bevy's volumetric fog would need real-time sun shadows
  we deliberately don't have (`GRAPHICS.md` §9.1).
- **Shift+L can miss things that load while it's on.** They come in lit
  until you toggle again.
- **Far terrain almost never switches to its lower-detail versions.** The
  switch distance is further than where terrain stops loading.
- **The player's blob shadow stays on in first person**, even though the
  player model is hidden.

## Where to look next

| If you want to… | Read |
|-----------------|------|
| Change how light looks, or understand a bake | [`LIGHTING.md`](LIGHTING.md) |
| Tune outlines | [`EDGE_DETECTION.md`](EDGE_DETECTION.md) |
| Find which file draws what | [`GRAPHICS.md`](GRAPHICS.md) |
| Chase a web stutter | [`WEB_PROFILING.md`](WEB_PROFILING.md) |

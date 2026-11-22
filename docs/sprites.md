# Loka sprite manifest (style A)

The world engine draws characters, buildings, places and decorations from one PNG atlas plus `manifest.json`, both in `frontend/public/sprites/style-a/`. Anything the manifest does not name keeps today's code-drawn art, so an export can land piece by piece. The parser is `parseManifest` in `frontend/lib/sprite-manifest.ts`; it refuses a bad file with the path of the bad field, and the engine then keeps the drawn art.

## The New art (preview) switch

Style A is off by default. Each person turns it on under You > Appearance > New art (preview), saved to their account as `users.new_art` (migration 153, `GET`/`PATCH /api/world/me` field `new_art`). The switch applies live: `chooseNewArt` in `sprites.ts` loads the atlas, then the game rebuilds the current world and engine; nothing reloads. The art flag itself flips only inside that rebuild (`showNewArt`, called right before the world is built), so no frame mixes classic and new art or uses the wrong zoom. Until the atlas is ready the classic art stays on screen. The manifest and the atlas each get `LOAD_TIMEOUT_MS`; a load that fails or times out is tried again after `retryDelay` (30 s, doubling up to 5 minutes) while the switch stays on, and at once when the switch is turned on again. Each sheet has at most one waiting retry (`retryTimers`); turning the switch on cancels it and starts again from the shortest wait, and a successful atlas load resets the atlas backoff. You > Appearance reads `newArtStatus` (`onNewArtStatus`): when the manifest or atlas failed it says the classic art is showing and will retry, and when only some body or accessory sheets failed it says some figures are still loading.

With it off, the classic art is drawn exactly as before the redesign. `world-game.tsx` builds Hearth and regions with `world-classic.ts` and `regions-classic.ts` (verbatim copies of the pre-redesign `world.ts` and `regions.ts`: old lot sizes, lamp spots, ground colours and window light pools), the engine keeps the old zoom (`round(css * dpr)`, dpr capped at 2) and never loads the atlas, and `figureOf`/`thingOf` return nothing, so every agent and thing keeps its code-drawn art. Keep those two files untouched; a change meant for both arts goes into each.

Style A agents wear no hats: `withoutHats` (`frontend/lib/looks.ts`) drops every hat accessory (`HATS`) from the look before a cast figure is built, and keeps the rest. The classic art still draws hats.

Accessories without art are dropped the same way (`withArtOnly` in `frontend/lib/sprite-manifest.ts`): quest gear such as a quill or feather, and the golden quill cosmetic, leave the look before a cast figure is built, so an agent whose body has art never falls back to the classic figure. While its body or accessory sheet is still loading, an agent shows only its shadow (`figurePending`); a sheet that fails or takes longer than `LOAD_TIMEOUT_MS` falls back to the drawn figure and is tried again in the background after `retryDelay` (and at once when the switch is turned back on), keeping the drawn figure until it loads.

## Cost

Measured on the current export (`python3` over `frontend/public/sprites/style-a/`, frame times from a headless Chrome harness with a pixel read each frame so the raster is included):

- On disk: 1.53 MB in all. `atlas.png` 0.30 MB, 30 body sheets 0.93 MB, 16 accessory sheets 0.13 MB, `manifest.json` 0.17 MB.
- Decoded: 50.8 MB if every sheet loaded (atlas 3.4 MB, bodies 40.1 MB, 0.7 to 2.2 MB each, accessories 7.2 MB). Sheets load only for bodies and accessories in use: a town of 12 agents with 7 bodies loads 15 sheets, 14.7 MB.
- Per look: the cut and recoloured frames of one figure are about 0.8 MB (27 frames of 64x118 for a human), built once per look and cached.
- Frame time, 12 agents and 4 shops, median per frame (classic, then new): 1440x900 at dpr 1, 6.7 and 6.1 ms; at dpr 2, 26.1 and 24.2 ms; at dpr 3 (capped to 2), 26.8 and 24.2 ms; a 390x844 phone at dpr 3, 6.7 and 6.5 ms. These are software raster numbers, so real GPUs are faster; the new art costs no more than the classic art.


## Updating the art

Two exports live in the repo, outside `public/`, so they never ship as they are:

- `frontend/sprites/style-a/`: Buildings, places and decorations. `manifest.json` (an `items` list: `name`, `file`, `w`, `h`, `density`, `anchor`, optional `sign`, `masks`, `points`) plus `sprites/` and `variants/`.
- `frontend/sprites/cast/`: Characters. `manifest.json` (version 2: `bodies`, `accessories`, `size_classes`, `region_colours_in_mask`, plus `hole_check`, `proportion_template` and other notes the packer ignores) plus `bodies/` and `accessories/`.

Full-size sources stay out of git. A manifest may name its source zip in `sources_url`, and the packed manifest keeps them as `sources.world` and `sources.cast`. To take a new export, replace its folder whole and repack:

```bash
rm -rf frontend/sprites/cast && mkdir -p frontend/sprites/cast
cp -R <export>/manifest.json <export>/bodies <export>/accessories frontend/sprites/cast/
python3 frontend/scripts/pack-sprites.py
```

It needs Pillow (`--world`, `--cast` and `--out` pick other folders). It writes `frontend/public/sprites/style-a/`: `atlas.png` with every thing, `cast/bodies/*.png` and `cast/accessories/*.png` (the export sheets as they are, masks left out), and `manifest.json`. Run `npm test` in `frontend/` afterwards.

- Thing points come only from each item's `points` (`door`, `chimney`, `clock_face`, `flag`, `light`, `flame`, `opening`, ...), scaled by `conventions.points` (x and w by png width / `w`, y and h by png height / `h`). A rect becomes two corner points. For `clock_face` the packer measures the face, adds `clock_rim` at 0.8 of its radius and clears the painted hands inside 0.65 of it, because the engine draws the live hands.
- Cast bodies keep their sheet, frame size, feet anchor and 8-frame walks per view. Each walk frame gets `slots` = `accessory_anchors[view]` plus `walk.offsets[view][k]`, except `hand`, which is `accessory_anchors[view].hand` plus `hand_walk_offsets[view][k]`, and `layers.hand` = `hand_walk_layers[view][k]`. Swaps come from `palette` and `palette_regions` for the regions skin, hair, shirt, pants, suit and tie, shaded by lightness against `region_defaults`.
- Cast accessories keep every size class (44, 52, 60, 72, 88) with a rect, pivot and layer per view.

## Scale

- Art px: the world unit (a tile is 16 art px).
- `density`: sprite px per art px. It must be 1, 2 or 4. Characters are 4 (one sprite px is one CSS px on a 1440x900 window) and the world is 2.
- Zoom (snap rule B): `css = max(2, floor(min(w/320, h/192)))` as before, `P = round(css * min(dpr, 2))` snapped to the nearest multiple of 4 (ties up, at least 4). dpr is capped at 2 like the classic art, so a dpr 3 phone sees the same number of tiles (P = 4 on a 390x844 phone, about 18 tiles across). A sprite px is then always a whole number of device px (`P / density`), and the zoom stays within 0.8x to 1.33x of the classic scale once that is 3 or more.
- Characters are drawn 1.5x the height of the old 20 art px figures (a 114 px human, a 96 px kid, a 104 px crab). Their feet anchor and hitbox stay where they were, and labels, bubbles and prompts rise with the sprite.
- World sprites are 1.25x the old size. A building is anchored bottom centre on its door, so the lots are 9x7 tiles plus the roof rows the shop art reaches above them (3 rows today, measured by `lotRoof`). A new lot's roof and body may not cover another building's art or door row, a portal, the banyan, an exit road or its signpost; the walkway row in front of a door may sit under a roof.

## File

```json
{
  "version": 1,
  "atlas": "atlas.png",
  "sources": { "world": "https://...", "cast": "https://..." },
  "bodies": { "<key>": Body },
  "accessories": { "<accessory>": Accessory },
  "things": { "<key>": Thing }
}
```

Colours are `#rrggbb`. Points are `[x, y]` and rects `[x, y, w, h]`, in sprite px. Points inside a frame are relative to its top left corner. Unknown fields are ignored at every level.

### Body (a character base)

| Field | Meaning |
|---|---|
| `species` | Species names this body draws, using the look options or their aliases (`human`, `businessman`, `kid`, `fox`, `bunny`, `unicorn`, ...). The human body also lists `person`, so stored looks keep working. |
| `atlas` | Optional sheet next to the manifest (`cast/bodies/human.png`). It loads the first time an agent needs it; until then that agent keeps its drawn figure. Without it the body is in `atlas.png`. |
| `classes` | `{ "head": 60, "body": 44 }`: the accessory size classes this body wears. |
| `density` | Default 4. |
| `size` | Frame size; every frame of the body has this size. |
| `anchor` | Feet point inside a frame. |
| `sideFaces` | `left` (default) or `right`: which way the side view faces. The engine mirrors the other side. |
| `features` | Accessories already drawn into the body (species features such as `ears-pointed`, or Pip's `bun`). |
| `views.front`, `views.side`, `views.back` | Each `{ "walk": [frame...], "idle": [frame...], "slots": { "head": [x, y] } }`. front and side are required; back falls back to front. walk is 8 frames for one cycle (two steps); idle defaults to the first walk frame. |
| frame | `[x, y]` atlas position, or `{ "at": [x, y], "slots": { "hand": [x, y] }, "layers": { "hand": "under" } }` to move a slot for this frame (head bob, a swinging hand). A slot layer of `under` hides that slot's accessory on this frame (the briefcase while the hand swings behind the back); `over` or no layer draws it as usual. Slots may sit outside the frame. |
| `swaps` | `{ "#rrggbb": "role" }` or `{ "#rrggbb": ["role", shade] }`. Roles are `skin`, `hair`, `shirt`, `pants`, `accent`, `suit` and `tie`, the colours `lookFor` picks for each agent (suit takes the pants colour and tie the accent; hair is lifted to at least 0.3 lightness so it never merges with the outline). shade runs from -1 (black) through 0 (the role colour) to 1 (white), so one base sprite gives every agent its own palette. |

A body is used only when its `features` are all in the look and every other accessory of the look, apart from the species' own features (a fox's ears and snout are drawn into the fox), has an entry in `accessories`. Otherwise that agent keeps the drawn art, so nobody loses a hat or gains one. A left-facing view mirrors the whole figure, body and accessories together: x becomes `frame_w - 1 - x` and the hidden frames stay hidden.

### Accessory (an overlay)

| Field | Meaning |
|---|---|
| `slot` | The body slot it hangs on (`head_top`, `brim`, `eyes`, `neck`, `back`, `hand`, ...). |
| `basis` | `head` (default) or `body`: which of the body's `classes` picks the size. |
| `atlas` | Optional sheet, as for bodies. |
| `sizes` | `{ "60": { "front": { "rect": [x, y, w, h], "pivot": [x, y], "layer": "over" }, ... } }`. The class nearest the body's wins. A missing side view uses front; a missing back view draws nothing (glasses from behind). `layer: "under"` draws it before the body (a cape). Older single-size accessories (`size`, `pivot`, `views`, `behind`) still load. |
| `swaps` | As for bodies. |

Recoloured and layered frames are built once per look and cached, never per frame. `figureByLook` is the only place built figures live: it keeps the `FIGURE_CACHE` (48) most recently used looks, about 38 MB at most, while `figureBySheet` keeps only each sheet's filtered look and cache key, so an evicted figure is rebuilt from the loaded sheets the next time it is drawn.

### Thing (building, place or decoration)

| Field | Meaning |
|---|---|
| `density` | Default 2. |
| `anchor` | Ground contact point, bottom centre. The engine puts it where the drawn sprite stood and draws the drop shadow there: an ellipse `0.42 * w` by `0.07 * w`. Do not bake shadows. Night is the engine tint only: sprites carry no glow masks and buildings no lit windows. |
| `frames` | One rect per variant; a position hash picks one. |
| `night` | Optional rects for a night version, one per frame. |
| `sign` | Blank board rect where the engine writes the place name. |
| `points` | Named point lists from the export: `chimney` (smoke), `clock_face` and `clock_rim` (live clock hands), `flag` (post office mail flag, raised when mail waits), `door` (the library reading lantern hangs 32 art px above it), `lens` (observatory star), `light` and `flame` (lamp pools), `opening` (the portal veil, two corners). Once a thing is drawn from the atlas only its own points count: a missing name drops that overlay instead of using the drawn sprite's spot. Buildings get no window light pools at night; lamps keep theirs. The portal veil shimmers over `opening` as an engine layer, day and night. |
| `masks` | `{ "roof": { "at": [x, y], "base": "#c05b47" } }`: white mask the size of the frame. Masked pixels take the role colour with their brightness relative to `base` kept. Houses pass `roof`, `wall`, `awning` and `awning2` from their shop style. |
| `swaps` | As for bodies, for unmasked colours (house walls). |

Thing keys the engine looks up: `shop`, `civic_clock`, `civic_library`, `civic_forge`, `civic_post`, `civic_market`, `civic_observatory`, `banyan`, `portal_arch`, `noticeboard`, `waysign`, `lantern_post`, `campfire`, `banner`, `stake`, `tree_green`, `tree_blossom`, `tree_autumn`, `tree_pine`, `tree_pine_snow`, `bush`, `bush_flowers`, `rock`, `reeds`, `cactus`, `ice_rock`, `lamp`, `bench`, `crate`, `barrel`, `planter`, `signpost`, `palm`, `snowman`, `mossy_log`, `sandstone`, `bollard`, `boulder`.

Body species the engine can draw: every name in `SPECIES_OPTIONS` (`frontend/lib/looks.ts`), the same list the agent tools offer. `human` is the default, and `person` or any species Loka does not know draws a human; `businessman` (with a briefcase unless the look names its own accessories) and `kid` are human bodies with human palettes.

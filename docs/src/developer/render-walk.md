# The render walk

`just render-walk` renders the map and the screens over many views and
checks each picture against named rules. It covers anything that changes
how the app looks: the map, the UI, its text. A golden pins one front view
per test. The walk exists because one view per test is not enough. On
2026-11-13 the globe build of 6bb85004c passed every golden. On a phone the
same build drew a Bitmap half again the size of the frame, let the world
shrink below the screen, and grew the native heap to 630 MB.

```sh
just render-walk --quick              # per batch: 70 to 95 s warm
just render-walk                      # everything: about 260 s warm
just render-walk --selftest           # the positive controls: 100 to 140 s
just render-walk --out DIR --m9       # elsewhere, with the M9 sim's walks
```

The output lands in `target/render-walk/` unless `--out` names another
directory:

- `index.html` is the contact sheet. It shows every picture with the rules
  it failed, plus a table of each gesture's frame times and heap. A failed
  picture has a red border and links to the whole screen. The others are
  thumbnails.
- `summary.tsv` has one row per rule per frame, gesture or screen. Its
  columns are `variant kind sequence size centre zoom image rule value
  limit verdict note`.
- The exit status is 1 when any rule failed and 2 when the walk itself
  could not run.

The measured times are from schneckenschreck on 2026-11-13, with a warm
build: quick 70 s and 94 s, full 259 s, selftest 103 s and 139 s. A cold release build of the
walk adds about two minutes for each projection.

## What it draws

The walk draws the same code the frontends draw with. There is no second
renderer.

- **The phone's map** is drawn through `loquax_android::map::render_into`,
  the facade call the app makes. It is drawn into a Bitmap that the walk
  sizes the way `MapView.java` sizes it: the screen plus the margins of
  `MapFrame.margin`, a quarter of each side, and no margin while the last
  frame turned the globe. A test reads `MapFrame.java` so that the two
  sides agree. The walk also keeps what the screen shows the way Java keeps
  it. When a frame fails, Java shows the last good frame scaled to the zoom
  the fingers asked for. That is how the failed globe frames of 2026-11-13
  became Lew's "small rectangle".
  The driver is `loquax-android/examples/render_walk.rs`, built once:
  the globe is the only world view since 1809 (until then it was built
  twice, for Equal Earth and with the `loquax/globe-world` switch).
- **The zoom ladder** covers every integer zoom from the world at its
  smallest to the deepest zoom the regions hold.
  - Centres: Germany (51 N 10 E), Morocco (31 N 5 W), both poles (85°),
    the antimeridian (17 S 179.99 E), and Berlin and Florence, where the
    fixture regions give street level from data, and Erg Chebbi and Erg
    Chegaga (the z10 tiles at 31.20 N 4.04 W and 29.99 N 6.15 W) and the
    Bani escarpment (29.76 N 6.77 W, the middle of the `bani-z11.lmh`
    block), where three Moroccan fixture regions lie over the height
    packs `erg-chebbi-z10.lmh`, `erg-chegaga-z10.lmh` and `bani-z11.lmh`:
    the desert relief under the vector map (1741, 1851, 1868). The packs
    are not copied into a walk home: the regions are, under the names the
    autoload gives them, and the shell's loader with `with_heights` finds
    them on disk and fetches the blocks under them from a local range
    server holding the packs and their catalogue, as a device fetches
    them (the audit of 1863). The first home prints the packs it got.
  - Frame sizes: 1224x2197 at a density of 2.75 (Lew's phone), 1080x2400
    at 2.625, 1600x2560 at 2.0, 900x560 at 1.0 (the window), and
    2197x1224 (Lew's phone in landscape).
  - Each zoom is asked at rest and settled on a held clock until its fades
    and tiles are done, which is what a person sees once the screen is
    still.
  - `--quick` keeps the phone and the window, Germany, the north pole and
    Berlin, and the zooms 4 to 16 in steps of two plus the floor.
- **The gestures** run on each frame size. `--quick` runs them on the
  phone only.
  - `erg-pan-seam`: 40 px a frame for 24 frames east across the relief
    tiles' seams at z12 over Erg Chebbi, from a fresh home, so the relief
    store starts cold (order 1853).
  - `erg-pinch-12-14`: over Erg Chebbi from z12 in to z14 and back out, a
    quarter step a frame about the screen's middle, cold as above.
  - `bani-pan-seam`: as `erg-pan-seam`, over the Bani escarpment (1868).
  - `pan-seam`: 40 px a frame across three tile seams at z13.
  - `pinch-out-in`: a quarter step a frame from z12 out to the world and
    back.
  - `pinch-at-thumb`: the same pinch about a focus a third into the
    screen; the ground under it must stay there.
  - `drag-pole-around`: 30 frames toward the north pole, then 30 frames
    around. This turns the globe.
  - `drag-globe-floor`: the phone's drag path at the zoom floor (order
    1883): 30 px a frame across, up, diagonally and down to the pole, 20
    frames each, half of every frame's move made while the frame renders,
    as a finger does on the device. `globe_disk_fixed` judges the disk
    where Java shows it, the frame under the gesture's leftover
    transform; before 1883 that disk slid by the move made during each
    render.
  - `jump-street-to-z5`: from Berlin z16 to Europe z5, the way a tap on a
    position asks for it.
  - `rotate`: portrait to landscape and back, at rest.

  Each gesture records its frame times on the host and the live heap at
  its start, peak and end. A counting allocator in the walk measures the
  heap.
- **The window's own map** (variant `window`, audit of 1943) runs the
  window's real path headless: the App, its scheduler and its tile
  cache with the curved memo on. It is released, since its rows judge
  frame times. A store of 16 overlapping regions of mixed zoom spans is
  cut first, and the pane is the desktop's 1800x1320 and 1440p, where
  zoom 10 lies inside the blend window. Four sequences: a pan, a wheel
  step out (z10 to z9), the step back in, and the pan again while
  regions land and tiles arrive under every step. Every fourth moving
  frame and the rest frame are kept, presented as the window presents
  them (nearest while moving, the map's ground where the frame does not
  reach). The test is `the_window_map_walks_inside_the_blend` in
  `loquax/tests/support/load_walk.rs`; its rows go to
  `rows-window-map.tsv`.
- **The window's screens** are rendered by iced's software renderer:
  - the chats, light and dark
  - the settings
  - the first start
  - the map with two pins

  The test is `every_window_screen_draws_at_every_walk_size` in
  `loquax/src/gui.rs`. The window has no density of its own, so the
  phones' sizes are taken at their logical size.
- **The terminal's screens** are the empty list, a chat with a late
  message, and the settings. They are drawn at the cells each size holds,
  at 8x16 logical pixels a cell. The test is
  `every_terminal_screen_draws_at_every_walk_size` in `loquax/src/tui.rs`.
- **With `--m9`**, the walk also runs `just m9-sim-walk`, the M9 host
  sim's walks, and puts their dumps on the sheet.

The existing goldens stay with their tests. `cargo test` and
`just loquax-gui` pin each golden pixel for pixel. The walk adds every
other view.

## The rules

Each rule has a name, one threshold and a measured value. The definitions
live in `loquax-android/src/render_walk.rs`.

| Rule | Applies to | Fails when |
|---|---|---|
| `bitmap_is_frame` | every map frame | the frame does not fit the Bitmap Java made |
| `view_covered` | every map frame | more than 0.5 % of the screen is undrawn (magenta on the sheet) |
| `world_spans_view` | a turning globe | more than 2 % of the middle line across the shorter side is space |
| `no_error_text` | every map frame | the frame says an error or a notice |
| `no_map_chip_over_data` | every map frame | the "no map here" chip is up at a zoom where the world's outline is drawn (z10 or shallower) |
| `label_inside_view` | every map frame | a shown name straddles the screen's edge |
| `label_clear_of_controls` | every map frame | a shown name lies under the map's two round buttons (56 dp, 16 dp from the end, 16 dp and 84 dp from the bottom) |
| `label_overlap` | every map frame | two shown names overlap |
| `label_current_zoom` | a settled frame | a name is from a deeper zoom than the frame's, or is still fading out |
| `label_on_world` | a turning globe | a name floats in space: three of its box's corners and middle are space |
| `frame_p95` | a gesture | the 95th percentile frame time is over half a frame at 60 Hz (8.3 ms; order 1853 moved it from the whole frame of 1799 to the line the note always named) |
| `heap_returns` | a gesture | the live heap ends more than 160 MB above its start |
| `heap_peak` | a gesture | the live heap grows more than 320 MB during the gesture |
| `view_kept` | the rotation | the centre or the zoom moved |
| `globe_disk_fixed` | a turning globe's gesture frame | the disk fitted from the frame's own pixels, placed where Java shows it (the leftover gesture transform, order 1883), differs from the gesture's reference frame at the same zoom by more than 1 px in centre or radius (order 1818, the wobble proof) |
| `bare_paper` | a gesture frame | more than 10 % of the screen is bare paper where the same view, settled at rest after the gesture, shows map content (order 1853: a pan frame over Berlin was nearly empty and no rule saw it). Paper is counted in 16 px blocks, a block bare where 12 of its 16 samples are paper, and the rest frame's shown name boxes read as paper: names fade by design while a finger moves, so a gesture frame without them misses no map. Only frames with more than 10 % bare blocks are settled again; the others carry their share as the bound |
| `view_only_by_input` | a settled frame, a gesture's rest held 120 s of held clock, the rotation's MapView made anew | a frame asked with no input shows another view than the one the last input left, by its centre or a thousandth of zoom (1819: the desktop's minute beat and the landed regions moved the view; the phone's recreated Activity fitted it anew) |
| `zoom_keeps_the_place` | a zoom gesture's frame | the geographic point that started under the pinch focus is more than 2 px from it; the rest frame of an out-and-in pinch adds one row for "the start view is back" (order 1818, the Morocco-to-Spain jump) |
| `frame_p95` (window) | a window map sequence | the 95th percentile step, drawing included, is over one frame at 30 Hz (33.3 ms; the shared coder host's load moves single steps by tens of milliseconds, so a 60 Hz gate judged the host) |
| `input_rides_the_memo` | a window map sequence | a whole curved frame was drawn while a finger dragged |
| `view_covered` (window) | a window map pan or rest frame | any of the screen lies outside the frame. A wheel step's frames are exempt: the glide scales the frame of the view it ends on, and stepping in that frame lies inside the screen with the ground round it by design (1441) |
| `sea_strip` | a window map frame | more of the screen is the theme's water colour than the rest frame shows, plus 2 % (the strip a moving curved frame painted where the memo did not reach, before the audit of 1943) |
| `screen_drawn` | a window or terminal screen | it drew nothing. For the window, that is at most two of four quarters with more than one colour. For the terminal, it is text on less than a tenth of its rows |
| `screen_repeats` | a window or terminal screen | the same screen drawn twice differs |

A moving frame is judged by the frame rules, `label_on_world`,
`label_inside_view`, `label_clear_of_controls` and `label_overlap`
(order 1818): the names of the last rest frame ride along while a
finger moves, and the ride must not push them over the edge, the
buttons or each other. Only `label_current_zoom` waits for the settle,
since names fade by design while the map moves. The gestures' moving
frames run on a held clock, 16 ms a frame, so two runs give identical
frames; the frame-time rows stay the wall clock's, measured around the
draw itself.

`RENDER_WALK_NAMES=<image>` makes the driver print the names of that
picture with their boxes, which is useful for checking a label verdict
by eye.

## Positive controls

`just render-walk --selftest` runs a small walk over Berlin on the phone
size. It runs once clean and once for each injected fault. Each fault must
turn its rules red on a frame or gesture where the clean walk is green.

| Fault | Rules it turns red |
|---|---|
| `oversized-bitmap` (the 1796 Bitmap) | `bitmap_is_frame`, `view_covered` |
| `blank-half` | `view_covered` |
| `stray-label` | `label_inside_view`, `label_clear_of_controls`, `label_overlap`, `label_current_zoom` |
| `error-text` | `no_error_text` |
| `slow-frame` | `frame_p95` |
| `heap-kept` | `heap_returns`, `heap_peak` |
| `view-lost` | `view_kept` |
| `disk-shift` (every moving frame's picture 3 px right) | `globe_disk_fixed` |
| `zoom-drift` (the view pushed 5 px once a zoom stands in the handover) | `zoom_keeps_the_place` |
| `bare-frame` (every moving frame's lower half painted paper) | `bare_paper` |
| `stray-view` (the second frame asked again at rest answers a zoom half a step out) | `view_only_by_input` |

On the host the clean walk fails `frame_p95` on nearly every gesture. The
rotation sometimes passes, but its frames are at rest and the fault does
not slow them. So for `frame_p95` the control asks for a step instead of a
turn: the p95 must grow by at least 15 ms when 20 ms are added to every
moving frame.

Three rules have no injection in the selftest: `world_spans_view`,
`no_map_chip_over_data` and `label_on_world`. Their unit tests in
`render_walk.rs` inject the fault. `loquax-android/tests/render_walk.rs`
runs the Bitmap rules on a real Florence frame.

## What it does not see

- **Java's own state.** The rotation checks the library's half: the view
  Java hands back is kept. It does not check whether `MapView` restores
  its view after an activity is recreated, which is walk finding 10 of
  2026-11-13.
- **The phone's own frame times and heap.** The walk measures on the
  host. The rate the rule holds to is 60 Hz, and the walk cannot see the
  display's real rate.
- **Real regions.** The walk draws the fixtures under
  `leviculum-map/fixtures` with the autoload off. A heap peak that only a
  phone with many downloaded regions reaches is out of its view.

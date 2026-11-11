# Nessa

Nessa is the Leviculum suite's lioness and its logo (Lew, 2026-11-11, order
1690). She appears on the launcher icons, in the windows, on the website and
on the M9's power-on screen, and every one of those pictures comes from
`nessa-R3-b.svg` in this directory: the SVG is the only source, the script is
the only renderer, and no picture is ever edited by hand.

## The source

`nessa-R3-b.svg` is draft R3-b as Lew drew it. It is one 512 square with the
body, the tail, the head, the face, and three signs hidden behind
`display:none`: a speech bubble (`sign-loquax`), a compass (`sign-lnomad`) and
a bolt (`sign-lnflash`). Each app shows the head with its own sign; the head
alone, or the whole figure, is what the handbook and the website use.

The figure will be redrawn later with an image model. The head and the signs
are final enough to ship, so this directory is the pipeline, not the final
art: replacing `nessa-R3-b.svg` and running the script moves every product at
once.

## The palette

| colour | where |
| --- | --- |
| `#f0b860` and `#b8702a` | the fur, and its shadow |
| `#f5cc86` | the forehead blaze |
| `#fbe6c2` | the muzzle, the eye whites, the speech bubble's dots |
| `#2b1d14` | the line work |
| `#c4502f` | terracotta: the nose, and the signs |
| `#f0b860` behind the foreground | the adaptive icon's background (`android/res/values/ic_launcher.xml`) |

The M9's splash is the exception: it is hand-drawn line art in one bit
(`loquax-m9/assets/splash-head.svg`), because her cream muzzle and dark nose
threshold into a bear's snout at 112 pixels on black.

## What is rendered, and where it lives

`python3 -I scripts/nessa-render.py` writes all of it; `--check` re-renders
into `target/tmp` and compares bytes. `just nessa-check` runs the check, in
`guards` and in `fast`.

| picture | from | where |
| --- | --- | --- |
| launcher PNGs, 48/72/96/144/192 px | head + speech bubble | `loquax-android/android/res/mipmap-*/ic_launcher.png` |
| adaptive icon layers | head + speech bubble | `loquax-android/android/res/{drawable/ic_launcher_foreground,drawable/ic_launcher_monochrome,mipmap-anydpi-v26/ic_launcher,values/ic_launcher}.xml` |
| the icon sources | head + sign | `loquax-android/icon/{loquax,loquax-foreground,loquax-background,loquax-monochrome}.svg` |
| the apps' SVG marks | head only, head + sign | `head.svg`, `loquax.svg`, `lnomad.svg`, `lnflash.svg` |
| 256 px PNG | head | `head-256.png` |
| 32 px PNG | head + speech bubble | `favicon-32.png` |
| 512 px PNG | the whole figure | `figure-512.png` |
| desktop icons, 256 px | head + speech bubble, head + sign | `packaging/loquax/loquax.png`, `packaging/lnomad/lnomad.png`, `packaging/lnflash/lnflash.png` |
| the window icon, 128 px | head + speech bubble | `leviculum-ui-iced/assets/window-icon.png` |
| the handbook's pictures | head, figure, 32 px favicon | `docs/src/images/nessa-{head,figure,favicon}.png` |
| the M9's splash, 1 bit a pixel | `loquax-m9/assets/splash-head.svg` | `loquax-m9/assets/splash-head.{png,1bpp}` |

The website's pages (`site/pages/`) link the handbook's copies at
`/docs/images/...`, because lblogd serves images only from the operator's file
area and ships no static assets of its own.

## Licence

Nessa is our own work, drawn by Lew Palm for Leviculum, and she ships under
the same AGPL-3.0 as the rest of the tree (see `LICENSE` at the root). No
third-party artwork is involved, which is why there is nothing to add to
`THIRD-PARTY-NOTICES`.

## Redrawing her

1. Edit or replace `nessa-R3-b.svg`. Keep the group ids `body`, `tail`,
   `head`, `face`, `sign-loquax`, `sign-lnomad`, `sign-lnflash`: the script
   finds the parts by name and refuses a source that lacks one.
2. Run `python3 -I scripts/nessa-render.py`.
3. Look at `head-256.png` and the smallest launcher PNG. Four pixels of
   muzzle at 48 px is where a face stops being her.
4. If the splash needs the change, edit
   `loquax-m9/assets/splash-head.svg` by hand and run the script again; it
   also writes the `.1bpp` the board burns.
5. `just nessa-check`, then commit the SVG, every generated file, and the
   goldens the pictures moved (`loquax-m9/tests/golden/`, the GUI and TUI
   snapshots).

# Relief goldens

The pictures `../../relief.rs` (`relief_goldens`) holds the relief to
(order 1741 and its audit): style R′ over the light table's paper, no
vector layer, 256 x 256 pixels centred on each fixture pack's one block,
sampled the way the frontends sample (the pack's own grid between its
block and tile zooms, bicubic into the view's pixels). A channel may move
by one; a picture that moves further is left in
`$TMPDIR/leviculum-golden/relief-<name>.png`, and `LEVICULUM_GOLDEN_BLESS=1`
writes the goldens again when the change is the one intended.

| golden | pack | what it shows |
|---|---|---|
| `chebbi-z10` | `erg-chebbi-z10.lmh` | the whole z10 tile around Erg Chebbi: the star-dune massif as a lit body in the south-east, the hamada and its drainage west and north, faint at this zoom (the derived grid averages the fine band away) |
| `chebbi-z12` | same | one z12 tile: the star dunes in the east with their shaded lee faces, interdune flats, the reg to the west |
| `chebbi-z14` | same | a quarter tile: soft dune flanks, GLO-30's 30 m posts upsampled four times |
| `chegaga-z10` | `erg-chegaga-z10.lmh` | Erg Chegaga's dune belt between the wadi floor in the north and the plain in the south, the NNE-SSW chains already visible |
| `chegaga-z12` | same | the linear chains and their lanes, the order's must-not-erase case |
| `chegaga-z14` | same | the chains 20 to 80 m high, a few hundred metres apart, each with its lit and shaded flank |
| `bani-z12` | `bani-z11.lmh` | the stone desert at Jbel Bani: the escarpment ridges, gullies cut into them, the hamada plain between |
| `bani-z14` | same | gullies and spurs of one ridge, the plain below it calm |
| `alps-z12` | `alps-z11.lmh` | central Switzerland (the MapOut area): high relief compressed to the same spread as the desert, busy and greyer than MapOut (the order's known limit 1) |
| `uckermark-z12` | `uckermark-z11.lmh` | the moraine lowland: kettle lakes flat, field edges and forest canopy as soft steps (GLO-30 is a surface model; the order's known limit 2) |

Each `<name>-before.png` is the same window drawn by the code before the
audit's seam fix (each plane measuring its own noise floor and sky
exaggeration): what the per-tile ground changed. The desert windows move
by a few luminance steps, the Uckermark calms (p5 151 to 160).

Heights: Copernicus GLO-30, see `../../../fixtures/README.md`. Produced
using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and © Airbus Defence
and Space GmbH 2014-2018 provided under COPERNICUS by the European Union
and ESA; all rights reserved.

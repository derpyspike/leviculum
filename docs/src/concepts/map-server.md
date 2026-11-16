# The map server

Order 1686 (K4), the expedition-map half of the Leitstern: the server
is the precondition for loading by looking, shading, points of
interest and sharing. This page decides the store on disk, the signed
index, what a device pins, the request protocol shared by Reticulum
and HTTP, the front-door cache, and the attribution per pack. The
minimal implementation lives in `lnmapd/` — `lnmapcut` writes the
store, `lnmapd` serves it, cutting and serving separate programs —
and every claim of this page is held by that crate's tests.

Decided by Lew (2026-11-11): every kind of data — vectors, shading,
points of interest, later the elevation fine tier — is a pyramid cut
on the server into immutable pieces; the server signs an index of
their hashes; a device verifies against the pinned server key,
whoever carried the piece; the setting is the server's Reticulum
address, the single trust anchor (`map_destination`); the store lives
at Lew's home, workorse is the public front door with a cache.
**Proposed by the reviewer, not Lew's word, in two places below:**
that the signed index names the server's own HTTP address, so one
value configures both paths (§ "The index"), and that the home store
opens no port and builds the outgoing tunnel to the front door itself
(§ "The tunnel from home").

[Loquax: map data over Reticulum](loquax-map-over-reticulum.md) fixed
the destination (`lev.maps`, its own daemon `lnmapd`, never a mode of
`lnpnd`), the client ladder, the speed probe and the fairness rules;
all of that stands. Its request API (`/regions` with whole-file
hashes, `/region` by id) is superseded by the piece protocol here:
the index replaces `/regions`, pieces replace `/region`, and the
catalogue thinking — pre-cut packs, the client takes the smallest
covering its view — is unchanged.

## The store on disk

```text
<store>/index                  the signed index, replaced atomically
<store>/pieces/<aa>/<64 hex>   one piece, immutable, named by the
                               SHA-256 of its bytes (<aa> its first
                               two characters, fanning the directory)
```

One directory, two transports: `lnmapd` serves it over HTTP and over
Reticulum, nginx may serve the same files statically. Pieces are
immutable — their name is their content — so every cache layer from
the front door to a browser may hold them forever; only `index`
changes, and it is replaced atomically (temp file and rename), so a
reader never sees a half-written index.

A pack is one logical cut of one data kind: `vectors` (a region file
per catalogue entry, the format of [Loquax: the map](loquax-map.md)
section "Region file") and, since order 1740, `elevation` (a height
file, § "Height packs" below) and, since order 1743, ground packs
(§ "Ground packs", not in the store yet); shading and pois-as-their-own-tier get
their own `kind` strings in the same index without a format change.

Each pack entry names the format version of its file and the oldest
reader version that reads it (1714, `lnmapd/src/store.rs`): for a
region the LMR version and the header's minimum reader version. A
client skips a pack whose minimum reader version is newer than the
region reader it was built with and tells the user in one sentence to
update (`lnmapd::client::readable`, which the prefix fetch runs before
it asks for the first piece); a newer format an old reader still reads
keeps its minimum reader version, so the update notice appears only for
a change that needs it. An index written before 1714 says 0 for both,
and the region's own header decides once it arrives.

## The pieces: a prefix split of the region file

The region format did the hard work already: from version 4 the file
is written index first, then the tiles coarse to fine, the names and
buildings last, exactly so that a reader of a file still arriving
uses every tile that is there (`leviculum-map/src/region.rs`, the
1291 layout). So a pack's pieces are nothing but the byte ranges of
the region file, split at zoom boundaries
(`lnmapd/src/store.rs`, `split_region`):

- piece 0: the header, the texts, the tile index, and the shallowest
  zoom's tiles;
- each further piece: one zoom deeper (a zoom with no bytes of its
  own folds into the piece before it);
- the last piece: the deepest zoom plus the label, building and poi
  sections at the file's end.

The pieces concatenate back to the byte-identical file, and **every
prefix of pieces is a readable region**: fetch them in order, stop
when the time budget is spent, and the map draws what arrived —
coarse before fine is the file layout, not a protocol feature. A
resumed fetch continues at the next piece; nothing is re-sent. Each
piece records the deepest zoom that is complete once it is there, so
a client can say "z13 landed, z15 is still coming" without opening
the assembly.

Within one pack the fine tier is not split by area: area granularity
comes from the catalogue (city and Bundesland packs), as decided in
[Loquax: map data over Reticulum](loquax-map-over-reticulum.md). A
country pack's fine pieces are large; the catalogue's answer is that
a country is Internet material, and the index's per-piece sizes let
the client ladder say so in numbers.

## The index

One file, binary, little-endian, signed; the exact byte layout is
the module doc of `lnmapd/src/store.rs` and is held by the format
tests. What it carries:

- **Header:** format version, **generation** (strictly grows with
  every write; the rollback check below), created time, and the
  server's own HTTP base URL. The URL in the index is the reviewer's
  proposal: with it, one pinned Reticulum address configures both
  paths — the device learns the HTTP side from the signed index and
  `map_source` stays an override, nothing more.
- **Per pack:** id, kind, bounds, zoom range, total bytes, the
  SHA-256 of the whole assembled file, the attribution piece's hash
  and length, and the pieces in assembly order — each with SHA-256,
  offset, length and the zoom it completes.
- **Trailer:** the server identity's public keys (x25519 and
  ed25519, the announce's 64 bytes) and an ed25519 signature over
  everything before them.

Versioning: the format version is for readers (a reader refuses a
version it does not know); the generation is for freshness. A device
remembers the highest generation it has verified and refuses an
index behind it — a replay of an old, validly signed index cannot
roll a device back to deleted packs or withdrawn pieces.

## What a device pins

One setting: `map_destination`, the server's `lev.maps` destination
hash. The verification chain
(`lnmapd/src/store.rs`, `Index::verify`; client side
`lnmapd/src/client.rs`):

1. The index's embedded public keys are hashed to an identity, and
   the `lev.maps` destination hash is recomputed from them. It must
   equal the pin — so the key that signed the index IS the key
   behind the destination the device would dial on the mesh, and an
   HTTP-only client needs no announce to check it.
2. The ed25519 signature over the index body must verify.
3. The generation must not be behind the highest already seen.
4. Every piece must hash to the name the index gave it — the whole
   piece check, whoever carried the piece: the front door, a
   neighbour, a USB stick. There is no trusted transport, only the
   pin.

A tampered piece is discarded before a byte of it reaches the
assembly; the good prefix stays usable. The shipped default pin is
Lew's server, as the zero-config page already lists its TCP entry; a
heard `lev.maps` announce that is not the pinned one is offered,
never auto-used ([Loquax: map data over
Reticulum](loquax-map-over-reticulum.md), "How it is found").

## The request protocol

The same two asks on both transports, the same piece ids (64 hex
characters of the SHA-256):

| ask        | HTTP                    | Reticulum (`lev.maps` link)            |
|------------|-------------------------|----------------------------------------|
| the index  | `GET <base>/index`      | request `/index`, no data              |
| one piece  | `GET <base>/piece/<id>` | request `/piece`, data `{"piece": id}` |

Answers are the raw bytes (HTTP) and the usual msgpack-bin response
(Reticulum), a single RESPONSE packet when it fits the link MDU, a
response Resource otherwise — the Resource's automatic bz2 earns on
region bytes what it earns on pages (the ~35 % measured in
[Loquax: map data over Reticulum](loquax-map-over-reticulum.md)).
Unknown ids: 404 on HTTP; on the mesh a silent drop, because the
protocol has no 404 and a clean timeout is the correct shape. The
speed probe (`/probe`) and the ask-before-LoRa rules stay as the
ladder page fixed them and ride on this protocol unchanged.

There is no "cut me this box" ask anywhere: the server does owe no
stranger a planet-archive cut; it serves what `lnmapcut` prepared.

## The front door

workorse is the public face; the store lives at home. `lnmapd
--upstream <home>` turns the same server into the front-door cache
(`lnmapd/src/serve.rs`):

- serves what it holds;
- a piece it lacks it fetches from the upstream **once**, checks
  that the bytes hash to the asked name (content addressing is the
  whole integrity check; a lying upstream is refused and nothing is
  cached), stores it, serves it — and never asks again, pieces being
  immutable;
- the piece cache is bounded (`--cache-cap-mb`): least recently
  served pieces leave when the cap is passed, the piece just written
  never;
- the index it refreshes on a TTL (`--index-ttl-secs`); a refreshed
  index must be well-formed, signed by its own embedded key and not
  behind the held generation, else the held one keeps serving — the
  front door cannot pin (it is not a device), but it never steps
  backwards and never serves garbage;
- upstream fetches are serialised: a cold cache costs the home link
  one transfer per piece, not one per requester.

The front door never signs anything and holds no key. A device
behind it verifies exactly as if it had asked the home store.

## The tunnel from home

**Proposed by the reviewer, not Lew's word.** The home store opens
no inbound port: it builds a persistent outgoing tunnel to the front
door itself (an autossh reverse tunnel or a WireGuard peer — K5
picks the mechanism on workorse), and the front door's `--upstream`
points at its local end of that tunnel. The failure modes are then:
home down → the front door serves everything it holds (measured in
the tests: index within TTL and all cached pieces); front door down
→ the mesh path to the home store still answers, since `lev.maps`
needs no inbound HTTP. Until the small home server exists, workorse
alone is both store and front (no `--upstream`), which is the same
binary and the same directory.

## Attribution

Every pack names an attribution piece: a small UTF-8 text piece —
the attribution line and the licence notice, taken from the region
file's own header texts — content-addressed and fetched like any
piece. A client that shows a pack's data can show its attribution
without fetching region bytes, and the licence text travels with the
data on every carrier. The region file itself carries the same texts
in its header (the map view's attribution line keeps working from
the region alone); the index-level piece is for catalogue UIs and
for data kinds whose own format has no text header.

## Worked example

The committed grunewald fixture (z12–15,
`leviculum-mapcut/fixtures/README.md`), cut by `leviculum-map-tool
cut` and added by `lnmapcut --store store --identity key --id
grunewald --http-url https://leviculum.network/maps grunewald.lmr`:

```text
grunewald: 4 pieces, 53302 bytes, zooms 12..=15, generation 1
  piece 0: completes z12, 22603 bytes, 40519689a831b747…
  piece 1: completes z13, 21974 bytes, cb7a33e0c21356ab…
  piece 2: completes z14,  4618 bytes, fb2aead0ac34dbbf…
  piece 3: completes z15,  4107 bytes, fa83751d2510cf05…
```

The signed index is 518 bytes. A client with a two-second budget on
a link that moves ~22 kB/s fetches piece 0, draws the forest at z12,
and picks up piece 1 in the next window; a fast link takes all four
and has the byte-identical region file. The tests
(`lnmapd/tests/store_format.rs`, `fetch_budget.rs`, `http_serve.rs`,
`node_integ.rs`) hold exactly these claims: every piece boundary is
a readable region with its zooms complete, the budget stops the
fetch cleanly, a tampered piece or index or a rollback is refused on
every transport, and the front door survives its upstream.

An id names one pack of one kind. `lnmapcut` refuses to put a height
file under the id of a vectors pack (or the reverse) and leaves the
store as it was, since a device that pinned the id would otherwise get
data of another kind; `--replace` says the swap is meant. The
prefix planner (`lnmapd::client::fetch_prefix`) takes only vectors
packs: a height pack's blocks stand alone and are fetched by block,
so its piece zoom byte is no "deepest zoom complete"
(`lnmapd/tests/store_elevation.rs`).

## Height packs

Order 1740 (K6a, Lew 2026-11-12: desert relief first, heights shipped
to the device instead of finished shade pictures). The data half only:
shading and drawing are order 1741. The byte layout is the module doc
of `leviculum-map/src/elevation.rs` and is held by
`leviculum-map/tests/elevation.rs` and `elevation_alloc.rs`; the cut is
`leviculum-map-tool heights` (`leviculum-map-tool/src/heights.rs`), the
store side `lnmapd/src/store.rs` (`KIND_ELEVATION`).

**Tile layout.** The heights live at z12 only, 256 x 256 per tile, one
height per Web Mercator pixel, sampled bilinearly from the source at the
pixel's centre, north row first. Tiles are grouped in **blocks**: the
8 x 8 z12 tiles under one z9 tile. A block is one piece. The pack's
file is a **directory piece** (the zooms, the box the cut was asked
for, the attribution and the notice, and the block list in piece
order), then the block pieces. z11, z10 and z9 are not shipped: the
device derives them from the block, each height the mean of the 2 x 2,
4 x 4 or 8 x 8 stored heights under it, rounded half up
(`Block::heights`). The pilot measured why: shipping z12 alone and
deriving the rest is 0.77 times the bytes of CoMaps' shade tiles for
the same box, the full z9 to z12 pyramid 1.16 times.

Unlike a region file, a height pack is not a prefix ladder: every
block piece stands alone and is complete from z9 to z12 over its own
area, so a device fetches the directory and then the blocks of its
view in any order. In the index, the directory's zoom byte is 0 (it
completes no zoom), every block's is 12; the pack's zoom range is
9..=12 and its bounds are the cut box.

**Encoding.** Whole metres as `i16`. Per tile: the residual of the 2D
predictor (left + up − up-left; the first row from the left, the first
column from above), in wrapping 16-bit arithmetic so every `i16` grid
round-trips; zigzag to `u16`; two byte planes (all high bytes, then all
low bytes); one zstd frame per tile, level 19, with its content
checksum. The writer is the C zstd library in the cut tool; the reader
is `ruzstd`, pure Rust and `no_std`, the same code on the boards, the
Android facade and the desktop. The committed Erg Chebbi fixture was
written by the C library and is read by the Rust decoder in the tests,
so the interop is tested, not assumed.

**Why metres suffice.** The relief of order 1741 normalises the shade
by local relief, so what matters is the height error against the
smallest relief the desert shows: Chegaga's linear dunes stand 20 to
80 m, Erg Chebbi's z12 tiles span 692 to 904 m. The vertical error
budget, per height:

| source of error | size |
|---|---|
| GLO-30 itself, absolute, against the reference (from the cell's own XML, `absolutePositionalAccuracyLE90`, cell N31 W005) | 1.76 m LE90, about 1.07 m as a standard deviation |
| rounding to whole metres | uniform within ±0.5 m, 0.29 m standard deviation |
| both together | about 1.11 m standard deviation: the rounding adds 4 % |
| the derived zooms | a mean of 4 to 64 stored heights, rounded once more: ±0.5 m |

So the rounding error is a quarter of the source's own, and 2.5 % of the
lowest dune. Its one visible trace: on ground flatter than one metre
per z12 pixel (about 33 m at 30° N, so slopes under 1.7°) the stored
surface steps in one-metre terraces; the source's own noise on such
ground is the same size, and the shading's smallest scale has to blur
over a few pixels in either case (1741 decides how). Decimetres would
cost 2.43 times the shade tiles' bytes instead of 1.16 (pilot) for an
accuracy the source does not have.

**Versioning and refusals.** Both piece kinds carry a magic (`LMHD`,
`LMHB`), a `u16` version (1) and their header length. A reader refuses
a version it does not know, rather than guess; a later version that
changes the encoding gets a new number, and a server that ships it
keeps the old pack until devices read the new one. Before allocating
anything a length field sizes, the reader checks that the lengths add
up to exactly the piece's own length, that the zooms and the tile edge
are ones it reads, that the tile count is the zooms' square and that
the block lies on the block zoom's grid; a hostile length is refused
with nothing allocated (`elevation_alloc.rs` counts the bytes). A tile
frame is refused when it does not decode to exactly one tile, carries
no content checksum or a wrong one; the zstd window is capped at one
tile and the decoder drains one tile's worth of blocks at a time, so a
frame of endless RLE blocks is refused within a megabyte (measured: the
uncapped decoder reached 200 MB on the test's frame).

**The cut.** `leviculum-map-tool heights --bbox w,s,e,n -o pack.lmh`
reads Copernicus GLO-30 from the public bucket (or `--source <dir>`).
It walks the blocks a row at a time, north to south; for each row it
fetches the 1° source cells the row needs (with the bilinear's reach
one source pixel past the row's south and east edges) into memory,
decodes them, drops the cells the row no longer needs, and writes each
block as it is done. Nothing touches the disk but the output: the peak
disk is the pack, the peak memory the cells of one block row (ten
cells, 631 MB resident, on the pilot box). Before it writes a byte the
cut probes every cell the box needs: the bucket has no cells over open
sea, but it also withholds some land cells, so an absent cell fails the
cut, every absent cell named, unless `--absent-is-sea` takes them as
0 m; the `MAPTOOL heights` line then names them (`absent_cells=`).
No-data pixels inside a cell are written as 0 m and counted
(`nodata_pixels=`). A cell whose header claims more than 3600 pixels
a side or tiles over 1024 (GLO-30's largest) is refused by name. Pixel
centres come from `leviculum_map::mercator`, the projection the map
draws with. The output depends only on
the source bytes, the box and the zooms; the tests cut twice and with
one and three threads and compare bytes.

**Attribution.** Taken from the licence that ships beside every cell
(`INFO/eula_F.pdf`): the attribution is Article 6 (b)'s notice for
adapted data, "produced using Copernicus WorldDEM-30 © DLR e.V.
2010-2014 and © Airbus Defence and Space GmbH 2014-2018 provided under
COPERNICUS by the European Union and ESA; all rights reserved"; the
notice names the licence and carries Article 6 (c)'s disclaimer word
for word. Both are in the directory piece and in the pack's
attribution piece.

**Measured on the pilot box** (29.3 to 31.5 N, 7.6 to 3.7 W, 2026-11-12,
this host, release build):

| | |
|---|---|
| blocks, tiles | 30 z9 blocks, 1,920 z12 tiles (they reach past the box) |
| pack | 37,063,251 bytes over 136,583 km² of blocks: 271 bytes per km² |
| the 1,350 z12 tiles that meet the box | 26,596,337 bytes (the pilot: 26.0 MB) |
| source | 20 GLO-30 cells fetched into memory (the largest 42.9 MB), none absent |
| cut | 118 s wall, 49 s user; zstd-19 encode 22 ms per tile (43 s summed) |
| host decode, z12 | 1.74 ms per tile |
| host derive, z11 / z10 / z9 | 7.1 / 28 / 111 ms per tile (4, 16, 64 tiles decoded) |
| working set to decode | 643,016 bytes peak on the heap: three tile buffers (384 KiB) plus the zstd decoder (about 244 KiB), pinned under 684 KiB by `elevation_alloc.rs` |

Not measured yet: the decode in the Android facade and on the M9 (no
device on the cutting host). What the M9 must hold is the working set
above, so it reads heights from PSRAM, not internal RAM; the flash cost
is `ruzstd`'s decoder.

## Height packs on the device

Order 1863 (Lew's relief in the Moroccan desert by mid December 2026).
Until then a pack reached the maps directory only by hand. Now a
height pack reaches it the way a region does: by HTTP range requests
from a static server, after the region it lies under.

**One block per device file, a fixed grid (decided).** The device
fetches the blocks of the pack's own grid (z9, 8 x 8 z12 tiles, about
68 by 78 km at 30 N), not one pack per region box. A block is the
smallest piece a pack already stands alone in, so it needs no new
format; the device's boxes follow the view and overlap, and a fixed
grid fetches each block once however many boxes lie over it, where one
pack per box would fetch the same heights again for every neighbour.
The cost is the block's size even for a small box: one block, about
1.2 MB, for a 12 km place (below).

**The catalogue.** The server keeps the packs as `leviculum-map-tool
heights` cuts them, one file per area, and publishes a catalogue
beside them: `leviculum-map-tool heights-catalogue -o heights.tsv
morocco.lmh [more.lmh ...]` writes, for every block of every pack, the
pack file, the block's zoom and coordinates, its byte offset and length
in the file, and its bounds in degrees x 10^7. The format is the module
doc of `leviculum-mapcut/src/heights.rs` and is held by
`leviculum-mapcut/tests/heights.rs`: a first line naming the format
and its version, `pack` and `block` lines, other lines skipped so a
later version can add kinds. A reader refuses a pack name that is not
one plain path segment, a block outside its pack's bytes or its zoom's
grid, and a version it does not know. It also refuses a block longer
than `BLOCK_MAX` (16 MiB, nine times the pilot's largest block of
1.81 MB below), a directory piece longer than `DIRECTORY_MAX` (1 MiB)
and a pack longer than such a directory can list: a device reads a
block into memory, and a catalogue may come from any server the user
names (audit 1863, round 2). The loader holds the same limits before
each range request. The packs must lie beside the
catalogue; the tool refuses a pack elsewhere.

**The device.** `loquax/src/mapload.rs`, the one loader of the window,
the terminal and the phone: a region that landed queues its packs, and
so, once a session and after them, does every autoloaded region on disk,
the most recently viewed first, from the view's first decision on (the
frontends schedule only with the autoload on, so with it off nothing is
fetched by itself); the
loader reads the catalogue once a session, finds the blocks under the
region's box, reads the pack's directory piece (once per pack file and
fetch) and each block by one range request, decodes every tile of the
block once (a frame whose checksum disagrees is refused there), and
writes `auto-h<zoom>-<x>-<y>.lmh` beside the region: a directory naming
the one block, the block's bounds as its box, the pack's attribution
and notice, then the block piece as served. It is written under a
`.part` name, synced and renamed, so a painter never indexes half of
it. A block already on disk is not fetched again. The packs queue
behind the regions: one fetch at a time, none while a region for the
screen loads or waits to; a failure retries on the long ladder (30 s
doubling to an hour); a catalogue the server does not have (403, 404,
410, or not a catalogue) is asked for again on the same ladder, once a
rung for every pack waiting, not given up for the session (decided
2026-11-15, audit 1863): the default source's catalogue is deployed
after the first regions load, and a phone runs for days, so a session
that gave up would keep every region it loaded meanwhile without relief
until it restarted. At an hour a rung the cost is one small request an
hour, and a missing catalogue wakes no frontend: the packs start at
the first poll after the rung, which any frame brings, so a still phone
is not woken for a server that may never have one. Offline, or with no
catalogue, the map draws as before without
relief, and no line is shown: the relief is a finish. Two writers evict
from the maps directory (a region's load and the height fetch); a file
one finds gone that the other removed counts as removed.

**The cache.** An autoloaded pack counts in `map_cache_mb` beside the
regions and goes with the last autoloaded region over its block
(`evict`); the Maps page's usage counts its bytes and "Delete all"
takes it.

**The source.** `height_source` in the config names the catalogue's
URL; unset, it is `heights.tsv` beside a configured `map_source` (a
server of its own serves both from one directory), and with neither
set the project's server, `https://leviculum.network/maps/heights.tsv`
(`HEIGHTS_DEFAULT`). Any HTTP server that answers range requests
serves it, as for the PMTiles archive.

**Runbook (workorse).** The cut needs the internet (the GLO-30
bucket) and about 650 MB of memory per block row; the pilot's Morocco
pack of order 1740 is that cut already:

```sh
leviculum-map-tool heights -o morocco.lmh --bbox=-7.6,29.3,-3.7,31.5
leviculum-map-tool heights-catalogue -o heights.tsv morocco.lmh
# both into the directory nginx serves as https://leviculum.network/maps/
```

A wider area is one more pack cut beside it and the catalogue written
again over all packs. The catalogue's 30 lines for Morocco are 2,578
bytes; a world of z9 blocks would be about 80,000 lines, 8 MB, read
once a session, which the reader caps at 16 MB.

**Measured** (2026-11-15, `loquax/examples/heights_walk.rs`, the
autoload's 12 km place at z10..z15 from a 2026-11-14 Protomaps extract
and the pilot's pack, both from a local range server held to 200 kB/s,
this host, release build):

| | Erg Chebbi | Bani escarpment |
|---|---|---|
| region file | 162,171 bytes, 3.7 s | 41,916 bytes, 2.9 s |
| height pack (one z9 block) | 1,041,903 bytes, 5.5 s | 1,067,723 bytes, 5.6 s |
| one 256 px tile at z13, relief on, cold / warm | 548 / 2.6 ms | 595 / 2.6 ms |
| the same tile, relief off | 0.3 / 0.2 ms | 0.2 / 0.2 ms |

The pilot's 30 blocks are 0.66 to 1.81 MB, 1.24 MB on average: a place
costs one block, a box on a block corner up to four (about 5 MB, 25 s at
200 kB/s). The cold tile is the shade plane's computation, which the
relief store then holds for every tile over it.

## Ground packs

Order 1743, stage A of the Sahara ground map (section 12 of
[Loquax as expedition map](loquax-expedition-map.md); Lew 2026-11-12:
ground classes from radar coherence and backscatter, imagery and
elevation, hamada a cool grey with a stone signature, a pastel and a
strong style, both shipped). The byte layout is the height pack's
container with its own magics, `LMGD` and `LMGB`
(`leviculum_map::elevation::PackKind::Ground`, module doc of
`leviculum-map/src/ground.rs`); a height reader refuses a ground pack by
its first four bytes and the reverse. A tile is 256 x 256 class bytes
(`leviculum_map::ground::Class`, 0 outside the cut) in one zstd frame
with its checksum, at **z11** (66 m at 30 N; the radar is 90 m), in the
height packs' z9 blocks of 4 x 4 tiles. z10 and z9 are derived on the
device, each cell the class most of the cells under it hold, ties to the
lower byte, the outside only where nothing else is.

**Inputs**, the three order 1742 cleared and nothing else (its draft,
`docs/drafts/africa-ground-map-licences.md`): Kellndorfer et al.'s
seasonal Sentinel-1 coherence (median of the four seasons' 12-day
coherence) and summer VV backscatter, GeoMAD 2024 (albedo from blue,
green and red, NDVI from near infrared and red, read at the 40 m
overview), and GLO-30 through a height pack the height cut already
wrote, so the ground and the relief see the same heights. Water
Observations from Space is not used: water is OpenStreetMap's, so open
water is no class and a dry lake is found from flatness and paleness
alone. The attribution and the notice name every input and its licence
and say the classes describe ground, never whether it can be crossed.

**The cut.** `leviculum-map-tool ground -o pack.lmg --bbox w,s,e,n
--heights pack.lmh [--source dir]` walks the blocks north to south; each
block is computed over a window reaching 512 z11 pixels (34 km) past it,
so filters, flow accumulation, the dry lakes' size and the
generalisation see the ground around it; only the block is written.
Features per pixel: coherence, VV, albedo, NDVI, the steepest z12 slope
under it, the height deviation within about 1 km, the height above the
5 km mean, and D8 flow accumulation over a priority-flood filled
surface. Catchments are counted inside the window only. The rules are
the pilot's (`leviculum-map-tool/src/ground/rules.rs`, each with its
reason), with one change, the **erg guard**: steep ground inside an erg
(dune sand on more than 35 % of the 1.5 km around) stays dune sand. The
pilot's mountain rule ((relief within 1 km over 25 m or slope over 18°)
and coherence at least 0.45) also fired on the slip faces of Erg
Chebbi's star dunes and painted grey specks there; `--pilot-rules` cuts
without the guard for comparison. Generalisation as in the pilot: a 5 x 5
mode filter, then patches under 12 pixels (about 5 ha) merged into the
nearest kept class, wadis exempt. The output depends only on the input
bytes, the box, the rules and the halo; a test cuts with one and three
threads and compares bytes.

**Validation**, a repeatable command: `leviculum-map-tool
ground-validate pack.lmg --heights pack.lmh --tracks <protomaps> --bbox
...` counts where OpenStreetMap's tracks (`kind_detail=track`, read at
z13) run per class and per slope against how much ground each holds, in
both halves of the box, and the dry lake against OpenStreetMap's water
around Lac Iriki. Measured on the pilot box (29.3 to 31.5 N, 7.6 to
3.7 W, 2026-11-15, Protomaps 2026-11-14, 17,701 km of tracks; the pilot
counted 17,629 km and dropped tracks near towns, this command does not):

| class | track share | area share | ratio | pilot ratio |
|---|---|---|---|---|
| gravel plain (reg) | 48.1 % | 31.6 % | 1.52 | 1.54 |
| sand sheet | 8.1 % | 5.4 % | 1.50 | 1.41 |
| wadi bed | 1.4 % | 0.9 % | 1.53 | 1.40 |
| vegetation, oasis | 8.6 % | 2.9 % | 2.98 | 2.78 |
| dry lake | 0.3 % | 0.3 % | 1.24 | 0.92 |
| mountain | 16.1 % | 26.0 % | 0.62 | 0.73 |
| uncertain | 9.0 % | 14.2 % | 0.64 | 0.57 |
| dune sand | 0.8 % | 1.7 % | 0.46 | 0.50 |
| hamada | 7.6 % | 17.0 % | 0.44 | 0.41 |

Slopes, tracks against area: 0-2° 1.23, 2-5° 1.15, 5-10° 1.06, 10-20°
0.65, above 20° 0.32 (pilot 0.40). Halves (west of 5.65 W / east): reg
1.32 / 1.82, dune 0.50 / 0.40, hamada **0.79 / 0.36**. The class shares
of the box are the pilot's within 0.3 points except the dry lake (0.3 %
against 0.8 %, the water input gone).

**Known weaknesses, kept visible.** Hamada against reg: the pilot
relabelled it from the very tracks this validation counts, so the split
is partly circular, and here it holds in the east half (0.36) but much
less in the west (0.79). The dry lake: without the water input the class
finds 65 % of OpenStreetMap's Lac Iriki (the pilot 94 %) and covers 1.69
times its area in the window (precision 0.38; the pilot's was three
times as wide). The erg guard changes no ratio above (the generalisation
removes most specks already); what it changes is the Erg Chebbi core: 17
mountain pixels (0.1 %) with the pilot's rules, none with the guard.

**Drawing** (`leviculum_map::ground::paint_row`): each display pixel
takes its class colour blended bilinearly between the four samples
around it (soft borders), then the signature of the nearest class,
anchored to the global pixel grid of the display zoom so tiles join
(tested). The painter lays the ground after the land fill and before
every other area, so OpenStreetMap's water, towns, fields and sand lie
over it, and the relief shades both. The uncertain class and ground
outside every pack stay the map's paper; above z9 nothing is drawn; the
dark table draws none. Every colour, signature ink and the relief's
shadow floor are in one table, `leviculum-map/src/ground_style.rs`,
with the trial's final values (`mapout-r2/README_R2.md`); a test fails
when a class has no row. The style is the config's `map_ground`
(`pastel`, the default, `strong`, or `off`). The cost, measured on a
1224 x 2197 frame over Erg Chebbi (release, this host): 18 ms at z12 and
20 to 22 ms at z14 on top of 3 to 5 ms without, about 7 ns a pixel,
paid once per tile by the frontends' tile caches.

**The legend** names ground kinds only (`Class::name`: dune sand, sand
sheet, rock plateau, gravel plain, mountain, dry lake, wadi bed,
vegetation, not classified); a test holds that no legend text says
passable, drivable or the like. Its source line
(`leviculum_map::ground::SOURCE_LINE`) also says the relief is GLO-30's
30 m and soft from zoom 14 on: it is not sharpened, the detail is not in
the data.

**Sizes.** The pilot box: 30 blocks, 480 tiles, **888,671 bytes** at
zstd level 19, over 136,583 km² of blocks (6.5 bytes per km²; the pilot
estimated 3.4 MB for z9 to z12 as WebP, which the derived zooms make
unnecessary). Extrapolated at the same density: Morocco with Western
Sahara (about 713,000 km²) about 4.6 MB, the Sahara (about 9.2 million
km², about 2,000 blocks) about 60 MB, against the pilot's estimate of
350 MB. The cut on this host, inputs local: 340 s wall for the box (11 s
a block, four threads, 3.5 GB resident), about six hours for the
Sahara; its reads, 218 MB of radar cells and 944 MB of GeoMAD overviews
for the box with its halo, scaled by block area (67 times the box) give
about 15 GB and 63 GB for the Sahara, an upper bound, since the box's
reads include the halo past its edge.

**Not in stage A:** a loader and catalogue for ground packs (they reach
the maps directory by hand, as the height packs did before 1863), the
phone's and the M9's style switch (the phone draws pastel, the M9
none), a legend on screen and the credit screen's lines, barrier lines
(their ink is undecided; the pilot's dark red read as roads) and
interdune corridors (90 m is too coarse for them, the pilot measured).

## What this page does not decide

Deployment (K5 on workorse: nginx in front, the tunnel mechanism,
systemd units), the Loquax client ladder's move from whole-region
fetches to pieces (it keeps its probe, thresholds and questions),
the shading itself (order 1741 draws from the height packs above), and periculum cells for the served
store (the S1 cells of [Loquax: map data over
Reticulum](loquax-map-over-reticulum.md) apply to this protocol and
come with the ladder work).

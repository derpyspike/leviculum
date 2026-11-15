# Loquax: one messenger on every target

Lew's product rule (2026-10-25, restated 2026-11-02 21:40): the full
messenger on the phone, the Linux window, the terminal and the embedded
screen (the ThinkNode M9 today), from one core. Every feature moves its
platform layer on all of them where that is technically possible. This
page is the measured state of that rule, feature by feature (1341).

## How the table was measured

Each row is a user-facing feature of the phone build, found by walking the
Android facade's enums `Request` (`loquax-android/src/schema.rs:272`)
and `Query` (`loquax-android/src/schema.rs:742`),
the bridge methods Java calls (`Core.java`), the activities that call them,
and the scene's node kinds (`loquax/src/ui/scene.rs`). For every target a
cell names the place that does it, `file:line`, or says MISSING. A MISSING
is a grep that found nothing, with a positive control beside it: for the
terminal's keys and palette, `Reply`, `Forward`, `SendAgain`, `Mute`,
`Verify`, `MarkUnread` and `Share` match zero times in
`leviculum-ui/src/keymap.rs` and `loquax/src/keys.rs`, while `Copy`
matches 14 and 8 times there. For the window, a MISSING intent is one
that falls into the catch-all arm of `App::intent` (`loquax/src/gui.rs`)
and has no other handler in `loquax/src/gui.rs` or
`loquax/src/gui/chat.rs`.

Every `file:line` on this page names what its line holds, as
`` `name` (`file:line`) ``, and citation-guard holds this page to that
form: the name must stand on the cited line itself, and a citation that
names nothing is refused (`EXACT_PAGES` in
`leviculum-std/tests/doc_citations.rs`, 1850). Until then most of the
table was bare, and 51 of its 168 citations pointed away from the
line that holds what their row is about, row 41 among them: a merge
that rewrites a row re-baselines the guard's line anchors.

Columns:

- **Scene**: the decision lives in the shared core (`loquax::ui`, the
  scene, the intents); yes for every row, which is the point of the core.
- **Android**: the Java call site of the bridge method, under
  `loquax-android/android/src/network/leviculum/loquax/`.
- **Window**: `loquax gui` (iced), under `loquax/src/`; plain `loquax`
  on a desktop since 1342.
- **Terminal**: `loquax tui` (ratatui), under `loquax/src/`, and plain
  `loquax` where there is no display; `ui.rs` is the model's palette and
  keys, which only the terminal reads.

Window and terminal are one binary in the amd64 and arm64 packages
(1342): built against glibc 2.31 with both features, since winit opens
X11 and Wayland by dlopen, which a musl-static binary cannot. Plain
`loquax` picks by `WAYLAND_DISPLAY` and `DISPLAY` (`loquax/src/front.rs`);
the armhf package and the static tarballs stay musl with the terminal
alone. The amd64 package carries the window's binary a second time,
built for x86-64-v3, as `/usr/lib/loquax/loquax-v3`, and the baseline
binary hands itself over to it on a CPU that runs it
(`loquax/src/cpu.rs`, 1469): tiny-skia picks its wide paths at compile
time, and 1442 measured the map's wheel 23 to 29 % faster with them.
The static tarballs stay baseline; `LOQUAX_CPU=baseline` keeps the
baseline binary on a v3 CPU.
- **M9**: see "The M9" below; the firmware has no messenger screen yet.

"CLI" means the target has it as a `loquax` subcommand
(`loquax/src/main.rs`), not on its screen. "n/a" means the target lacks
the hardware, which is no gap.

## The table

Rows marked (1341) or (1371) were MISSING before that pass and are closed by it.

| # | Feature | Android | Window | Terminal |
|---|---------|---------|--------|----------|
| 1 | Chats: list, open, send text | `Core.send` (`ChatActivity.java:2206`) | `Intent::Send` (`loquax/src/gui.rs:1729`) | `PaletteCommand::Send` (`loquax/src/ui.rs:4543`) |
| 2 | Drafts | `Core.draft` (`ChatActivity.java:2941`) | `tell_draft` (`loquax/src/gui.rs:1432`) | `set_draft` (`loquax/src/ui.rs:1787`) |
| 3 | React | `Core.react` (`ChatActivity.java:2331`) | `Intent::React` (`loquax/src/gui.rs:1854`) | `PaletteCommand::React` (`loquax/src/ui.rs:6080`) (palette `react`) |
| 4 | Reply | `Core.reply` (`ChatActivity.java:2206`) | `Intent::Reply` (`loquax/src/gui.rs:1868`) | `PaletteCommand::Reply` (`loquax/src/ui.rs:6007`) (palette `reply`) (1371) |
| 5 | Forward | `Core.forward` (`ChatActivity.java:2464`) | `Message::ForwardPick` (`loquax/src/gui/chat.rs:2489`), `Intent::Forward` (`loquax/src/gui.rs:1812`) (1371) | `PaletteCommand::Forward` (`loquax/src/ui.rs:6009`) (palette `forward`) (1371) |
| 6 | Send again | `Core.sendAgain` (`ChatActivity.java:3074`) | `Intent::SendAgain` (`loquax/src/gui.rs:1914`) | `PaletteCommand::SendAgain` (`loquax/src/ui.rs:6008`) (palette `send again`) (1371) |
| 7 | Delete a message | `Core.delete` (`ChatActivity.java:2554`) | `Intent::Delete` (`loquax/src/gui.rs:1925`) | `PaletteCommand::DeleteMessage` (`loquax/src/ui.rs:5997`) |
| 8 | Copy a message | `setPrimaryClip` (`ChatActivity.java:2491`) | `Intent::Copy` (`loquax/src/gui.rs:1904`) | `Action::Copy` (`loquax/src/ui.rs:4751`) (OSC 52) |
| 9 | Search | `searching` (`ConversationsActivity.java:755`) | `Intent::Search` (`loquax/src/gui.rs:1719`) | `PaletteCommand::Search` (`loquax/src/ui.rs:5993`) |
| 10 | Read receipts on/off | `Core.receipts` (`SettingsActivity.java:871`) | `Intent::ReadReceipts` (`loquax/src/gui.rs:1685`) | `PaletteCommand::ReadReceipts` (`loquax/src/ui.rs:5971`) |
| 11 | Send a picture (Gallery) | `Core.photo` (`ChatActivity.java:1662`) | `Intent::SendPhoto` (`loquax/src/gui.rs:1592`), `pick` (`loquax/src/desktop.rs:158`) (1341) | `PaletteCommand::SendPicture` (`loquax/src/ui.rs:6036`) (palette `send picture`, a typed path), `UiEffect::Attach` (`loquax/src/shell.rs:435`) (1371) |
| 12 | Photo size, three tiers | `Core.photoSize` (`SettingsActivity.java:541`) | `photo_rows` (`loquax/src/gui/chat.rs:1377`) (settings) (1371) | `PaletteCommand::PhotoSize` (`loquax/src/ui.rs:6022`) (palette `photo size`) (1371) |
| 13 | View a photo | `PhotoActivity` (`ChatActivity.java:2015`) | `Intent::ViewPhoto` (`loquax/src/gui.rs:1915`) (own viewer) | `view_photo` (`loquax/src/ui.rs:4792`), `UiEffect::ShowPhoto` (`loquax/src/shell.rs:465`) (system viewer) |
| 14 | Camera | `Core.stage` (`ChatActivity.java:1215`) | n/a | n/a |
| 15 | Send a file | `Core.file` (`ChatActivity.java:1715`) | `Intent::SendFile` (`loquax/src/gui.rs:1595`), `Finished::Send` (`loquax/src/shell.rs:892`) (1341) | `PaletteCommand::SendFile` (`loquax/src/ui.rs:6040`) (palette `send file`, a typed path), `UiEffect::Attach` (`loquax/src/shell.rs:435`) (1371) |
| 16 | Open and save a received file | `Core.saveFile` (`ChatActivity.java:1915`) | `Intent::OpenFile` (`loquax/src/gui.rs:1916`), `UiEffect::OpenFile` (`loquax/src/shell.rs:483`) | `open_file` (`loquax/src/ui.rs:4801`), `UiEffect::OpenFile` (`loquax/src/shell.rs:483`) |
| 17 | Record a voice message | `input` (`RecordingBar.java:368`) | `Message::RecPress` (`loquax/src/gui/chat.rs:2657`), `UiEffect::Recording` (`loquax/src/shell.rs:570`), `mic.rs` | `record_key` (`loquax/src/ui.rs:5333`), `UiEffect::Recording` (`loquax/src/shell.rs:570`), `mic.rs` |
| 18 | Play a voice message | `Core.voicePcm` (`ChatActivity.java:1864`) | `Intent::PlayVoice` (`loquax/src/gui.rs:1919`), `UiEffect::PlayVoice` (`loquax/src/shell.rs:502`) | `play_voice` (`loquax/src/ui.rs:4810`), `UiEffect::PlayVoice` (`loquax/src/shell.rs:502`) |
| 19 | Groups: new, join, members, add, remove, leave, delete, start again | `Core.group` (`GroupDialogs.java:51`) | `Intent::Group` (`loquax/src/gui.rs:1678`) (all verbs) | `PaletteCommand::NewGroup` (`loquax/src/ui.rs:6107`) to `PaletteCommand::StartAgain` (`loquax/src/ui.rs:6189`) |
| 20 | Map with positions | `Core.friends` (`MapPane.java:456`) | `Intent::ShowMap` (`loquax/src/gui.rs:1758`) | `PaletteCommand::Map` (`loquax/src/ui.rs:6057`), `tui/map.rs` |
| 21 | Buildings on the map | `draws_buildings` (`loquax/src/picture.rs:1145`) (shared) | `draws_buildings` (`loquax/src/picture.rs:1145`) | `draws_buildings` (`loquax/src/picture.rs:1145`) (sixel, kitty) |
| 22 | Load the map area | `prefetch` (`MapView.java:1290`) (autoload) | `Intent::LoadMapArea` (`loquax/src/gui.rs:1943`) | `PaletteCommand::LoadMapArea` (`loquax/src/ui.rs:6053`) |
| 23 | Delete all downloaded maps | `Core.deleteRegions` (`SettingsActivity.java:791`) | `Intent::DeleteAllRegions` (`loquax/src/gui.rs:1710`) | `PaletteCommand::DeleteAllMaps` (`loquax/src/ui.rs:6032`) (palette `delete all maps`), `UiEffect::DeleteAllMaps` (`loquax/src/shell.rs:453`) (1371) |
| 24 | Mesh positions (nodes nearby) | `nearby` (`MapPane.java:498`) | `Message::AddNearby` (`loquax/src/gui.rs:2346`) | `nearby` (`loquax/src/tui/map.rs:1598`) |
| 25 | Share the position, once or timed | `Core.shareFix` (`ChatActivity.java:1106`), `ShareService` (`ShareService.java:37`) | `Intent::EnterPosition` (`loquax/src/gui.rs:1544`), `Intent::FixAtMapCentre` (`loquax/src/gui.rs:1552`), position by hand `parse` (`loquax/src/ui/by_hand.rs:27`) (1371) | `PaletteCommand::Position` (`loquax/src/ui.rs:6023`) (palette `position`), `PaletteCommand::SharePosition` (`loquax/src/ui.rs:6027`) (`share position`, `stop sharing`) (1371) |
| 26 | Pin | `Core.pin` (`ConversationsActivity.java:730`) | row menu `row_actions` (`loquax/src/gui/chat.rs:2354`), `Intent::Pin` (`loquax/src/gui.rs:1839`) (1371) | `PaletteCommand::Pin` (`loquax/src/ui.rs:6005`) |
| 27 | Mute | `Core.mute` (`ContactActivity.java:540`) | row menu (1371) | `PaletteCommand::Mute` (`loquax/src/ui.rs:6013`) (palette `mute`) (1371) |
| 28 | Archive | `Core.archive` (`ConversationsActivity.java:740`) | row menu (1371) | `PaletteCommand::Archive` (`loquax/src/ui.rs:6048`) |
| 29 | Block | `Core.block` (`ContactActivity.java:447`) | row menu (1371) | `PaletteCommand::Block` (`loquax/src/ui.rs:6084`) |
| 30 | Verify | `Core.verify` (`ContactActivity.java:348`) | row menu (1371) | `PaletteCommand::Verify` (`loquax/src/ui.rs:6014`) (palette `verify`) (1371) |
| 31 | Mark unread | `Core.unread` (`ConversationsActivity.java:723`) | row menu (1371) | `PaletteCommand::MarkUnread` (`loquax/src/ui.rs:6018`) (palette `mark unread`) (1371) |
| 32 | Rename | `Core.rename` (`ContactActivity.java:610`) | row menu, its field (1371) | `PaletteCommand::Rename` (`loquax/src/ui.rs:6076`) |
| 33 | Forget a conversation | `Core.forget` (`ContactActivity.java:489`) | row menu, asked once (1371) | `PaletteCommand::DeleteConversation` (`loquax/src/ui.rs:6001`) (palette `delete conversation`, asked once) |
| 34 | Add a contact by code, typed or pasted | `Core.addContact` (`AddContact.java:96`) | `Message::NewGo` (`loquax/src/gui.rs:1401`) | `PaletteCommand::AddContact` (`loquax/src/ui.rs:6101`) |
| 35 | A contact code from a QR | `Java_network_leviculum_loquax_Native_scan` (`loquax-android/src/lib.rs:391`) (the scanner screen's decoder) | `Message::PickCode` (`loquax/src/gui.rs:2363`), `Message::PickCode` (`loquax/src/gui/chat.rs:1479`), `image_codes` (`loquax/src/scan.rs:39`) (1341) | `PaletteCommand::CodeFromPicture` (`loquax/src/ui.rs:6044`) (palette `code in a picture`, a typed path), `UiEffect::ScanCode` (`loquax/src/shell.rs:438`) (1371) |
| 36 | My code | `EXTRA_TEXT` (`ProfileActivity.java:285`) | `Intent::ShareCode` (`loquax/src/gui.rs:1612`), `profile_layer` (`loquax/src/gui.rs:3139`) (settings row, QR) (1371) | `PaletteCommand::MyCode` (`loquax/src/ui.rs:6206`) |
| 37 | Home setup | `Core.startHere` (`WelcomeActivity.java:589`) | CLI (`Setup` (`loquax/src/main.rs:211`)) | CLI (`Setup` (`loquax/src/main.rs:211`)) |
| 38 | Link a device | `Core.linkDevice` (`ProfileActivity.java:385`) | CLI (`Join` (`loquax/src/main.rs:241`), `Admit` (`loquax/src/main.rs:225`)) | CLI (`Join` (`loquax/src/main.rs:241`), `Admit` (`loquax/src/main.rs:225`)) |
| 39 | Move the phone | `Core.movePhone` (`ProfileActivity.java:433`) | MISSING | MISSING |
| 40 | Backup | `Core.backup` (`BackupActivity.java:130`) | MISSING | MISSING |
| 41 | Help | `R.string.help` (`ConversationsActivity.java:662`) | `Intent::Help` (`loquax/src/gui.rs:1600`), `help_layer` (`loquax/src/gui.rs:3261`) (the shared overlay) (1371) | `PaletteCommand::Help` (`loquax/src/ui.rs:5939`) (palette `help`) |
| 42 | Settings, nine sections in one order (1450) | `entries` (`SettingsActivity.java:191`) | `settings_pane` (`loquax/src/gui/chat.rs:444`) (gear) | `settings_view` (`loquax/src/ui.rs:8389`) (`S`, palette `settings`) |
| 43 | Browse tab | `Core.browse` (`BrowsePane.java:499`) (behind `browse_tab`) | `browse_view` (`loquax/src/gui.rs:3286`), behind `browse_tab` (1369) | `draw_screen` (`loquax/src/tui.rs:1714`), behind `browse_tab` (1369) |
| 44 | Notifications of new messages | `notify` (`LoquaxService.java:342`) | `desktop::post` (`loquax/src/gui.rs:457`), `post` (`loquax/src/desktop.rs:118`) (1341) | `notify` (`loquax/src/tui.rs:743`), bell and `post` (`loquax/src/desktop.rs:118`) (1341) |
| 45 | Icon and application id | `LoquaxApp` (`loquax-android/android/AndroidManifest.xml:70`) | `application_id` (`loquax/src/gui.rs:598`) (1341) | n/a |
| 46 | Rooms tab (RRC), a room as a chat (1450) | `Core.rooms` (`RoomsPane.java:261`), behind `rooms_hub` (1384) | `rooms_pane` (`loquax/src/gui/chat.rs:751`), behind `rooms_hub` (1379) | `draw_rooms` (`loquax/src/tui.rs:1744`), behind `rooms_hub` (1379) |
| 47 | Explore: pages and public rooms under one button, the bar's third (1578) | `Core.explore` (`ExplorePane.java:681`), `loquax-android/src/explore.rs` | `explore_pane` (`loquax/src/gui/chat.rs:913`) | `draw_explore` (`loquax/src/tui.rs:1895`) (`E`, palette `explore`) |

## Counts

| | MISSING before 1341 | after 1341 | after 1371 |
|---|---|---|---|
| Window | 20 | 15 | 2 |
| Terminal | 15 | 14 | 2 |

Counted are the MISSING cells of the table; "CLI" and "n/a" are no gaps.
A cell marked (1371) names its place as of that pass; the unmarked ones
name it as 1341 measured it, and those lines have moved since.
The Browse tab (row 43) was off on both desktop targets until 1369 and was
not counted; since 1369 it is built on all three, behind the same
`browse_tab` switch, which is on by default since 1618: Explore is the
browser's way in, and `browse_tab = off` is the opt-out.
The Rooms tab (row 46, 1379) is Reticulum Relay Chat on the hub the
config's `rooms_hub` names; without one there is no tab. The model, the
scene's `RoomsNode` and the hub link (`loquax/src/rooms_link.rs`, over the
node the engine lends to `Lender::Rooms`) are shared; the window and the
terminal draw the node. The phone draws it since 1384: the schema's
`RoomsScreen` (`loquax-android/src/rooms.rs`) flattens the node, the
session lends the phone's node (the mailbox's when it is on the phone,
else the leaf's) to the Rooms and runs the same hub link, and
`RoomsPane.java` draws it beside `BrowsePane`, its tab shown only where
`rooms_hub` is set, as on the desktop. Since 1450 (walk 1, finding 20)
every target draws a room as a chat from the same node: bubbles, the
writer's avatar and name over the first of a run, the comings and goings
as quiet lines, the people seen under the room's name and behind it, and
"Leave room" in its menu; IRC's `/me`, `/part` and `/join` stay as
shortcuts no screen names. No target has a way to set `rooms_hub` but
the config file, so a phone set up by the app has no tab. The M9 needs a renderer screen for
the node in `loquax-m9`, rows in its key table, and the hub link over its
own node; what each takes is written down on [the handheld
page](handheld-messenger.md), "The Rooms on the M9".
Since 1578 the bar has three buttons on every target that has a bar,
Chats, Map and Explore, and no fourth (Lew, 2026-11-08): rows 43 and 46
are what Explore opens, not buttons of their own. Explore (row 47) is the
shared core's list (`loquax/src/ui/explore.rs`): the pages the home and
the device heard and the bookmarks, one row per destination, bookmarks
first, then last seen first under Today, This week and Older, A to Z a
toggle kept per device (`explore_az`); the rooms the reader is in, the
first ten shipped hubs and the hubs heard on the mesh. Since 1618 (the
walk of 17024c3a) a row is a name alone, its short hash only where two
names look alike, "Unnamed page" never a hash; the search finds names and
a pasted address, never a hash's letters, and filters the hubs too; a new
order is applied at most once a second and never while a finger is on the
list; the reader's word for a hub is "server", and a tap on one opens its
sheet (how far, the rooms it named, one button that makes it the public
rooms' server) and changes nothing. Since 1652 a hub used there links at
once (the privacy sentence of 1577 with it), sends `/list` after its
WELCOME and shows the rooms it names as rows a tap joins, the typed name
below them; its state says "Not connected yet", "Connecting…" with the
seconds after five, or "The server does not answer" with "Try again".
The tab never
disappears; with nothing to list each section says so in one sentence.
With `browse_tab` off Explore lists the pages and says they cannot be
opened. The M9 has no third button: what it
would need is on [the handheld page](handheld-messenger.md), "Explore on
the M9".

Public channels carried over LXMF propagation, an earlier concept, are
superseded by these RRC rooms and by mailbox groups (Lew, 2026-11-09), so
a channel layer on top of propagation nodes is not to be built again.

## What 1341 brought, and how

- **Pictures and files in the window.** The window sets the core's
  `Abilities::picker` (`loquax/src/gui.rs:355`), so the scene gives it the plus and
  the attachment sheet with Gallery and File, which the window draws over
  the bar (`scene.sheet` (`loquax/src/gui/chat.rs:2557`)). A row asks the XDG desktop portal's
  `FileChooser.OpenFile` over the session bus (`pick` (`loquax/src/desktop.rs:158`)), the
  dialog the desktop draws for every program. The crate is zbus, already
  in the lock through the window's theme reader (leviculum-ui-iced, mundy);
  neither `rfd` nor another dialog crate is in the tree, and zbus needs no
  GTK. The shell sends the pick as the phone sends its picker's
  (`attach` (`loquax/src/shell.rs:1014`)): a picture scaled at the reader's photo size, a file
  whole up to what a receiver takes. Saving a received file is the
  existing save under the home's `files/` and the system's handler
  (`UiEffect::OpenFile` (`loquax/src/shell.rs:483`)), the same in both desktop targets.
- **Voice** was already on both desktop targets: recording through the
  system's recorder (`pw-record`, `parecord`, `arecord`, `mic.rs`),
  playback as a WAV file for the system's player (`UiEffect::PlayVoice` (`loquax/src/shell.rs:502`)). No
  audio crate was added.
- **QR without a camera.** The phone's decoder (rqrr) moved from
  `loquax-android` into `loquax::scan`, and `scan::image_codes` reads a
  PNG, JPEG, WebP or GIF of a code (`image_codes` (`loquax/src/scan.rs:39`)). The window's new
  conversation offers "Read a code from a picture"; pasting a code's text
  already worked in both desktop targets.
- **Notifications** follow the phone's rule (`notify` (`LoquaxService.java:342`)):
  a grown unread count, not muted, not at the first look, not while the
  screen is in front (`desktop.rs`, `Notices`). The window posts through
  `org.freedesktop.Notifications` while it lacks the focus; the terminal,
  which cannot tell whether it is in front, rings its bell for every chat
  but the open one and posts the same notification where a session bus
  exists.
- **The application id** is `loquax`, the desktop file's basename
  (`packaging/loquax/loquax.desktop`), so Wayland matches the window to
  the launcher entry and its icon. No asset moved.

## The position on the desktop

A desktop has no receiver, and that is no gap (1371): the core takes the
own fix by hand (`loquax/src/ui/by_hand.rs`). A shell that has no
receiver sets `Abilities::manual`, and its position sheet offers "Type my
position" (latitude and longitude, typed or pasted, `geo:` links too,
read digit by digit without a float) and, once the map was shown, "The
map's centre". From then on the sheet offers "Now, once" and the four
timed shares, and the fix goes out as the phone's GNSS fix does, the
same telemetry report. The sheet says "Set by hand" over them. The
terminal has the same through its palette: `position` asks for it
(Enter alone takes the map's centre), `share position` asks how long
(`now`, `15m`, `1h`, `8h`, `until`), `stop sharing` ends it. A chat's
report is telemetry, which carries no source; where the fix goes into a
`lev.position` announce it is `Source::Manual`
(`loquax/src/mesh_position.rs`).

## The Browse tab

Before 1369, 1337b's fetch through the phone's own node lived in the
Android crate and the desktop shell dropped the browser's effects. 1369
moved the fetch into the shared crate (`loquax/src/browse_fetch.rs`,
feature `browse`), which the phone, the terminal and the window now run
alike over the node each already has; the desktop shell drains the
browser's effects (`effect` (`loquax/src/browsing.rs:142`)) as the phone's session
does. The window draws the tab with lnomad's window view
(`view` (`lnomad/src/gui.rs:681`)), pictures included, and the terminal with
lnomad's terminal view (`draw_screen` (`loquax/src/tui.rs:1714`)); neither has a renderer
of its own. The switch is the phone's, `browse_tab`, on by default on
all three since 1618 (off until then). Details in
`lnomad-everywhere.md`, "The tab on the desktop".

## What 1371 brought, and how

- **The terminal's verbs.** Reply, send again, forward, mute, verify,
  mark unread, the photo size, the position, sharing it, deleting all
  maps, and a picture, a file or a code by a typed path are palette
  commands (`leviculum-ui/src/commands.rs`, `loquax/src/ui/verbs.rs`),
  each the handler the phone's request reaches. The terminal has no
  picker, so a path typed into the palette goes to the shell as
  `UiEffect::Attach` or `UiEffect::ScanCode`, read by the attach the
  window's chooser uses (`loquax/src/shell.rs`).
- **The window's row menu.** A right click on a conversation opens the
  scene's row verbs (mark unread, pin, mute, archive, forget) and the
  contact's the phone keeps on its contact screen (block, verify,
  rename). They reach the model as `UiEvent::PeerWord`, resolved by
  `UiModel::peer_verb`, the terminal's resolver too; forget asks once,
  rename opens a field.
- **The window's forward, help, photo size and code.** The bubble menu's
  Forward lists the conversations; the keys' help is the shared overlay
  lnomad's window draws; the settings page has the photo size's three
  tiers, My code (the code and a QR the test scans back) and Help.

## The remaining gaps, with their reason

- **Both desktop targets, rows 39 and 40 (move the phone, backup):** not
  built in 1371; both are possible, but each is a flow of its own (the
  phone's transfer by code with its guard against two live copies, and a
  backup file with its restore), and on a computer the identity lives in
  Loquax's home directory, which the system's backup already copies.

## The M9

The firmware has no messenger screen yet: the order of work
([The handheld messenger](handheld-messenger.md), "Order of work") puts
the M9 frontend at step 5, after the status screen and the core taken out
of `loquax`, and the tree holds no M9 binary today (`leviculum-esp/src/bin`
has the Heltec V4 alone). What the board can carry once it has one, from
[The ThinkNode M9](thinknode-m9.md): a 240x320 screen without touch and a
37-key keyboard, GNSS, a buzzer.

- Rows 1 to 10, 19, 26 to 34, 36, 41 and 42 (text, groups, the contact
  verbs, settings): the core's scene on the 240x320 screen, driven by the
  keyboard as the terminal is.
- Rows 11 to 15 and 35 (pictures, files, camera, QR): the board has no
  camera; a received photo can be drawn small, a file only listed.
- Rows 17 and 18 (voice): the board has no microphone and only a buzzer,
  so it neither records nor plays a voice message; it shows that one came.
  This is the one hardware reason; everything else is order of work.
- Rows 20 to 25 (map, positions, sharing): the GNSS gives an own fix, as
  the desktop's is typed by hand since 1371; the map is steps 6 and 7.
- Row 44 (notifications): the buzzer and the screen.
- Rows 37 to 40 (home setup, link, move, backup): through the phone or
  the CLI, not on the device.

## Related

- [Loquax: architecture](loquax-architecture.md)
- [Loquax: the user interface](loquax-ui.md)
- [Loquax on Android: navigation and views](loquax-android-navigation.md)
- [The handheld messenger](handheld-messenger.md)

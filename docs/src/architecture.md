# Architecture Overview

This is the entry point to the **Concepts** part of the manual. It
covers the sans-IO core, the crate split, the driver event loop, and
the platform-abstraction traits — the mechanics that the four concept
pages build on:

- [Interface Isolation](concepts/interface-isolation.md) — why only the
  interface knows its medium's quirks.
- [Python-RNS Compatibility](concepts/python-rns-compatibility.md) —
  wire/semantic compatibility and the drop-in daemon, vs. internal
  parity (not a goal).
- [Identity and Forward Secrecy](concepts/identity-and-forward-secrecy.md)
  — dual keypairs, derived destinations, ratchets.
- [Storage and Embedding](concepts/storage-and-embedding.md) — the
  `Clock`/`Storage`/`Interface` traits that let one core run on a host
  or a microcontroller.

## The crate split

The protocol logic lives in one `no_std` crate; everything platform-
specific wraps around it:

| Crate | Role |
|-------|------|
| `leviculum-core` | All protocol logic, `#![no_std] + alloc`, zero async (`leviculum-core/src/lib.rs:59`). |
| `leviculum-std` | Host driver: tokio event loop, interfaces, `FileStorage`, RPC, config. |
| `leviculum-nrf` | Embedded driver: Embassy event loop on nRF52 (cross-compiled, outside the host workspace). |
| `leviculum-ffi` | C ABI over the core for other-language bindings. |
| `leviculum-cli` | The `lnsd` / `lnstest` / `lncp` binaries. |

The application boundary is `NodeCore`: feed it bytes via
`handle_packet` / `handle_timeout` and drain a
`TickOutput { actions, events }`. The core decides *what* to send; the
driver decides *how and when* to put it on the wire. See
[Storage and Embedding](concepts/storage-and-embedding.md) for the
injected `Clock`/`Storage`/`Interface` traits that make this portable.

## Sans-I/O Core

```
                     ┌─────────────────────────────────┐
                     │         leviculum-core          │
                     │                                 │
  handle_packet() ──►│  NodeCore<R, C, S>              │──► TickOutput {
  (iface_id, data)   │    ├── Transport (routing)      │      actions: Vec<Action>,
                     │    ├── Links + Channels         │      events: Vec<NodeEvent>,
  handle_timeout() ─►│    └── Destinations             │    }
                     │                                 │
  next_deadline() ──►│  Returns: Option<u64>           │
                     └─────────────────────────────────┘

  Action::SendPacket { iface, data, peer }  — send to one interface,
                                             optionally naming the peer
                                             behind it the bytes are for
  Action::Broadcast { data, exclude }       — send to all interfaces (except one)
```

## Driver Event Loop

The `leviculum-std` driver has 6 `select!` branches:

```rust
loop {
    select! {
        // 1. Packet from any interface
        (iface_id, data) = registry.recv_any() => {
            output = core.handle_packet(iface_id, &data);
            post_dispatch(output);
        }
        // 2. External action (connect, send, announce)
        output = action_dispatch_rx.recv() => { post_dispatch(output); }
        // 3. Timer fires
        _ = sleep_until(next_poll) => {
            output = core.handle_timeout();
            post_dispatch(output);
        }
        // 4. Shutdown
        _ = shutdown.changed() => break
        // 5. New interface (TCP accept, local client connect)
        handle = new_interface_rx.recv() => {
            registry.register(handle);
            output = core.handle_interface_up(iface_idx);
            post_dispatch(output);
        }
        // 6. Periodic storage flush (crash protection, hourly)
        _ = sleep_until(next_flush) => { core.storage_mut().flush(); }
    }
}
```

### Post-dispatch (after every core call)

1. `dispatch_actions(&mut ifaces, &output.actions)` — routes Actions to interfaces (protocol logic in core)
2. React to errors — `BufferFull`: log. `Disconnected`: call `handle_interface_down()`
3. Forward `output.events` to the application
4. Schedule `handle_timeout()` from `output.next_deadline_ms`

## Interface Trait

```rust
pub trait Interface {
    fn id(&self) -> InterfaceId;
    fn name(&self) -> &str;
    fn mtu(&self) -> usize;
    fn is_online(&self) -> bool;
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError>;
}
```

Send-only. Receive is driver-specific (tokio: `mpsc::poll_recv`, Embassy:
interrupt DMA, bare-metal: poll FIFO). `try_send` is fire-and-forget:
Reticulum is best-effort, higher layers retransmit.

`dispatch_actions()` lives in core (not the driver) because action routing
(broadcast exclusion, interface selection) is protocol knowledge.

In `leviculum-std`, `InterfaceHandle` wraps `tokio::sync::mpsc::Sender`
behind the trait. An embedded driver implements it directly on a radio struct.

Core processes packets with zero delay. Collision avoidance (jitter, CSMA)
is the interface's responsibility — fast interfaces (TCP) transmit immediately,
slow interfaces (LoRa) apply send-side jitter. This is the
[interface-isolation](concepts/interface-isolation.md) rule in code.

## Writing a Driver

### 1. Create interface objects
Implement `Interface` on your outbound channel. Register with your own
bookkeeping. Core references interfaces by `InterfaceId` only.

### 2. Run the event loop
Minimum 3 branches: receive, timer, shutdown. Feed everything through
the post-dispatch sequence above.

### 3. Handle the receive path
Driver-specific. On complete packet: `core.handle_packet(iface_id, &data)`
→ post-dispatch. On disconnect: `core.handle_interface_down(iface_id)`.

## Packet Flow

### Incoming
```
Interface → deframe → mpsc → recv_any() → handle_packet()
  → Transport::process_incoming() → TickOutput
  → dispatch_actions() → interfaces → wire
  → events → application
```

### Outgoing
```
Application → connect/send/announce → TickOutput (via action_dispatch)
  → dispatch_actions() → interfaces → wire
```

### Local Client (Shared Instance)
```
lnstest/lncp → Unix socket → LocalInterface (HDLC)
  → handle_packet() with is_local_client=true
  → local_client_known_dests updated (6h TTL)
```

### RPC (rnstatus, rnpath, rnprobe)
```
Python CLI → Unix socket → RPC server (multiprocessing.connection, pickle)
  → handlers query NodeCore state or trigger probe
  → pickle response → CLI
```

The server serves at most 64 connections at once and gives each 10 s from
accept to last response; a connection past the cap is closed unanswered and
counted in a rate-limited `RPC_CONN_REFUSED` warning, so idle connections
cannot hang `rnstatus` against `lnsd`.

The shared-instance socket and this RPC channel are what make `lnsd` a
drop-in for `rnsd`; see
[Python-RNS Compatibility](concepts/python-rns-compatibility.md).

### IPC platform support

The shared-instance data channel and the RPC control channel use abstract
Unix sockets on Linux, filesystem Unix sockets on macOS/BSD, and TCP loopback
on Windows (mirroring Python-RNS's AF_INET fallback). Linux is the tested path
and is the one exercised by our CI; macOS/Windows IPC is community-supported
and not exercised by our CI.

## Storage Trait

For the conceptual rationale (one core, host or embedded backend) see
[Storage and Embedding](concepts/storage-and-embedding.md); for the
per-method deep dive see
[Storage Trait Split Analysis](storage-trait-analysis.md).

Type-safe methods organized by collection:

| Collection | Key methods |
|------------|-------------|
| Packet dedup | `has_packet_hash`, `add_packet_hash` |
| Path table | `get_path`, `set_path`, `remove_path`, `expire_paths` |
| Reverse table | `get_reverse`, `set_reverse`, `remove_reverse` |
| Link table | `get_link_entry`, `set_link_entry`, `remove_link_entry` |
| Announce table | `get_announce`, `set_announce`, `remove_announce` |
| Announce cache | `get_announce_cache`, `set_announce_cache` |
| Receipts | `get_receipt`, `set_receipt`, `remove_receipt` |
| Ratchets | `load_ratchet`, `store_ratchet`, `list_ratchet_keys` |
| Cleanup | `expire_*` per collection |

Shared types in `storage_types.rs`: `PathEntry`, `ReverseEntry`, `LinkEntry`,
`AnnounceEntry`, `PacketReceipt`.

Implementations: `NoStorage` (no-op), `MemoryStorage` (BTreeMap, host/tests),
`EmbeddedStorage` (heapless `FnvIndexMap`, fixed capacity, used by `leviculum-nrf`),
`FileStorage` (wraps MemoryStorage + disk).

### FileStorage Persistence

| File | Format | Strategy | Contents |
|------|--------|----------|----------|
| `known_destinations` | msgpack map | Batch flush (hourly + shutdown) | Identity → destination |
| `packet_hashlist` | msgpack array | Batch flush | 32-byte dedup hashes |
| `ratchets/{hash}` | msgpack map | Write-through | Receiver ratchet keys |
| `ratchetkeys/{hash}` | signed msgpack | Write-through | Sender ratchet private keys |

Non-persistent collections (paths, reverses, links, announces, receipts)
are RAM-only and rebuilt from network on restart.

## Logging

Sentence-style messages with inline context. Good:
```
Destination <81b22f60> is now 4 hops away via <ecc35451> on iface 1
Answering path request for <4c0c6c7f> on iface 1, path is known
```
Bad:
```
path updated dest=81b22f60 hops=4
```

Use `HexShort` for hashes. Always explain drop reasons ("rate limited",
"duplicate packet", "no path known").

| Component | What | Level |
|-----------|------|-------|
| transport process_incoming | Packet dispatch, drop reasons | `trace!` |
| transport handle_announce | Path updates, rebroadcast decisions | `debug!` |
| transport forward_packet | Forwarding decisions | `debug!` |
| node/link_management | Link lifecycle, RTT | `debug!` |
| driver | Startup, interface registration | `info!` |
| interfaces | Connection events, I/O errors | `info!`/`warn!` |

## Memory allocator

Every Leviculum program runs on one allocator, now and in future (Lew,
2026-11-13, order 1805): **mimalloc v3**, through the workspace crate
`leviculum-alloc`, which owns the only mimalloc dependency (`mimalloc`
0.1.52 / `libmimalloc-sys` 0.1.49, mimalloc 3.3.2). The choice was measured
in the allocator study of 2026-11-13 (review repository,
`notizen/2026-11-13-allocator-studie.md`) against musl's mallocng, glibc's
ptmalloc, Zig's SmpAllocator, jemalloc, snmalloc and mimalloc v2, on lnsd,
lnpnd and the Loquax Mailbox: it is the only candidate that builds on every
shipped triple, hands a spike back in 0.1 s, costs the least CPU per flush
and under cross-thread churn, and starts in the least memory.

Every binary carries the same two lines:

```rust
#[global_allocator]
static ALLOC: leviculum_alloc::Global = leviculum_alloc::GLOBAL;

fn main() {
    leviculum_alloc::start();
    // an async body runs behind a plain `main`, after this line
}
```

Under `heap-accounting` the static is
`CountingOver<leviculum_alloc::Global>` instead, so the census counts and
forwards to the same allocator.

Three settings come with it, and the crate owns all three:

- **Huge pages off at compile time.** `.cargo/config.toml` passes
  `-DMI_DEFAULT_ALLOW_THP=0` to mimalloc's C build through `[env] CFLAGS`,
  on every triple. On glibc mimalloc initialises in a constructor before
  `main`, so a `PR_SET_THP_DISABLE` in `main` comes too late there: the study
  measured 6.8 MB `--version` with it and 2.9 MB with the define. `start()`
  sets the prctl as well, as a belt.
- **A release thread.** `start()` spawns one thread, `alloc-release`, that
  runs `mi_collect(true)` once a second. mimalloc v3 keeps a freed span for
  ten seconds or more; the thread brought lnsd from 191 to 50 MB anonymous
  memory 20 s after start, at no measurable CPU.
- **A release at the spike sites.** `leviculum_alloc::release()` runs
  `mi_collect(true)` on the calling thread. leviculum-std calls it where a
  blocking job frees a structure the size of the store: at the end of the
  `known_destinations` flush closure, after the replaced table is dropped,
  and after the start-up load of `known_destinations`. The flush's spike is
  back in 0.1 s instead of 10 to 20 s (`tests/flush_release_memory.rs`).

**The link rule.** musl binaries on every triple link with rust-lld and
Rust's self-contained musl (`-C link-self-contained=yes`), so the libc
`malloc` that C code still calls (bundled SQLite, libdbus) is musl's
mallocng, never Zig's SmpAllocator, which keeps every slab and strands every
cross-thread free. Zig is the cross C compiler for arm64 and armhf
(`scripts/zig-cc.sh`, `scripts/zig-ar.sh`) and links nothing musl. On armv7
libmimalloc-sys asks for `-latomic`; armv7-A has native 64-bit atomics, so
`DEP_ATOMIC = "c"` names libc instead and rust-lld would fail loudly on an
`__atomic_*` call. The glibc desktop `loquax` still links through cargo
zigbuild at glibc 2.31. The packaging pins ziglang 0.16.0 and cargo-zigbuild
0.23.4.

**Exemptions**, the only ones: `libloquax_android.so`, where bionic's scudo
owns the JVM's process and `leviculum-alloc` is `System` (both functions
are no-ops); firmware images (nRF, M9), which have no libc and their own
heap; `leviculum-heap-lab` and test or bench binaries with their own
counting allocators (`leviculum-std/src/bin/heap-gap-bench.rs`). One-shot
tools, lnflash included, are not exempt: with huge pages off mimalloc v3
starts in 2.7 to 4.0 MB against mallocng's 3.2 MB, and one rule beats a
list.

`scripts/check-allocator.py` enforces the rule. Its source half (in
`just guards`) finds every binary target by cargo's rules and wants the two
lines, refuses a mimalloc, jemalloc or snmalloc dependency anywhere but in
`leviculum-alloc`, wants the three `release()` sites, the define, a musl
build without cargo zigbuild in `scripts/build-deb.sh`, and re-reads each
exemption's reason. Its bytes half (`--binary`, run by build-deb.sh on
every packaged binary) wants mimalloc's message string in the file, musl
mallocng's `__libc_malloc_impl` symbol in every musl binary (a zigbuild link
strips it and brings Zig's malloc), and `option 'allow_thp': 0` from
`MIMALLOC_VERBOSE=1 <bin> --version` wherever the binary runs, natively or
under qemu-user. The nightly's tarball step reads the same two strings with
grep.

The allocator decides whether a spike stays, not how high it gets: how
high the flush peaks is the streaming flush's business (order 1749,
`tests/known_destinations_flush_memory.rs`).

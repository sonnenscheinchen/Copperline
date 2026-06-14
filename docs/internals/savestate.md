# Save states (`savestate.rs`)

A save state snapshots the whole emulated machine to a single file and
restores it exactly. Because the core is deterministic
([](architecture.md#determinism-and-the-host-boundary)), a restored run
is byte-for-byte identical to one that was never interrupted -- the
regression gate for the feature literally `cmp`s screenshots taken on
either side of a save/restore cycle.

User-facing behaviour (shortcuts, menu items, the `--save-state-after` /
`--load-state` flags, and the operational caveats) is documented in the
[interactive UI guide](../guide/ui.md#save-states) and the
[headless-runs guide](../guide/headless.md#save-states-headless).
This chapter is the implementation and format reference.

## Design

The state is produced by `serde` derives on the live state structs
themselves -- there is no hand-maintained parallel "snapshot" schema.
`Bus` and everything it owns derive `Serialize`/`Deserialize`, as does
the vendored CPU core (`vendor/m68k`'s `CpuCore` is one flat struct of
plain registers and configuration; it has no lazily built decode tables,
so the whole struct round-trips). New fields are picked up by the
derives automatically; the cost of that convenience is the versioning
rule below.

What is captured:

| Component | Contents |
|---|---|
| `CpuCore` | registers, SR flags, prefetch queue, pending interrupt/stop state, MMU/CACR state, cycle-timing configuration, `cpu_type` and address mask |
| `MachineRuntimeState` | the `M68kMachine` fields outside the core: `last_cacr`, `sync_cck_on`, `cpu_clocks_per_cck`, `cpu_clock_carry` |
| `icache` / `dcache` | the opt-in 020/030 cache models (`Option<Box<CpuCache>>`, `None` when not configured) |
| `Bus` | chip/slow RAM, ROM and extended ROM, Zorro boards (including their RAM), both CIAs, RTC, Agnus/Copper/Denise/Paula/blitter state, floppy controller with in-memory disk images, Gayle IDE, A2091 SCSI, Akiko/CDTV with NVRAM, beam-event capture buffers, DMA pointers, interrupt latches, and the bus-arbitration counters |

Deliberately excluded, with the mechanism in parentheses:

- **Host sinks**: Paula's `Box<dyn SerialSink>` / `Box<dyn AudioSink>`
  (`#[serde(skip, default = ...)]` producing inert null sinks). On load,
  `Bus::adopt_host_resources` moves the live sinks from the old Bus onto
  the restored one, so audio output continues uninterrupted.
- **Diagnostic host state**: the `COPPERLINE_TRACE_BLITTER` file handle
  (skipped, moved across like the sinks), the debugger and its
  breakpoints/watchpoints (never serialized; they stay armed across a
  load), and the `dbg_*` instrumentation counters in `M68kMachine`.
- **Wall-clock anchors**: `DeviceClock::realtime_anchor` is an
  `Instant` and is skipped; it deserializes as `None` and
  `realtime_cck_due` lazily re-anchors to the host clock on first use.
  `Emulator::load_state` additionally re-baselines the frame pacer
  (`reanchor_realtime_clock`) so the run does not sprint to "catch up"
  the emulated-time jump.
- **Memo caches**: the bitplane slot-plan `Cell` cache and the pending
  debugger-window register hit (skipped; rebuilt or irrelevant).

ROM is embedded rather than fingerprinted: a state is self-contained
with respect to everything that was in memory, so there is no separate
config-compatibility check -- restoring the Bus and CPU restores the
machine model along with them (the CPU bus adapter's address-mask copy
is re-synced from the restored core's `address_mask`).

### File-backed images

Two subsystems hold open `File` handles inside otherwise serializable
state. Both get manual serde implementations through small shadow
structs, and both **reopen the file during deserialization**, which
turns a missing or moved image into a clean load-time error instead of a
later I/O panic:

- `HardDriveImage` (shared by Gayle IDE and A2091 SCSI) serializes as
  `HardDriveImageState { path, memory, total_sectors, rdb_overlay,
  overlay_write_warned, scsi_bus }`. A file-backed image stores
  `memory: None` and reopens `path` read/write on load; an in-memory
  directory-built volume stores the whole image in `memory`, so its
  session-only writes survive the round trip. The synthesized-RDB
  overlay for bare hardfiles is embedded either way. Consequence: HDF
  *file contents* are not part of the state -- guest writes made after
  the snapshot are still visible after restoring.
- `CdImage` serializes as `CdImageState { paths, tracks, extents,
  total_sectors }` and reopens every image file read-only on load
  (`CdImage` now keeps the per-file `paths` alongside its `files`
  for exactly this purpose).

Floppy images need no special handling: `FloppyImage` keeps its data
in memory (`StandardAdf(Vec<u8>)` or per-track structures), so inserted
disks travel inside the state, unsaved track writes included.

## File format

```
offset  size  contents
0       8     magic, ASCII "CLSSTATE"
8       4     format version, u32 little-endian (STATE_VERSION)
12      ...   zlib stream (RFC 1950) containing the payload
```

The payload inside the zlib stream is five bincode values written
back-to-back by `M68kMachine::write_state`, in this fixed order:

1. `CpuCore`
2. `MachineRuntimeState`
3. `icache: Option<Box<CpuCache>>`
4. `dcache: Option<Box<CpuCache>>`
5. `Bus`

`M68kMachine::apply_state` reads them back in the same order and only
swaps the machine onto the parsed state after every component has
deserialized, so a truncated or corrupt file leaves the live machine
untouched (`savestate::tests::truncated_payload_leaves_the_machine_untouched`).

Encoding details, for anyone reading a state file from outside:

- bincode 1.x legacy defaults: little-endian, **fixed-width** integers
  (`u16` is 2 bytes, `u32` 4, `usize` 8), `bool` as one byte,
  `Option<T>` as a one-byte tag (0/1) followed by the value, enum
  variants as a `u32` index, and `Vec`/`String`/`PathBuf` as a `u64`
  length prefix followed by the elements/UTF-8 bytes.
- Arrays larger than 32 elements go through `serde-big-array` (the
  AGA palette's two `[u16; 256]` nibble planes, autoconfig ROM images,
  CPU-cache line arrays); on the wire they are simply the elements in
  order, like any other array.
- The payload is **not self-describing**: the schema is the Rust
  structs at the `STATE_VERSION` that wrote the file. There are no field
  names or tags in the stream.
- Compression is `flate2` at `Compression::fast()`; any standard zlib
  inflater reads it regardless of level. A Kickstart 2.05 machine
  (512K chip + 512K ROM) compresses to roughly 400 KB.

## Versioning

`STATE_VERSION` (in `savestate.rs`) is compared exactly on load; a
mismatch fails with a message naming both versions. Because the payload
is positional bincode of the live structs, **any** shape change to any
serialized struct -- a field added, removed, reordered, or retyped
anywhere under `Bus`, the chipset modules, `CpuCore`, floppy or
expansion state -- silently changes the wire layout. The rule is
therefore: bump `STATE_VERSION` whenever such a change lands, so stale
files are refused with a clear version message instead of failing with a
confusing decode error (or worse, decoding into nonsense). There is no
migration machinery; old states are simply invalidated.

## Snapshot point and atomicity

The app-level contract is that states are taken between emulated frames:
the window event loop and the headless timers both act only after
`step_frame` returns, and `--save-state-after` fires at the first frame
boundary past its deadline. Strictly, the serialized surface is complete
enough that any inter-instruction point round-trips (the unit test saves
mid-frame after arbitrary `step_slice` counts); the frame boundary is
kept as the documented contract because it is what the calling code
guarantees and it keeps presentation state trivially rebuildable.

`savestate::save` takes `&M68kMachine` and does not mutate emulated
state. `savestate::load` parses fully before applying, then moves host
resources across; on success the window forces power on, clears any CPU
halt latch, and invalidates `last_rendered_emulated_frame` so the next
presentation re-renders from the restored Bus.

## Verification

The determinism gate lives at three levels:

- `cpu::tests::save_state_round_trip_replays_identically`: runs a
  chip-RAM loop that also writes COLOR00 (so CPU, RAM, and beam-event
  capture all advance), saves at T1, runs 20k instructions to T2 saving
  the state again, rewinds to T1, replays the same step pattern, and
  asserts the trace matches **and the re-serialized T2 state file is
  byte-identical** to the original timeline's.
- `savestate::tests` cover magic/version rejection and the
  truncated-payload atomicity guarantee; `harddrive::tests` and
  `cdrom::tests` cover the reopen-by-path round trips and the
  missing-file error paths.
- End-to-end: save mid-run plus `--screenshot-after T`, then
  `--load-state` plus `--screenshot-after T` in a fresh process, and
  `cmp` the PNGs. Verified byte-identical on Kickstart 2.05, State of
  the Art mid-demo (floppy and blitter state in flight), and A1200
  Workbench (AGA, Gayle).

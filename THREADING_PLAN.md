# Render-thread pipeline: design and implementation plan

Status: implemented and default-on on branch `threaded-render`.
`COPPERLINE_THREADED_RENDER=0` (also `false`, `off`, or `no`) forces the
old synchronous render path for comparison. The analysis below was done
after the 2026-06-15 host-CPU optimisation pass (committed to `main`:
idle device ticks, background-fill chunking, poll-stats table).

2026-06-16 implementation update: the current renderer is now an owned
completed-frame consumer through `RenderInput`/`render_from_input`. Agnus
blanking and geometry latches are captured into the input bundle. Denise
collision accumulation is completed synchronously at frame end on the bus,
so the worker never writes late `CLXDAT` bits back into emulator-visible
state. The worker renders, applies presentation post-processing, owns the
deinterlacer history, and returns a tagged presentation buffer plus render
timing to the main thread; wgpu/winit present stays on the main thread.

2026-06-16 Rosetta/live-audio update: the A1200 `second-nature.adf` CPAL
underrun report was reproduced with the x86_64/Rosetta build and then with
the native arm64 build. The root cause was the live CPAL queue/pacer
interaction: playback began below the advertised 150 ms cushion, and after a
host wall-clock hitch the sink continued reporting a full cushion even when
the queue had been drained. That was addressed in the audio sink by
prebuffering to the target, reporting under-target queue deficit back to the
real-time pacer, and rebuffering before the CPAL callback reaches an empty
queue. This is evidence against treating render threading as the first fix
for this symptom.

## Why threading, and the realistic ceiling

The emulator runs the CPU, chipset and the framebuffer paint on a single
thread (winit event loop). A beta tester's borderline machine (A1200/AGA,
68000 clocked to 24.83 MHz = 7x colour clock = 3.5x stock) could not
sustain real time on one core, starving the audio ring buffer -> stutter.

Profiled split on that config (after the committed hot-path pass):

- per-cck chipset/bus arbitration (advance_beam, bitplane_slot_active_at,
  tick_timed_devices, advance_audio, ...): ~60%  -- inherently serial
- CPU emulation (step_slice, bus reads, m68k decode): ~22%
- renderer (bitplane::render): ~12% after the background-fill fix
  (was ~24%; background phase went 1230ms -> 28ms)

The renderer is internally *sequential* (the playfield loop marches a
bitplane-pointer replay cursor down the frame; rows are not independent),
so data-parallel row splitting would break byte-identity. The right design
is a **2-stage pipeline**: emulate frame N on the main thread while a
worker renders frame N-1. Frame wall-time becomes
`max(emulate, render+present)` instead of the sum. With render ~12% the
ceiling is modest now (~10-12%); the bigger structural lever would be
moving *emulation* to the worker and render+present to main, but that
requires routing live input across the thread boundary (see "Alternative").

For the reported 2.3 GHz 11th-gen Intel machine, this should not be treated
as the primary fix until that host is profiled. If render is really only
~12% of frame time, the best possible gain from hiding it is modest and will
not reduce total CPU consumption; it only spreads work over another core.
If the machine is more than ~10-15% over real-time budget, the serial
chipset/bus/audio path is likely the real bottleneck.

Cross-building x86_64 on the M5 did not reveal a hidden Rosetta win. In the
headless checks run on 2026-06-16, the x86_64 binary was roughly 0.62-0.64x
native arm64 speed while producing byte-identical screenshots. Keep native
arm64 as the performance baseline on Apple Silicon; use the x86_64 build only
to reproduce Intel/Rosetta-specific behavior.

`taskpolicy -c background target/release/copperline` is
a useful low-clock stress profile on Apple Silicon because it pushes the
Rosetta process onto efficiency cores, but it is harsher than a pure CPU
frequency proxy: it also applies background scheduling. On 2026-06-16 the
A1200/AGA AROS 8-second headless screenshot run reached only 54.4% emulated
speed under this policy (versus about 315% for the same x86_64 binary without
the policy). A live A1200 `second-nature.adf` run under the policy produced
no CPAL underrun warnings after the audio-pacer fix, but it spent most
seconds wall-late and clearly did not keep real time. Use this profile to
stress recovery behavior, not as a one-to-one model of a slower Intel host.

GPU constraint: wgpu/winit require surface present and window ops on the
**main thread**. So the worker may *paint into a CPU framebuffer* but must
not touch the GPU surface. Present stays on main.

## Determinism requirements

The renderer already consumes a *completed-frame* snapshot, not live state:
`bus.frame_chip_ram()` returns `last_frame_chip_ram` (a clone taken at the
end-of-frame swap, bus.rs ~5902), plus a timed write journal and recorded
beam events. Rendering is a pure function of that immutable data. If the
worker owns its copy, the emulation thread can advance frame N freely while
the worker paints frame N-1. Save-state / input-replay determinism is
untouched (they depend on the emulation core, not the paint).

Implementation notes for the bus dependencies that had to be removed from
`bitplane::render`:

1. Denise collision write-back: `Bus::begin_new_beam_frame` now completes
   unread live collision replay to the end of the just-finished frame before
   the next CPU frame can run. The synchronous renderer still returns and ORs
   collision bits harmlessly, but the worker path treats them as diagnostic
   render output and does not write late bits back to `CLXDAT`.
2. Agnus programmable blanking/frame-end blanking: frame height and
   programmable horizontal/vertical blank windows are captured into
   `RenderInput`; the worker does not sample live Agnus latches while painting
   an earlier frame.
3. Debug/trace side effects: plane export and display-plan logging use the
   bundled frame number/time. The file writes can run on the worker because
   all metadata comes from the bundle.

## The owned render bundle (`RenderInput`)

`bitplane::render(bus, fb)` currently pulls 17 accessors plus
`RenderState::from_bus(bus)`. To run on a worker, extract them into an owned
struct at frame boundary and add `render_from_input(&RenderInput, fb) ->
VideoRenderFrameTiming`. Then `render(bus, fb)` becomes: build input,
call `render_from_input`, record the returned timing. The headless and
screenshot paths keep calling `render(bus, fb)` and must stay
byte-identical (gate below).

Bundle contents (all owned; clone the slices, move/clone the snapshot):

- geometry: FrameGeometry (Copy)
- visible_start_vpos: u32
- palette_split: (Palette, Palette, bool)
- top_palette_end: Palette
- frame_render_events: Vec<BeamRegisterWrite>      (clone &[])
- current_render_base: RenderRegisterSnapshot
- current_render_events: Vec<BeamRegisterWrite>    (clone &[])
- bottom_palette_events: Vec<BeamRegisterWrite>    (clone &[])
- chip_ram: Vec<u8>                                (2 MB AGA; see handoff)
- chip_ram_writes: Vec<BeamChipRamWrite>           (clone &[])
- captured_bitplane_rows, captured_sprite_lines    (clone; can be large)
- sprite_dma_observed, sprite_display_enable_x_by_y
- render_state: RenderState (from_bus) -- lift its inputs into the bundle
- emulated_seconds, emulated_frames: f64/u64       (for COPPERLINE_DBG_* )
- agnus_frame_lines and programmable blank windows
- returned collision bits for synchronous rendering / diagnostics; the
  threaded path does not apply them to Denise CLXDAT

Gotchas:

1. Write-back: `record_video_render_frame(timing)` mutates the bus. On the
   worker, return the timing from `render_from_input` and record it on main.
2. Hardware write-back: `CLXDAT` collision bits are emulator-visible state,
   not presentation metadata. They are completed synchronously on the bus at
   frame end; do not apply worker-returned collision bits later.
3. DBG side effects: the playfield export keyed on `emulated_seconds`
   (COPPERLINE_DBG_AFTER/UNTIL) writes files. Pass the scalars in the
   bundle; the file write is fine on the worker but rare. No bus access.
4. Snapshot cost: the bus still clones chip RAM into `last_frame_chip_ram`
   and `RenderInput` currently clones it into the worker bundle. A future
   optimisation should hand ownership of that buffer to the worker via a
   double-buffered swap (two Vecs ping-pong between bus and worker) rather
   than cloning into the bundle.

## Pipeline wiring (window.rs)

Persistent worker thread (std::thread, no new deps). Two channels:
`main -> worker` sends `RenderInput`; `worker -> main` returns the painted
+ post-processed presentation framebuffer (and the timing). Per main-loop
iteration:

1. step_frame()  (emulate frame N)
2. build RenderInput for N (or reuse the swapped snapshot buffer)
3. send N to worker
4. recv the finished buffer for N-1 (blocks only if the worker is behind)
5. upload N-1 to the GPU surface and present (main thread)
6. record N-1's returned render timing

The worker runs `render_from_input` then the existing post-process
(center/mask/stretch/deinterlace) into the presentation buffer. The centring,
masking and stretching pieces are pure framebuffer transforms once their
frame metadata is bundled. The deinterlacer is stateful: it owns previous
fields and phosphor history, so either the worker must own that state for the
whole session or deinterlacing must remain on main. Power-off, reset, mode
changes, screenshots, frame dumps, and video recording must all consume a
worker result tagged with the emulated frame number they are saving.

One frame of added latency is acceptable for an emulator, but it must be
explicitly represented in the capture path. Do not gate a screenshot or
frame dump on frame N while saving the N-1 presentation buffer.

## Verification strategy (so it is not landed blind)

1. Byte-identity gate (already built, /tmp/gate equivalents): OCS/ECS/AGA
   boot screenshots must be SHA-identical before/after the `RenderInput`
   refactor. This catches any extraction error.
2. Unit/regression tests for the hardware side effects separated above:
   Denise collision bits, programmable blanking, frame-end blanking, and
   interlace/phosphor history. These should name the hardware behaviour, not
   a particular title. Current coverage includes frame-end live `CLXDAT`
   completion, programmable blanking, and deinterlace/phosphor history.
3. Threaded path smoke: `cargo test
   video::window::tests::recording_captures_emulated_frames_with_audio`
   exercises worker render completion, tagged presentation output, deinterlace
   ownership, and recorder capture alignment.
4. Threaded-dump comparison: add a headless mode that drives the *threaded*
   pipeline with `--dump-frames` and diff every frame against the synchronous
   baseline. This verifies channel ordering / frame skew / bundle correctness
   end-to-end. Only the literal GPU present call (a few lines, unchanged) then
   remains unverified -- low risk.
5. Full unit suite + clippy + fmt.
6. Human check at the live window: no tearing, input latency acceptable,
   audio underruns gone on a slow machine.

## Alternative (higher ceiling, more risk): emulation on the worker

Move the *emulator* to the worker thread; main runs winit + render +
present. This offloads the ~60% emulation off the UI/GPU thread. Needs:
live keyboard/mouse/menu events routed main -> worker over a channel
(scripted input is already timestamped and could share the path), and the
frame snapshot routed worker -> main. Bigger event-loop refactor; same
determinism argument holds. Consider after the render-pipeline lands and is
measured.

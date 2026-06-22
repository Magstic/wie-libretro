# WIE Libretro Architecture

This document describes the Libretro core in this fork of WIE.

The core exposes WIE as a Libretro `cdylib` so RetroArch and other Libretro frontends can run ZIP-packaged WIPI, SKVM, and J2ME content.

## Goals

- **Primary product**: `wie_libretro`, a Libretro core for desktop and Android frontends.
- **Supported content**: ZIP archives for KTF, LGT, SKT, and generic J2ME/JAR payloads.
- **Host integration**: implement WIE's platform traits with Libretro video, audio, input, logging, and save directories.
- **Portability**: keep the core pure Rust where possible. The ARM engine is the `arm32_cpu` interpreter and does not require Unicorn or native JIT dependencies.
- **Non-goals**: savestates, rewind, netplay, run-ahead, and deterministic frame stepping.

## Crate Layout

```text
wie_libretro/
├─ Cargo.toml           # cdylib + rlib; optional `hooks` feature
├─ wie_libretro.info    # RetroArch metadata
└─ src/
   ├─ lib.rs            # exported retro_* ABI and global lifecycle state
   ├─ ffi.rs            # minimal Libretro C ABI bindings
   ├─ core.rs           # LibretroCore, worker thread, run loop glue
   ├─ environment.rs    # core options, frontend directories, localization
   ├─ platform.rs       # WIE Platform and Screen implementations
   ├─ audio.rs          # PCM mixer and rustysynth MIDI path
   ├─ video.rs          # frame storage and RGB565 fallback conversion
   ├─ input.rs          # RetroPad/keyboard polling to WIE key events
   ├─ content.rs        # ZIP extraction and runtime detection
   ├─ filesystem.rs     # WIE filesystem mapped to frontend save dir
   ├─ database.rs       # RMS/database storage mapped to frontend save dir
   └─ shared.rs         # cross-thread shared state
```

The Libretro crate depends on the existing WIE runtime crates:

- `wie_ktf`
- `wie_lgt`
- `wie_skt`
- `wie_j2me`
- `wie_backend`
- `wie_core_arm` through runtime crates

## High-Level Data Flow

```text
Libretro frontend
  ├─ retro_load_game
  │    └─ read ZIP → extract → detect runtime → create WIE emulator
  ├─ retro_run, 60 Hz nominal
  │    ├─ poll input → Keydown/Keyup events → worker channel
  │    ├─ copy latest framebuffer → video_refresh
  │    └─ mix PCM + software MIDI → audio_sample_batch
  └─ retro_unload_game / retro_deinit
       └─ stop MIDI, signal worker, join worker, drop emulator

wie-libretro worker thread
  ├─ receive input events
  ├─ convert redraw requests into Event::Redraw
  └─ call Emulator::tick() repeatedly on a large Rust stack
```

## Relationship to WIE Backend

WIE is already split into platform-independent runtime code and host-facing traits. `wie_libretro` is one host implementation.

The important backend traits are:

| WIE trait | Libretro implementation |
|---|---|
| `Platform` | `LibretroPlatform` |
| `Screen` | `LibretroScreen` |
| `AudioSink` | `LibretroAudioSink` |
| `Filesystem` | `LibretroFilesystem` |
| `DatabaseRepository` / `Database` | `LibretroDatabaseRepository` / `LibretroDatabase` |
| `Emulator` | Owned by the worker thread and driven through `tick()` |

The core does not rewrite the emulator runtime. It provides host services and lets the existing KTF/LGT/SKT/J2ME implementations run normally.

## Libretro ABI Lifecycle

The exported ABI lives in `wie_libretro/src/lib.rs`.

Global state is stored in a `LazyLock<Mutex<GlobalState>>`. `GlobalState` stores frontend callbacks, the save directory, the log interface, and the currently loaded `LibretroCore`.

Lifecycle responsibilities:

| Function | Responsibility |
|---|---|
| `retro_set_environment` | store `environ_cb` and register core options |
| `retro_set_video_refresh` | store video callback |
| `retro_set_audio_sample` / `retro_set_audio_sample_batch` | store audio callbacks |
| `retro_set_input_poll` / `retro_set_input_state` | store input callbacks |
| `retro_init` | query save directory, log interface, and duplicate-frame support |
| `retro_get_system_info` | advertise ZIP content, full-path loading, and no frontend extraction |
| `retro_get_system_av_info` | report geometry from core options and fixed 48 kHz audio |
| `retro_load_game` | read content, parse options, start `LibretroCore` |
| `retro_run` | delegate one frontend frame to `LibretroCore::run` |
| `retro_reset` | unload and reload the same content blob |
| `retro_unload_game` | shut down the current core |
| `retro_deinit` | shut down and reset global state |
| `retro_serialize*` | unsupported, returns no savestate data |
| `retro_get_memory_*` | unsupported, returns null/zero |

All exported functions use an FFI guard to keep Rust panics from crossing the C ABI boundary.

## Content Loading

The core accepts ZIP content only.

`retro_get_system_info` reports:

- `valid_extensions = "zip"`
- `need_fullpath = true`
- `block_extract = true`

`retro_load_game` reads content from extended game info memory if available, otherwise from the full path. The ZIP is extracted with `wie_backend::extract_zip` and passed to `content::load_emulator`.

Runtime detection order in `auto` mode:

| Detection | Runtime |
|---|---|
| `KtfEmulator::loadable_archive` | KTF |
| `LgtEmulator::loadable_archive` | LGT |
| `SktEmulator::loadable_archive` | SKT |
| first `.jar` in the ZIP | J2ME |

The `wie_runtime` core option can force `ktf`, `lgt`, `skt`, or `j2me`. Forced modes validate that the archive is compatible with the requested runtime.

KTF and LGT use `Options { enable_gdbserver: false, profile: None }`.

## Threading Model

The emulator runs on a dedicated worker thread created by `LibretroCore::load`.

Reasons:

- WIE can require a deep Rust stack when executing nested Java/guest calls.
- Libretro frontends do not guarantee a large stack for the `retro_run` caller.
- WIE is wall-clock driven, while Libretro calls `retro_run` at frontend cadence.

The worker thread is created with:

- name: `wie-libretro-emu`
- stack size: `32 * 1024 * 1024`

`retro_run` does not execute guest code directly. It only:

1. reads updated core options,
2. polls frontend input,
3. sends input events to the worker,
4. presents the latest framebuffer,
5. renders audio for the frontend callback.

Worker loop:

1. drain pending `WorkerMsg::Event` messages,
2. convert `redraw_requested` into `Event::Redraw`,
3. call `Emulator::tick()`,
4. sleep enough to avoid a busy loop when `tick()` returns quickly.

Shutdown sequence:

1. silence and close MIDI state,
2. clear PCM voices,
3. set `quit`,
4. join the worker thread,
5. clear PCM voices again after join.

## Shared State

`Shared` is the cross-thread state between Libretro frontend calls and the worker.

| Field | Purpose |
|---|---|
| `framebuffer: Mutex<Frame>` | latest painted ARGB8888 frame |
| `redraw_requested: AtomicBool` | set by `Screen::request_redraw`, consumed by worker |
| `audio: Arc<Mutex<AudioState>>` | PCM voice queue and mixer buffers |
| `midi: Arc<MidiOutput>` | software MIDI synthesizer state |
| `quit: AtomicBool` | worker/core shutdown flag |
| `fatal: Mutex<Option<String>>` | worker error reporting |

The frontend copies the framebuffer into its own `video_frame` before calling `video_refresh`. This avoids holding the framebuffer lock while entering frontend code.

## Time Model

`LibretroPlatform::now()` returns wall-clock milliseconds from `SystemTime::now()`.

WIE's backend executor is wall-clock driven. `Emulator::tick()` runs work until backend time budgets and timers say it should stop. The Libretro frontend frame rate does not deterministically step guest time.

Implications:

- Frontend fast-forward is disabled with `RETRO_ENVIRONMENT_SET_FASTFORWARDING_OVERRIDE` because WIE logic and audio are wall-clock driven.
- Pausing or stalling the frontend may stop video/audio presentation, but the worker's timing model remains wall-clock based while loaded.
- Savestate and rewind are intentionally not supported.

## Video

The preferred pixel format is `RETRO_PIXEL_FORMAT_XRGB8888`.

`Screen::paint` stores `Image::argb8888()` into `Shared::framebuffer`. On little-endian targets, WIE's `0xAARRGGBB` `u32` storage is compatible with Libretro XRGB8888 memory layout for practical frontend use.

If the frontend rejects XRGB8888, the core requests `RETRO_PIXEL_FORMAT_RGB565` and converts from the stored ARGB8888 frame in `Frame::rgb565()`.

Geometry comes from `wie_resolution`:

| Option | Size |
|---|---|
| `240x320` | default |
| `176x208` | compact phone screen |
| `128x160` | small phone screen |
| `128x128` | square phone screen |

The selected resolution is session-level state. Runtime changes are detected, but resolution changes require reloading content to fully take effect.

## Input

`InputManager` converts Libretro polling state into WIE edge-triggered events.

Supported sources:

- `RETRO_DEVICE_JOYPAD`
- left analog stick axes
- `RETRO_DEVICE_KEYBOARD`

The manager tracks individual input sources and a pressed-count per WIE key. This avoids duplicate `Keydown`/early `Keyup` when multiple physical sources map to the same mobile key.

Generated events:

- `Event::Keydown(KeyCode)`
- `Event::Keyup(KeyCode)`

Key repeat generation was intentionally removed from the Libretro host. If a guest runtime emits or consumes repeat semantics internally, that remains runtime-specific; the host does not synthesize `Event::Keyrepeat`.

Input descriptors are registered with `RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS` so frontends can show meaningful labels and allow user remapping.

## Audio

The Libretro audio path is fully inside the core and uses `audio_sample_batch` when available.

Constants:

- output sample rate: `48_000 Hz`
- nominal frontend cadence: `60 Hz`
- frames per `retro_run`: `round(48000 / 60) = 800` stereo frames

### PCM

`AudioSink::play_wave` pushes a `PcmVoice` into `AudioState`.

`AudioState::render`:

1. clears reusable left/right float buffers,
2. mixes active voices,
3. linearly resamples source PCM to 48 kHz,
4. removes finished voices,
5. writes interleaved stereo `i16` output.

Mono input is duplicated to left/right. Stereo input preserves both channels.

### MIDI

MIDI is handled by a built-in software synthesizer based on `rustysynth`.

`MidiOutput` accepts WIE MIDI events, feeds a `SoftwareMidiSynth`, and mixes rendered synthesizer output into the same Libretro audio buffer as PCM.

SoundFont selection:

1. if `wie_midi_soundfont` names a valid `.sf2` under the frontend system directory `wie/sf2`, load it,
2. otherwise use the embedded `assets/sines.sf2` fallback.

MIDI can be hot-enabled or hot-disabled with the `wie_midi` core option. Shutdown sends all-sound-off/all-notes-off style cleanup across all 16 channels before dropping the synth.

## Filesystem and Database Storage

The base directory is:

```text
<frontend save directory>/wie
```

If the frontend save directory is unavailable, the core falls back to the current directory or the temporary directory.

Layout:

```text
<save>/wie/<aid>/fs/<guest_path...>
<save>/wie/<aid>/db/<db_name>/<record_id>
```

`LibretroFilesystem` rejects path traversal, absolute paths, Windows prefixes, empty guest paths, and invalid app IDs. It uses synchronous `std::fs` operations behind the async backend trait, matching the existing CLI approach.

`LibretroDatabaseRepository` stores RMS/database records as one file per record ID. Record IDs start at 1 and `next_id` finds the first free numeric slot.

## Core Options

Options are registered through the newest frontend-supported interface:

1. `RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL`
2. `RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2`
3. `RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL`
4. `RETRO_ENVIRONMENT_SET_CORE_OPTIONS`
5. legacy `RETRO_ENVIRONMENT_SET_VARIABLES`

Supported options:

| Key | Values | Default | Hot update |
|---|---|---|---|
| `wie_resolution` | `240x320`, `176x208`, `128x160`, `128x128` | `240x320` | reload required |
| `wie_runtime` | `auto`, `ktf`, `lgt`, `skt`, `j2me` | `auto` | reload required |
| `wie_midi` | `on`, `off` | `on` | yes |
| `wie_midi_soundfont` | `builtin` or discovered `.sf2` files | `builtin` | reload required |

SoundFonts are discovered in:

```text
<frontend system directory>/wie/sf2
```

The option metadata has English, Simplified Chinese, and Traditional Chinese strings.

## Build Features

The `hooks` Cargo feature controls game-specific binary patches in `wie_core_arm`.

Default build:

```sh
cargo build -p wie_libretro --release
```

Build with hooks enabled:

```sh
cargo build -p wie_libretro --release --features hooks
```

The feature chain is:

```text
wie_libretro/hooks
  ├─ wie_ktf/hooks
  │    └─ wie_core_arm/hooks
  └─ wie_lgt/hooks
       └─ wie_core_arm/hooks
```

When `wie_core_arm/hooks` is disabled, `install_binary_patches` is compiled as a no-op returning `Ok(0)`. When enabled, `wie_core_arm` includes `data/binary_patches.toml` patterns and installs matching hooks/patches during runtime initialization.

## Platform Build Commands

Desktop Libretro core:

```sh
cargo build -p wie_libretro --release
cargo build -p wie_libretro --release --features hooks
```

Android x86_64:

```sh
cargo ndk -t x86_64 -P 29 build -p wie_libretro --release
cargo ndk -t x86_64 -P 29 build -p wie_libretro --release --features hooks
```

Android aarch64:

```sh
cargo ndk -t arm64-v8a -P 29 build -p wie_libretro --release
cargo ndk -t arm64-v8a -P 29 build -p wie_libretro --release --features hooks
```

Typical artifacts:

| Target | Artifact |
|---|---|
| Windows desktop | `target/release/wie_libretro.dll` |
| Linux desktop | `target/release/libwie_libretro.so` |
| Android x86_64 | `target/x86_64-linux-android/release/libwie_libretro.so` |
| Android aarch64 | `target/aarch64-linux-android/release/libwie_libretro.so` |

## Error Handling and Safety

- FFI callbacks are guarded against panics escaping Rust.
- Worker panics are caught and reported through `Shared::fatal`.
- `Platform::exit()` sets `quit` and unwinds with a private `GuestExit` marker, which is handled by the worker boundary.
- `retro_run` logs fatal worker errors but does not request frontend shutdown.
- Video callbacks are called without holding the framebuffer mutex.
- Filesystem paths are normalized before touching host storage.
- Unsupported Libretro memory and serialization APIs return null/zero/false.

## Known Limitations

- No savestate, rewind, run-ahead, fast-forward, or netplay.
- ZIP is the only accepted content container.
- Resolution and runtime changes require reloading content.
- Guest execution is wall-clock driven, not frame deterministic.
- MIDI quality depends on the selected SoundFont; the embedded fallback prioritizes portability over accurate device reproduction.
- The ARM CPU is interpreted in software, so host single-thread performance strongly affects runtime speed.

## Verification

Recommended checks before release builds:

```sh
cargo fmt
cargo clippy --workspace
cargo test --workspace
```
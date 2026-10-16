<p align="center">
  <img src="docs/assets/glue5-logo.png" alt="Glue 5 logo" width="160" height="160">
</p>

<h1 align="center">Glue 5</h1>

Glue 5 is a recompilation toolchain and native compatibility layer for
GTA V's WebAssembly build. It turns user-supplied game code into a native
application and connects it to the device's graphics, audio, storage and
controls. The game runs locally on iOS and macOS. Phones, tablets and
computers share the same code base; more platforms are planned.

The game's code is compiled for the target device when you build the app.
It runs without just-in-time (JIT) compilation or streaming from another
computer. Improvements to the shared runtime benefit both mobile and
desktop builds.

**This project provides only the recompilation tools and compatibility
layer. It does not include or distribute ANY GTA V assets or game binaries.**
Users must supply the required files from the supported **WebAssembly
(WASM) version of the game**: `game.wasm` at build time, and the prepared
game assets and translated shaders at run time. Files from a regular PC
or console installation alone are not sufficient.

`game.wasm` is translated to C with [wasm2c] at build time, compiled with the
platform's C compiler and linked into a Rust host that provides the
Emscripten/WASI imports, threads, file access, input, audio and GPU support.

**Status:** the story mode runs on macOS and iOS devices (wgpu on Metal,
with sound); keyboard, mouse, gamepads and touch controls work. The
Android app builds (wgpu on Vulkan); it has not run on a device yet. The
Linux build and AppImage (wgpu on Vulkan, X11 and Wayland) are set up but
not yet built or run.

See [design.md](docs/design.md) for architecture, tradeoffs and known limits.

<p align="center">
  <img src="docs/screenshots/ios-gameplay.png" alt="GTA V running in Glue 5 on iOS, driving through Los Santos with on-screen touch controls" width="100%">
</p>

<p align="center">
  <img src="docs/screenshots/ios-launcher.jpg" alt="Glue 5 start page on iOS with Play, Settings and Find Files buttons and game files ready" width="100%">
</p>

## Layout

| Path | What |
|---|---|
| `crates/glue5-game` | The game module. Its build script verifies `game.wasm`, runs wasm2c, applies hooks and compiles the C. |
| `crates/glue5-runtime` | Host runtime: Emscripten/WASI imports, threads, VFS, input, audio output, the GPU command ring (and a null backend). |
| `crates/glue5-gpu` | The renderer: executes the engine's D3D11-style command ring with wgpu (Metal on Apple platforms, Vulkan elsewhere). |
| `crates/glue5` | The application (SDL3), the same on every platform: window, input, audio, settings and touch controls (egui overlay). Its `headless` example runs the game without a window. |
| `platform/ios` | What iOS needs around it: `main.c`, entitlements, the xcodegen spec. |
| `platform/android` | The Gradle project around it: manifest, the activity (SDL's, subclassed), the adaptive icon. |
| `platform/macos` | Extra bundle metadata; cargo-packager configuration lives in `crates/glue5/Cargo.toml`. |
| `platform/linux` | The container the AppImage is built in (`Dockerfile`). |
| `.mise` | [mise]: the toolchain (`config.toml`) and the developer tasks (`tasks/`): setup, desktop, iOS, Android and Linux. |
| `patches` | Our patches to wasm2c, wgpu, wgpu-core, wgpu-hal and sp-fidelity, applied by `mise run setup` ([README](patches/README.md)). |
| `third_party/wabt` | wasm2c's sources, pinned (a submodule). `third_party/wgpu`, `third_party/wgpu-core`, `third_party/wgpu-hal` and `third_party/sp-fidelity`, the patched crates, are made by setup and not in git. |

Symlink your game directory
(`b/<build>/game.wasm`, `b/<build>/shaders/`, `data/`) to `./game`, or point
`GLUE5_GAME_DIR` at it. The build checks `game.wasm` against the supported
build.

## Building

Requirements: [mise] and a C/C++ compiler; for iOS, Xcode; for Windows
and Linux, [see below](#windows). mise installs
Rust, CMake and [xcodegen] (`.mise/config.toml`) on first use, and for
Android the JDK, Gradle, the SDK's command-line tools and cargo-ndk; the
Android tasks install the SDK packages they need (platform, build tools,
NDK: versions in `platform/android/gradle.properties`) with sdkmanager.

```sh
mise run play          # the first build compiles ~1 GB of generated C
```

Every task first runs `mise run setup`, which prepares wgpu-core, wgpu-hal
and wasm2c with our patches (only when they change). Run cargo by hand
through mise too: in a shell with `mise activate`, or `mise exec -- cargo ...`.

Compiled C objects are cached in `target/glue5-tools/objcache`, shared by
debug and release builds; later builds only recompile what changed.

### Windows

The MSVC toolchain (`x86_64-pc-windows-msvc`): no MinGW or MSYS compiler.
Besides mise, install:

- Visual Studio Build Tools 2022 or later, with *Desktop development with
  C++* (MSVC and the Windows SDK): Rust's linker, SDL, wasm2c.
- LLVM, for `clang-cl` on the `PATH` (e.g. `scoop install llvm`): the
  game's generated C is compiled with it, against MSVC's headers and
  runtime. `cl.exe` truncates the engine's longest names. `CC` overrides it.
- Git for Windows (the full one, not MinGit), whose bash runs the tasks.
  Point mise at it, ahead of WSL's `bash.exe`: set `MISE_BASH_PATH` to its
  `bin\bash.exe` in your user environment.
- Developer Mode (Settings › For developers), so mise can link tools.

The repository checks out with LF line endings on every platform
(`.gitattributes`): the tasks are bash scripts and the patches apply to LF
sources. Link the game directory with a junction
(`New-Item -ItemType Junction game -Target <dir>`) or set `GLUE5_GAME_DIR`.
Rendering is wgpu on Vulkan: the GPU driver provides it.

### Linux

Clang compiles the game's generated C (`CC` overrides it), and SDL is built
against the headers of the backends it supports: install them before the
first build, or SDL leaves those out (no window without X11 or Wayland's).
On Debian and Ubuntu, the list in `platform/linux/Dockerfile`; on other
distributions, SDL's `docs/README-linux.md`. SDL loads X11 or Wayland
(Wayland when the compositor has `fifo-v1`, X11 or XWayland otherwise;
`SDL_VIDEO_DRIVER=x11` or `wayland` forces one), the sound server and udev
at run time. Rendering is wgpu on Vulkan, from the GPU driver.

`mise run linux:package` builds `target/package/glue5_<version>_<arch>.AppImage`
with cargo-packager (it downloads linuxdeploy on first use). The AppImage
holds the executable and the icon only: glibc, Vulkan and the libraries SDL
loads are the system's, so it runs on systems with the glibc it was built
against or later. `mise run linux:container [x86_64|aarch64]` builds it in
`platform/linux/Dockerfile` (Ubuntu 22.04, glibc 2.35) with Docker, from any
system, its build tree in `target/linux-<arch>`. Give Docker the memory for
up to eight C compilations of ~15 MB files at once.

The game is looked for in `game/` beside the AppImage, as beside the Mac app.

## Tasks

| What | Command |
|---|---|
| Play (desktop) | `mise run play` |
| macOS: build and open the app bundle | `mise run macos` |
| macOS: build only the app bundle | `mise run macos:build` |
| macOS: open the built app again | `mise run macos:run` |
| macOS: build a distributable disk image | `mise run macos:package` |
| Linux: build the AppImage (on Linux) | `mise run linux:package` |
| Linux: build the AppImage in a container (Docker), on any system | `mise run linux:container [x86_64\|aarch64]` |
| Run without a window: scripted input, PNG shots, statistics | `mise run headless --press 30 start --for 40 --shots 300 shots` (`mise run headless -- --help` lists the options) |
| Tests | `mise run test`; every translated shader through the adapter and naga's validator: `mise run test:shaders` |
| Merge recorded pipelines into the app's seed | `mise run shaders:seed <pipelines.jsonl>...` |
| Record pipelines on automated runs (benchmark, map tours, graphics settings) into the seed | `mise run shaders:collect [default low min mid high max]` |
| iOS: build, sign, install, launch on the connected device | `mise run ios [device]` |
| iOS: only check that the app builds | `mise run ios:build` |
| iOS: launch the installed app again, with its console | `mise run ios:run [device]` |
| iOS: copy the game into the app's Documents/game | `mise run ios:game [device]` |
| Android: build, install, launch on the connected device with its log | `mise run android [device]` |
| Android: only build the APK | `mise run android:build` |
| Android: launch the installed app again, with its log | `mise run android:run [device]` |
| Android: copy the game into the app's Android/data/local.glue5/files/game | `mise run android:game [device]` |

`mise tasks` lists them all. iOS signing uses the team in `GLUE5_TEAM`:
put it in `.mise/config.local.toml` (ignored by git) under `[env]`. With several
phones connected, pass the device (name, identifier or UDID); for Android,
its serial number (`adb devices`).

### macOS packaging

`mise run macos:build` creates `target/package/Glue 5.app` for the Mac's
architecture; `mise run macos:package` also creates a DMG in `target/package`.
These tasks require Xcode 26+ for the shared Icon Composer icon. mise manages
cargo-packager alongside the other tools in `.mise/config.toml` on macOS
and Linux.
Direct Cargo builds and the packaged app both target macOS 14+, configured in
`.cargo/config.toml`. There is no bundle signing, Developer ID or notarization
step; the executable retains the linker-generated ad-hoc signature on Apple
Silicon. Downloaded unsigned distributions may be blocked by Gatekeeper.

Game data and shaders are **not bundled**. Choose the external game directory
on the start page, or put a `game` directory beside `Glue 5.app`. The executable
still contains the game code compiled from the build-time `game.wasm`.
Saves and settings remain in the user's Application Support directory.

`mise run macos` builds and opens the bundle through Launch Services; an
already running instance is brought forward. For a fresh launch, quit it
first. Arguments can be passed, e.g. `mise run macos --play /absolute/game`.
`mise run play` remains the direct Cargo workflow with terminal logs.

The bundle declares the Games category and Game Mode support. On Apple
Silicon, enter native macOS fullscreen using the green window button and
check Game Mode in the system game menu/overlay. The OS decides whether to
activate it; the plist alone does not guarantee activation. See Apple's
[Game Mode requirements](https://support.apple.com/en-us/105118).

### Runtime options

The app and headless runner accept these environment variables.
`mise run ios` and `ios:run` forward `RUST_LOG` and `GLUE5_*` variables
to the device; so do `mise run android` and `android:run` (as the launch
intent's extras).

| Variable | Effect |
|---|---|
| `GLUE5_GAME_DIR` | The game directory, for the build and at run time (`./game` under cargo). |
| `RUST_LOG` | Log filter (default `info`): renderer statistics every 10 s (frame times, CPU time per renderer stage, memory); `glue5::audio=debug` adds the sound device's. |
| `GLUE5_ENGINE_LOG=stdout` | The engine's log on the terminal too (it is always in `<user dir>/game.log`). |
| `GLUE5_ENGINE_ARGS` | More engine switches, e.g. `-FrameQueueLimit=2`. |
| `GLUE5_GPU_PROFILE=1` | GPU time per pass in the statistics. |
| `GLUE5_PLAY=1` | Start the game without the start page (as `--play`; on a phone too). |
| `GLUE5_NO_BC=1`, `GLUE5_NO_FLOAT32_FILTER=1` | Mobile GPU fallbacks on a desktop GPU: BC textures decoded on the CPU (and re-encoded as ETC2 where the GPU has it), 32-bit float textures unfiltered. |
| `GLUE5_NO_ETC2=1` | Decoded BC textures stay RGBA8 instead of ETC2. |
| `GAMERESOURCEMB`, `KEEPTHREADS` | The engine's module settings (`gameResourceMB`, `keepThreads`), read like the browser build reads them under Node. |
| `GLUE5_ALLOW_UNKNOWN_BUILD=1` | Build against a `game.wasm` other than the supported one. |
| `GLUE5_TEAM` | Apple development team id, to sign the iOS app. |

## Controls

Keyboard, mouse and controllers work as on PC, including the game's
controller prompts. Click to capture the pointer; Esc releases it. On
touch screens, an on-screen controller appears while touch is in use and
hides when a controller or keyboard is used. The game's text boxes bring
up the system keyboard on touch devices.

The start page shows the game files, Story mode, GTA Online, Sandbox (the
whole map, free roam, no missions) and Settings; the last mode played comes
first. App settings cover resolution, upscaling, VSync, frame rate, touch
controls, low memory, the game folder and GTA Online's name and private
lobby. During play, open them with F1, the ⚙ button on
touch screens, or by holding Back + Start on a controller. Use the D-pad or arrow keys to
navigate, A or Enter to select, and B or Esc to go back.

Esc / Start opens the game's own menu. Graphics quality is configured in
Settings › Graphics and saved in `settings.xml`. Confirming a change that
requires a restart, such as texture quality, ends the game. On desktop,
the app restarts directly into the game. On a phone, reopen the app.

## GTA Online

GTA Online's freemode runs without Rockstar's services and without a
server of ours. GTA ONLINE on the start page puts the player in the lobby
of everyone playing, where the game's own matchmaking finds or hosts a
session; Settings › Game › Private lobby takes a code instead, to play with
whoever enters the same one. The name other players see is in the same tab.

- The lobby is a virtual LAN the engine plays on as on a real one
  (`vnet.rs`): its sockets carry their datagrams over one UDP socket per
  player (`p2p.rs`, port 47000 to 47015, or another if all are taken).
- Players find each other on the same network by broadcast, and on the
  Internet through the BitTorrent DHT (`dht.rs`), where each announces
  itself under the lobby's key. Behind NATs both send until the NATs let
  them through; players who cannot reach each other go through one who
  can.
- Characters are kept in the user directory, `cloud/` (`cloud.rs`, with the
  stats the game's data lacks: `stats.rs`); the gamer id in `online-id`.

## Renderer

The engine emits D3D11-style commands into a ring in wasm memory (the browser
build's `wgpu_worker.js` protocol); `glue5-gpu` executes them in order on one
thread. The translated WGSL shaders (`shaders/`) are adapted at load time
(constant buffers packed into one dynamic uniform binding per stage, vertex
outputs given the pixel shader's interpolation) and compiled, with render
pipelines, on background threads. Draws whose pipeline is still compiling
are skipped (the headless runner's `--sync-pipelines` compiles them on the
spot instead).

The back buffer follows the window: its size in pixels times the render
scale (Settings › Resolution). The present step scales it to the window
using MetalFX on supported Apple GPUs or a bicubic filter otherwise
(Settings › Upscaling). The app's UI is drawn afterwards at the window's
resolution. With a frame rate set, Apple platforms show frames at that pace,
each for the same time ([frame pacing](docs/design.md#frame-pacing)): on
120 Hz displays 40 is offered too.

Pipeline recipes are recorded in `<user dir>/cache/pipelines.jsonl`. Before
the game starts, Play compiles the pipelines known for it (the app's seed,
`crates/glue5-gpu/seed/`, and the recorded ones) while the start page shows
how far it is: minutes on a phone's first run (an iPhone 13 Pro takes ~200 ms
of CPU per pipeline the system has not seen), about a second after, when
Metal's system cache or the saved Vulkan pipeline cache has them. Recipes
are the same on every device (each makes its own pipeline of one), so the
seed is grown on a computer: `mise run shaders:collect` runs the game
headless at levels of the graphics settings (a new player's, the low-memory
devices', minimum, middle, high, maximum), each time through the game's
benchmark (a fly-through of the map) and a tour of the map
(`crates/glue5/examples/tour.txt`: interiors, places at several hours and
weathers); the two shader qualities also tour a grid over the whole map (a
stop every 400 m, the sea skipped). It merges what they drew into the seed
(about 80 minutes). Recordings from playthroughs on devices go in with
`mise run shaders:seed <pipelines.jsonl>...`.

## Platforms

All platforms use `crates/glue5`. The desktop binary calls `glue5::main`.
On iOS, an Xcode project generated by [xcodegen] from
`platform/ios/project.yml` links the crate as a static library. On Android,
as SDL's README-android describes, the Gradle project in `platform/android`
packages the crate as a shared library (`libglue5.so`, built with
cargo-ndk), SDL's own (`libSDL3.so`) and SDL's Java activity, taken from the
`sdl3-src` crate the libraries are built with; the activity calls `SDL_main`.

The app looks for the game in this order: the command line (desktop),
`GLUE5_GAME_DIR`, the saved location in `glue5.json`, then platform defaults.
On desktop, the default is `game` next to the executable or in the
repository. On iOS, it is `Documents/game`, populated with
`mise run ios:game`, Finder > device > Files, or the Files app. On Android,
it is `Android/data/local.glue5/files/game`, populated with
`mise run android:game`.

If the files are missing, the start page explains where to put them.
Desktop users can choose a folder; on iOS, the app checks again when it
returns to the foreground. `glue5 --play`, or a game folder passed on the
command line, skips the start page.

## Hooks

`crates/glue5-game/hooks.rs` lists game functions the host replaces, by their
wasm name. Signatures come from the module; an unset hook keeps the original,
and a hook can call the original through `glue5_game::orig`.

## Future work

- **Give freed wasm memory back to the system.** The wasm memory is only
  reserved (16 GiB of address space); its pages count once the engine writes
  them, and then for good: wasm has no `munmap` or `madvise`, so memory the
  engine frees stays in the footprint. Most of the ~1.3 GB written is the
  engine's own data (macOS, low memory, early in story mode: the resource
  heap at 1023 MiB with ~770 in use, ~500 MB of other heaps). Hooks on
  `rage::sysMemGrowBuddyAllocator::Free` (its bookkeeping looks to be kept
  outside the blocks, which needs checking) and on large frees of the game's
  malloc (the pages inside the chunk) could hand whole freed pages back
  (`MADV_FREE_REUSABLE` on Apple platforms, `MADV_DONTNEED` elsewhere): at
  most ~250 MB there, less where free space was never written. No compiler
  or wasm flag changes how much the engine writes.

## Notes

- Game data must be the copies prepared for the browser build (re-keyed archives).
- The user directory (saves, `glue5.json`, the engine log `game.log`, the
  renderer's caches in `cache/`) is SDL's per-user application directory:
  `~/Library/Application Support/glue5` on macOS, `~/.local/share/glue5` on
  Linux, `%APPDATA%\glue5` on Windows.

[wasm2c]: https://github.com/WebAssembly/wabt/tree/main/wasm2c
[xcodegen]: https://github.com/yonaskolb/XcodeGen
[mise]: https://mise.jdx.dev

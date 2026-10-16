//! Host runtime for the recompiled game: what the browser build got from
//! Emscripten's JS glue, game.js, the workers and the page.
//!
//! [`start`] reserves the wasm memory, installs the imports and runs the
//! engine's `main` on its own thread. Engine threads (pthreads) become native
//! threads, each with its own instance over the shared memory.

pub mod audio;
mod audio_update;
mod cloud;
pub mod cpu;
mod device_reset;
mod dht;
pub mod exit;
pub mod game_settings;
mod gamer_tags;
mod gamelog;
pub mod gpu;
mod granular;
mod imports;
pub mod input;
pub mod memory;
mod online;
mod overlay;
mod p2p;
pub mod qos;
mod readback;
mod scenario;
mod shader_settings;
mod shadow_reveal;
mod script_assets;
mod skin;
mod stats;
mod threads;
mod time;
pub mod tour;
mod trap_site;
mod upscale;
pub mod vfs;
mod vnet;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use anyhow::{Context, Result};

pub use glue5_game::GAME_BUILD;

/// The CPU cores the process may use, asked once: on Linux and Android the
/// standard library reads the cgroup quotas each time, which Android's
/// sandbox denies (and logs).
pub fn cores() -> usize {
    static CORES: OnceLock<usize> = OnceLock::new();
    *CORES.get_or_init(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
}

/// The game directory's layout, as the whale5 server serves it: the engine's
/// files under `data/`, this build's translated shaders under
/// `b/<build>/shaders/`.
pub mod layout {
    use std::path::{Path, PathBuf};

    pub fn data_dir(game: &Path) -> PathBuf {
        game.join("data")
    }

    pub fn shader_dir(game: &Path) -> PathBuf {
        game.join("b").join(super::GAME_BUILD).join("shaders")
    }

    /// Whether `game` holds the game, for this build.
    pub fn is_game_dir(game: &Path) -> bool {
        data_dir(game).is_dir() && shader_dir(game).join("index.json").is_file()
    }
}

use crate::memory::{Mem, WasmMemory};
use crate::vfs::Vfs;

/// How the game is started.
#[derive(Clone, Debug)]
pub struct Config {
    /// The game directory: `b/<build>/` (shaders) and `data/`.
    pub game_dir: PathBuf,
    /// Where saves and settings go.
    pub user_dir: PathBuf,
    /// Back buffer size.
    pub width: u32,
    pub height: u32,
    /// 1 = story, 2 = sandbox (GTA V map), 3 = sandbox (env_test).
    pub start_mode: u32,
    /// Smaller engine pools (with [`game_settings::apply_low_preset`], the
    /// browser build's `?low=1`).
    pub low_memory: bool,
    /// The GPU memory budget in MiB (0: `GLUE5_GPU_BUDGET_MB`, else the
    /// phone's default): the engine's heap and the renderer's textures.
    pub gpu_budget_mb: Option<u32>,
    /// Keep the engine and its renderer in the GPU memory budget (off:
    /// unbounded, as before it).
    pub limit_gpu_memory: bool,
    /// Frames per second at most: 0 lets the engine run as fast as the
    /// renderer presents (the display's refresh with vsync); 60 or 30 use
    /// the engine's own limiter.
    pub max_fps: u32,
    /// The game's benchmark (its fly-through of the map) instead of the
    /// game; the engine writes the results in the user directory
    /// (`Documents/Rockstar Games/GTA V/Benchmarks`) and asks to quit.
    pub benchmark: bool,
    /// GTA Online in a lobby, instead of story mode or the sandbox.
    pub online: Option<Online>,
    /// The game's language, as a tag like `it-IT` (the engine matches the
    /// whole tag, then the language alone). `None`: `$LANG`, else English.
    pub language: Option<String>,
}

/// How the game plays online.
#[derive(Clone, Debug)]
pub struct Online {
    /// The player's name, as the others see it (16 characters at most).
    pub name: String,
    /// A private lobby's code; empty: the lobby of everyone, where the
    /// game's matchmaking puts the player in a session.
    pub lobby: String,
}

impl Config {
    /// The engine's command line, as the browser page builds it.
    pub fn engine_args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-rootdir=/game/",
            "-wgpu",
            "-nodisplaycalibration",
            "-noSocialClub",
            "-pc:nosocialclub",
            "-nonetwork",
            "-output",
        ]
        .map(String::from)
        .into();
        args.push(format!("-width={}", self.width));
        args.push(format!("-height={}", self.height));
        args.push("-forceResolution".into());
        args.extend(["-Script_all=warning", "-replay_all=warning", "-fastscriptcore"].map(String::from));
        // Resources stay where they were placed: some have their page map
        // outside them (glue5-game's build/page_map.rs), which a move would
        // look for inside them. The engine does not move them here anyway.
        args.push("-nodefrag".into());
        // This build is a BankRelease one: its debug pad took the first pad to
        // send input, and START and Back with it (START never opened the
        // pause menu, and after it the pad's other buttons went to the debug
        // tools). Pads as in a retail build.
        args.extend(["-disableDebugPadButtons", "-disallowdebugpad"].map(String::from));
        // The engine's limiter counts 60 Hz intervals per frame (0: off).
        let frame_limit = match self.max_fps {
            0 => 0,
            fps => (60 / fps.clamp(1, 60)).max(1),
        };
        args.push(format!("-frameLimit={frame_limit}"));
        args.push(format!("-wasmstartchoice={}", self.start_mode));
        if self.benchmark {
            args.extend(["-benchmark", "-benchmarkIterations=1"].map(String::from));
        }
        // No quality switches: they would override the game's own settings
        // (its menu) at every start.
        // More engine switches, for experiments.
        let extra: Vec<String> =
            std::env::var("GLUE5_ENGINE_ARGS").map(|e| e.split_whitespace().map(String::from).collect()).unwrap_or_default();
        // A second frame queued: with one, the engine's render thread waits
        // for the GPU to finish the previous frame before it starts the
        // next. The game's benchmark ran 11-32 % faster with two on Windows
        // and 8 % on a Pixel 8a, for a frame more of latency. iPhones gained
        // nothing (their CPU sets the pace): they keep one.
        let ios = cfg!(target_os = "ios");
        if !ios && !extra.iter().any(|a| a.to_ascii_lowercase().starts_with("-framequeuelimit")) {
            args.push("-FrameQueueLimit=2".into());
        }
        args.extend(extra);
        if self.online.is_some() {
            args.retain(|a| a != "-nonetwork");
            args.extend(online::ENGINE_ARGS.iter().map(|a| a.to_string()));
        }
        args
    }
}

/// Process-wide state shared by imports and host threads.
pub(crate) struct Runtime {
    pub config: Config,
    pub memory: WasmMemory,
    pub vfs: Vfs,
    pub clock: time::Clock,
    /// Addresses the engine publishes for the host (0 until published).
    pub input_block: AtomicU64,
    pub audio_ring: AtomicU64,
    pub gpu_ring: AtomicU64,
    /// Taken when the engine starts its GPU worker.
    pub gpu_backend: Mutex<Option<gpu::GpuBackend>>,
    pub threads: threads::Registry,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub(crate) fn rt() -> &'static Runtime {
    RUNTIME.get().expect("runtime not started")
}

/// A running game.
pub struct Game {
    engine: JoinHandle<i32>,
}

impl Game {
    /// Waits for the engine's `main` to return (it normally never does).
    pub fn join(self) -> i32 {
        self.engine.join().unwrap_or(1)
    }

    /// The wasm memory, for host subsystems (input, audio, GPU).
    pub fn mem(&self) -> Mem {
        rt().memory.mem()
    }

    /// The engine's audio output (silent until the engine has started its mixer).
    pub fn audio(&self) -> audio::AudioOut {
        audio::AudioOut
    }

    /// The engine's keyboard/mouse input block, once it has published it.
    pub fn input(&self) -> Option<input::InputBlock> {
        nonzero(rt().input_block.load(Ordering::Acquire)).map(|at| input::InputBlock::new(rt().memory.mem(), at))
    }
}

fn nonzero(v: u64) -> Option<u64> {
    (v != 0).then_some(v)
}

/// Starts the game, rendering with `gpu` (e.g. [`gpu::null`]). Only one game
/// can run per process.
pub fn start(config: Config, gpu: gpu::GpuBackend) -> Result<Game> {
    let data = layout::data_dir(&config.game_dir);
    std::fs::create_dir_all(&config.user_dir)?;
    let log = gamelog::start(&config.user_dir).context("creating the engine log")?;
    log::info!("engine log: {} (GLUE5_ENGINE_LOG=stdout also prints it)", log.display());
    glue5_game::set_trap_hook(trap_site::record);
    time::fine_timers();
    let vfs = Vfs::new(data.clone(), config.user_dir.clone()).with_context(|| format!("opening {}", data.display()))?;
    let memory = WasmMemory::reserve()?;
    let runtime = Runtime {
        config,
        memory,
        vfs,
        clock: time::Clock::new(),
        input_block: AtomicU64::new(0),
        audio_ring: AtomicU64::new(0),
        gpu_ring: AtomicU64::new(0),
        gpu_backend: Mutex::new(Some(gpu)),
        threads: threads::Registry::default(),
    };
    if RUNTIME.set(runtime).is_err() {
        anyhow::bail!("the game is already running");
    }
    // Each module sets the hooks it implements (glue5-game's hooks/).
    let mut hooks = glue5_game::Hooks::default();
    input::hooks(&mut hooks);
    exit::hooks(&mut hooks);
    gamelog::hooks(&mut hooks);
    memory::hooks(&mut hooks);
    skin::hooks(&mut hooks);
    shader_settings::hooks(&mut hooks);
    shadow_reveal::hooks(&mut hooks);
    device_reset::hooks(&mut hooks);
    readback::hooks(&mut hooks);
    audio_update::hooks(&mut hooks);
    granular::hooks(&mut hooks);
    script_assets::hooks(&mut hooks);
    gamer_tags::hooks(&mut hooks);
    scenario::hooks(&mut hooks);
    tour::hooks(&mut hooks);
    upscale::hooks(&mut hooks);
    if let Some(online) = &rt().config.online {
        online::start(online, &mut hooks);
    }
    glue5_game::install(glue5_game::imports_from!(imports), hooks);
    let engine = threads::spawn_main()?;
    Ok(Game { engine })
}

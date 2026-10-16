//! The game without a window, for tests and measurements: the renderer
//! draws offscreen, input comes from a script.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use glue5_runtime::Game;
use glue5_runtime::audio::{CHANNELS, SAMPLE_RATE};
use glue5_runtime::input::{self, Gamepad, button};

/// The app's allocator (see src/main.rs), so measurements match it.
#[cfg(windows)]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The game without a window, for tests and measurements.
///
/// The run ends after --for, when the game exits or asks to restart (its
/// menu), or, with status 1, when it stops working (an engine thread's
/// trap). Presses hold for 0.1 s (menus repeat a key held longer).
#[derive(Parser)]
struct Args {
    /// End after S seconds.
    #[arg(long = "for", value_name = "S")]
    run_for: Option<f64>,
    /// Press a pad button at S seconds: start, select, south or a, east or
    /// b, west or x, north or y, l1, r1, l2, r2, l3, r3, up, down, left,
    /// right.
    #[arg(long, num_args = 2, value_names = ["S", "BUTTON"])]
    press: Vec<String>,
    /// Press a key at S seconds: esc, enter, tab, backspace, space, grave,
    /// shift, up, down, left, right, a letter or a digit.
    #[arg(long, num_args = 2, value_names = ["S", "KEY"])]
    key: Vec<String>,
    /// Also take commands from standard input, one per line: press
    /// <button>, key <key>, hold <key> <s>, text <text>, size <w>x<h> (the
    /// back buffer, as a window resize).
    #[arg(long)]
    stdin: bool,
    /// Every N-th presented frame to a PNG in DIR.
    #[arg(long, num_args = 2, value_names = ["N", "DIR"])]
    shots: Vec<String>,
    /// The back buffer's size.
    #[arg(long, value_name = "WxH", default_value = "1280x720", value_parser = parse_size)]
    size: (u32, u32),
    /// Present the frames as into a window of this size, scaled by
    /// --upscaler (with --shots, the presented frames too: outNNNNNN.png).
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    output: Option<(u32, u32)>,
    /// With --output: bicubic, metalfx, metalfx-temporal or fsr.
    #[arg(long, value_name = "NAME", default_value = "bicubic", value_parser = parse_upscaler)]
    upscaler: glue5_gpu::Upscaler,
    /// Frames per second at most, as the app's setting (the engine's
    /// limiter; 0: as fast as it can).
    #[arg(long, default_value_t = 0)]
    fps: u32,
    /// The low-memory profile (smaller pools, its graphics preset written
    /// into the game's settings).
    #[arg(long)]
    low: bool,
    /// A graphics setting written into the game's settings (settings.xml
    /// names, e.g. ShadowQuality=3), after --low.
    #[arg(long = "setting", value_name = "NAME=V", value_parser = parse_setting)]
    settings: Vec<(String, String)>,
    /// Start in the sandbox (the map, no story) instead of the story mode.
    #[arg(long)]
    sandbox: bool,
    /// GTA Online, as this player name.
    #[arg(long, value_name = "NAME")]
    online: Option<String>,
    /// With --online: a private lobby instead of everyone.
    #[arg(long, value_name = "CODE", requires = "online", default_value = "")]
    lobby: String,
    /// The game's benchmark (a fly-through of the map; the run ends with
    /// it, results in the user directory).
    #[arg(long)]
    benchmark: bool,
    /// Visit the map: S seconds at each stop of FILE (lines `x y z hour
    /// weather [# name]`, z `ground`: on the ground there), the camera
    /// turning; the run ends after the last (use with --sandbox).
    #[arg(long, num_args = 2, value_names = ["FILE", "S"])]
    tour: Vec<String>,
    /// Saves, settings and the pipeline cache (default: glue5-headless in
    /// the temporary directory).
    #[arg(long, value_name = "DIR")]
    user_dir: Option<PathBuf>,
    /// Play the sound into a pretend device, as the app does (without one
    /// the engine mixes on its own clock).
    #[arg(long)]
    audio: bool,
    /// Compile pipelines when first drawn, not skipping the draws meanwhile
    /// (shots comparable between runs).
    #[arg(long)]
    sync_pipelines: bool,
    /// No rendering at all.
    #[arg(long)]
    no_gpu: bool,
    /// The game directory.
    #[arg(long, value_name = "DIR", env = "GLUE5_GAME_DIR")]
    game: PathBuf,
}

fn parse_size(size: &str) -> Result<(u32, u32)> {
    let (w, h) = size.split_once('x').context("<width>x<height>")?;
    Ok((w.parse()?, h.parse()?))
}

fn parse_upscaler(name: &str) -> Result<glue5_gpu::Upscaler> {
    Ok(match name {
        "bicubic" => glue5_gpu::Upscaler::Bicubic,
        "metalfx" => glue5_gpu::Upscaler::MetalFx,
        "metalfx-temporal" => glue5_gpu::Upscaler::MetalFxTemporal,
        "fsr" => glue5_gpu::Upscaler::Fsr,
        _ => bail!("unknown upscaler {name}"),
    })
}

fn parse_setting(setting: &str) -> Result<(String, String)> {
    let (name, value) = setting.split_once('=').context("<name>=<value>")?;
    Ok((name.into(), value.into()))
}

/// How long a press holds its button or key, in seconds.
const HOLD: f64 = 0.1;

/// A key press: when (s), the key, whether it is down now, for how long (s).
type Key = (f64, u8, bool, f64);

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let mut presses = Vec::new();
    for p in args.press.chunks(2) {
        presses.push((p[0].parse::<f64>()?, parse_button(&p[1])?));
    }
    let mut keys = Vec::new();
    for k in args.key.chunks(2) {
        keys.push((k[0].parse::<f64>()?, parse_key(&k[1])?, false, HOLD));
    }
    let shots = match &args.shots[..] {
        [every, dir] => Some((every.parse::<u32>()?, PathBuf::from(dir))),
        _ => None,
    };
    let tour = match &args.tour[..] {
        [path, stay] => {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
            Some((glue5_runtime::tour::parse(&text)?, Duration::from_secs_f64(stay.parse()?)))
        }
        _ => None,
    };
    let user_dir = args.user_dir.unwrap_or_else(|| std::env::temp_dir().join("glue5-headless"));
    let (game_dir, low) = (args.game, args.low);
    if low {
        glue5_runtime::game_settings::apply_low_preset(&user_dir)?;
    }
    if !args.settings.is_empty() {
        glue5_runtime::game_settings::apply(&user_dir, &args.settings)?;
    }
    let gpu = if args.no_gpu {
        glue5_runtime::gpu::null()
    } else {
        let mut settings = glue5_gpu::Settings::for_game(&game_dir);
        settings.cache_dir = Some(user_dir.join("cache"));
        settings.sync_pipelines = args.sync_pipelines;
        if let Some((every, dir)) = &shots {
            std::fs::create_dir_all(dir)?;
            settings.shot_every = *every;
            settings.shot_dir = Some(dir.clone());
        }
        let screen = match args.output {
            Some(size) => {
                let presentation = glue5_gpu::Presentation::new(size, false, args.upscaler);
                Some(glue5_gpu::Screen::offscreen(presentation, shots.clone())?)
            }
            None => None,
        };
        glue5_gpu::backend(settings, screen)
    };
    // The camera turns all along the tour, as fast as the stick turns it.
    let spin = if tour.is_some() { [1.0, 0.0] } else { [0.0; 2] };
    if let Some((stops, stay)) = tour {
        log::info!("tour: {} stops, {} s each", stops.len(), stay.as_secs_f64());
        glue5_runtime::tour::start(stops, stay);
    }
    let game = glue5_runtime::start(
        glue5_runtime::Config {
            game_dir,
            user_dir,
            width: args.size.0,
            height: args.size.1,
            start_mode: if args.sandbox { 2 } else { 1 },
            low_memory: low,
            gpu_budget_mb: None,
            max_fps: args.fps,
            benchmark: args.benchmark,
            online: args.online.map(|name| glue5_runtime::Online { name, lobby: args.lobby }),
            language: None,
        },
        gpu,
    )?;
    if args.audio {
        let out = game.audio();
        std::thread::spawn(move || {
            // The app's sound device: 256 frames at a time.
            const PERIOD: usize = 256;
            let mut buf = [0f32; PERIOD * CHANNELS];
            let (start, mut frames) = (Instant::now(), 0);
            loop {
                frames += PERIOD;
                let due = start + Duration::from_secs_f64(frames as f64 / SAMPLE_RATE as f64);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                out.read(&mut buf);
            }
        });
    }
    let (live_tx, live) = std::sync::mpsc::channel::<String>();
    if args.stdin {
        std::thread::spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                let _ = live_tx.send(line);
            }
        });
    }
    let t0 = Instant::now();
    let mut focused = false;
    loop {
        let t = t0.elapsed().as_secs_f64();
        for line in live.try_iter() {
            if let Err(e) = command(&line, t, &game, &mut presses, &mut keys) {
                log::warn!("{line}: {e}");
            }
        }
        if !focused && let Some(input) = game.input() {
            input.focus(true); // without focus the engine ignores input
            focused = true;
        }
        if let Some(input) = game.input() {
            for (at, vk, down, hold) in &mut keys {
                let held = (*at..*at + *hold).contains(&t);
                if held != *down {
                    input.key(*vk, held);
                    *down = held;
                }
            }
        }
        let buttons = presses.iter().filter(|(at, _)| (*at..at + HOLD).contains(&t)).fold(0, |b, (_, bits)| b | bits);
        input::set_gamepad(0, Gamepad { connected: true, buttons, right: spin, ..Default::default() });
        if args.run_for.is_some_and(|limit| t >= limit) {
            std::process::exit(0);
        }
        if let Some(exit) = glue5_runtime::exit::requested() {
            log::info!("the engine asked for {exit:?} at {t:.1} s");
            std::process::exit(0);
        }
        if glue5_runtime::tour::finished() {
            log::info!("tour: done at {t:.1} s");
            std::process::exit(0);
        }
        if glue5_runtime::exit::failed().is_some() {
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A line from standard input, at `t` seconds.
fn command(line: &str, t: f64, game: &Game, presses: &mut Vec<(f64, u16)>, keys: &mut Vec<Key>) -> Result<()> {
    match line.split_whitespace().collect::<Vec<_>>()[..] {
        ["press", name] => presses.push((t, parse_button(name)?)),
        ["key", name] => keys.push((t, parse_key(name)?, false, HOLD)),
        ["hold", name, secs] => keys.push((t, parse_key(name)?, false, secs.parse()?)),
        ["text", ..] => game.input().context("no input yet")?.text(line.trim_start()["text".len()..].trim()),
        ["size", size] => {
            let (w, h) = size.split_once('x').context("size <width>x<height>")?;
            game.input().context("no input yet")?.resize(w.parse()?, h.parse()?);
        }
        _ => bail!("expected `press <button>`, `key <key>`, `hold <key> <s>`, `text <text>` or `size <w>x<h>`"),
    }
    Ok(())
}

fn parse_button(name: &str) -> Result<u16> {
    Ok(match name {
        "start" => button::START,
        "select" => button::SELECT,
        "south" | "a" | "cross" => button::SOUTH,
        "east" | "b" | "circle" => button::EAST,
        "west" | "x" | "square" => button::WEST,
        "north" | "y" | "triangle" => button::NORTH,
        "l1" => button::L1,
        "r1" => button::R1,
        "l2" => button::L2,
        "r2" => button::R2,
        "l3" => button::L3,
        "r3" => button::R3,
        "up" => button::UP,
        "down" => button::DOWN,
        "left" => button::LEFT,
        "right" => button::RIGHT,
        other => bail!("unknown button {other}"),
    })
}

/// Windows virtual-key codes.
fn parse_key(name: &str) -> Result<u8> {
    Ok(match name {
        "esc" => 0x1B,
        "enter" => 0x0D,
        "tab" => 0x09,
        "backspace" => 0x08,
        "space" => 0x20,
        "grave" => 0xC0,
        "shift" => 0x10,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric() => s.as_bytes()[0].to_ascii_uppercase(),
        other => bail!("unknown key {other}"),
    })
}

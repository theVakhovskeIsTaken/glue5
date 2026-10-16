//! The application: window, renderer, engine and the event loop.
//!
//! It opens on the start page with the renderer already running: the
//! game's files, Play, Settings. What applies when the engine starts (frame
//! rate, memory profile, the game folder, a phone's resolution) can be
//! changed there before it loads, and a game that fails while starting is
//! not started again by itself. Play starts the engine, which then owns the
//! frames; the application's UI goes over them. `glue5 --play` (a restart
//! the game asks for) starts the game without the start page.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use glue5_gpu::{Presentation, Screen, Upscaler};
use glue5_runtime::exit::Exit;
use glue5_runtime::input::{Gamepad, InputBlock, mouse};
use glue5_runtime::layout::is_game_dir;
use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::Scancode;
use sdl3::mouse::MouseButton;

use crate::haptics;
use crate::paths::{self, MOBILE};
use crate::settings::{self, PerfOverlay, Settings, TouchMode, Upscaling};
use crate::touch::{self, Touch};
use crate::ui::{self, Action, Mode, Page, Started, Status, Ui};
use crate::{audio, benchmark, keymap, pads};

/// The settings button's opacity over the game while the touch controls
/// are hidden (a controller in use, or touch off): there, but faint.
const GEAR_ALONE: f32 = 0.35;

/// In the user directory: why the last game stopped, for the start page
/// (gone once the game is started again).
const FAILURE_FILE: &str = "last-failure.txt";
/// How long the pointer waits to be captured once it should be (`App::pointer`).
const CAPTURE_DELAY: Duration = Duration::from_millis(150);

/// Esc's Windows virtual-key code: back, in the game's menus.
const VK_ESCAPE: u8 = 0x1B;

pub fn run() -> Result<()> {
    // SDL's event wait on Android pumps events, which queues the poll
    // sentinel, which wakes the wait: it spins for its whole timeout (a big
    // core at 100%). Without the sentinel it sleeps until an event; polling
    // then drains the queue, which is what this loop does anyway.
    #[cfg(target_os = "android")]
    sdl3::hint::set("SDL_POLL_SENTINEL", "0");
    let sdl = sdl3::init().map_err(|e| anyhow!("SDL: {e}"))?;
    let video = sdl.video().map_err(|e| anyhow!("SDL video: {e}"))?;
    if MOBILE {
        sdl3::hint::set("SDL_ORIENTATIONS", "LandscapeLeft LandscapeRight");
        // Touches drive the touch controls, not a pretend mouse.
        sdl3::hint::set("SDL_TOUCH_MOUSE_EVENTS", "0");
    }
    // The renderer sizes the window's Metal layer when it configures it for
    // the window. SDL resizing the layer as well (macOS), from inside the
    // system's resize loop, had frames drawn for one size shown at another.
    sdl3::hint::set("SDL_VIDEO_METAL_AUTO_RESIZE_DRAWABLE", "0");
    let user_dir = paths::user_dir()?;
    let mut settings = Settings::load(&user_dir);
    // The Vulkan pools' trimming is the process's choice before any device
    // opens (the Bifrost/Valhall reuse hazard is not worth "as before").
    settings.apply_env();
    // Before the GPU opens; turned on later, it shows from the next launch.
    let metal_hud = cfg!(target_vendor = "apple") && settings.overlay == PerfOverlay::MetalHud;
    if metal_hud {
        // SAFETY: Apple's libc locks the environment for getenv and setenv
        // (elsewhere this does nothing).
        unsafe { glue5_gpu::enable_metal_hud() };
    }
    let args = Args::parse();

    let mut builder = video.window("Glue 5", 1280, 720);
    builder.resizable().high_pixel_density();
    if MOBILE {
        builder.fullscreen();
    }
    if cfg!(target_vendor = "apple") {
        builder.metal_view();
    }
    let window = builder.build()?;
    let pixels = window.size_in_pixels();
    settings.for_screen(pixels.1);

    let presentation = Presentation::new(pixels, settings.vsync, upscaler(&settings));
    presentation.set_metal_hud(settings.overlay == PerfOverlay::MetalHud);
    let overlay = glue5_gpu::Overlay::default();
    let instance = glue5_gpu::instance();
    #[cfg(target_os = "android")]
    let surfaces = instance.clone();
    // The window outlives the renderer: the process exits with it.
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_display_and_window(&window, &window)?)?
    };
    let screen = Screen::open(glue5_gpu::Window {
        instance,
        surface,
        presentation: presentation.clone(),
        overlay: overlay.clone(),
    })?;
    // The adapter's family is only known now its device opened: tune the
    // defaults for it once, and the player's changes after are theirs.
    settings.tune_for_adapter(glue5_gpu::perf::gpu_name());
    settings.apply_env();
    settings.save(&user_dir);
    // While the system resizes the window in a loop of its own (macOS,
    // Windows), events wait for it to end, and so does this loop; SDL still
    // calls watches, and from there the renderer follows the window. The
    // game's resolution changes once, when this loop runs again. SDL sends
    // the application's lifecycle only to watches too, from the call that
    // learns of it (not through the queue): the loop follows it from there.
    let watched = presentation.clone();
    let lifecycle = Arc::new(Lifecycle::default());
    let life = lifecycle.clone();
    let _watch = sdl.event().map_err(|e| anyhow!("SDL events: {e}"))?.add_event_watch(move |event: Event| match event {
        Event::Window { win_event: WindowEvent::PixelSizeChanged(w, h), .. } => {
            watched.set_size(w.max(0) as u32, h.max(0) as u32)
        }
        Event::AppWillEnterBackground { .. } => life.background.store(true, Ordering::Relaxed),
        Event::AppDidEnterForeground { .. } => {
            life.background.store(false, Ordering::Relaxed);
            life.came_back.store(true, Ordering::Relaxed);
        }
        _ => {}
    });

    // Text input runs only while the game's text box is open (see `update`).
    let text_input = video.text_input();
    let refresh_rate = window_refresh_rate(&window);
    let mode = match () {
        _ if settings.online => Mode::Online,
        _ if settings.sandbox => Mode::Sandbox,
        _ => Mode::Story,
    };
    let last_benchmark = benchmark::last(&user_dir);
    let mut app = App {
        pads: pads::Pads::new(sdl.gamepad().map_err(|e| anyhow!("SDL gamepad: {e}"))?),
        mouse: sdl.mouse(),
        events: sdl.event_pump().map_err(|e| anyhow!("SDL events: {e}"))?,
        sdl,
        window,
        text_input,
        typing: false,
        keyboard_away: false,
        presentation,
        #[cfg(target_os = "android")]
        instance: surfaces,
        user_dir,
        settings,
        ui: Ui::new(overlay),
        touch: Touch::new(),
        touch_input: MOBILE,
        touch_shown: false,
        touch_mouse: Default::default(),
        lifecycle,
        folder: None,
        game_dir: None,
        problem: None,
        last_failure: None,
        graphics_reset: false,
        upscalers: UPSCALERS.iter().copied().filter(|u| screen.has_upscaler(u.upscaler())).collect(),
        frame_generation: screen.has_frame_generation(),
        metal_hud,
        refresh_rate,
        screen: Some(screen),
        play_requested: false,
        mode,
        benchmark: last_benchmark,
        capture_since: None,
        mouse_used: !MOBILE,
        game: None,
    };
    app.last_failure = std::fs::read_to_string(app.user_dir.join(FAILURE_FILE)).ok();
    app.find_game(args.game_dir.as_deref());
    if args.play && app.game_dir.is_some() {
        app.play()?;
    }
    app.run()
}

/// The command line (desktop): `glue5 [--play] [game folder]`; the start
/// page is skipped only with `--play` (a folder alone is where the game is:
/// a game that fails is not started again by itself).
struct Args {
    /// Start the game without the start page.
    play: bool,
    game_dir: Option<PathBuf>,
}

impl Args {
    fn parse() -> Args {
        let mut args = Args { play: false, game_dir: None };
        // Phones pass arguments of their own.
        for arg in std::env::args_os().skip(1).filter(|_| !MOBILE) {
            if arg == "--play" {
                args.play = true;
            } else {
                args.game_dir = Some(arg.into());
            }
        }
        // Measurements on a phone, whose screen nobody taps (`mise run ios:run`
        // passes GLUE5_* variables on).
        args.play |= std::env::var_os("GLUE5_PLAY").is_some();
        args
    }
}

/// The application's lifecycle, as SDL tells the event watch.
#[derive(Default)]
struct Lifecycle {
    /// In the background (phones: no GPU work there).
    background: AtomicBool,
    /// Back in the foreground since the loop last looked.
    came_back: AtomicBool,
}

struct App {
    sdl: sdl3::Sdl,
    window: sdl3::video::Window,
    events: sdl3::EventPump,
    mouse: sdl3::mouse::MouseUtil,
    text_input: sdl3::keyboard::TextInputUtil,
    /// Typed text is on: while the game's text box is open.
    typing: bool,
    /// The on-screen keyboard, which hides the touch controls, was put away
    /// with a tap above it: the game's text box takes A or B from them.
    keyboard_away: bool,
    pads: pads::Pads,
    presentation: Presentation,
    /// For the window's surfaces after the first.
    #[cfg(target_os = "android")]
    instance: wgpu::Instance,
    user_dir: PathBuf,
    settings: Settings,
    ui: Ui,
    touch: Touch,
    /// Touch is the input in use: the touch controls follow it (touching
    /// shows them, a controller or keyboard hides them; being connected is
    /// not enough, phones often have a paired controller lying around).
    touch_input: bool,
    touch_shown: bool,
    /// What the touch controls last did with the mouse and Esc (the pause
    /// menu's).
    touch_mouse: touch::Mouse,
    lifecycle: Arc<Lifecycle>,
    folder: Option<FolderPick>,
    /// The game's folder, once found.
    game_dir: Option<PathBuf>,
    /// Why it was not, or a folder chosen that is not one.
    problem: Option<String>,
    /// Why the last game stopped, if it did.
    last_failure: Option<String>,
    /// The game's graphics settings went back to their defaults.
    graphics_reset: bool,
    /// The upscalers this GPU has.
    upscalers: Vec<Upscaling>,
    /// It can generate frames.
    frame_generation: bool,
    /// Apple's Metal HUD can show: it was on at launch.
    metal_hud: bool,
    /// The display's refresh rate at most, in Hz.
    refresh_rate: f32,
    /// The renderer, until the game takes it over.
    screen: Option<Screen>,
    /// Play was chosen: the game starts once its pipelines are prepared.
    play_requested: bool,
    /// What Play starts.
    mode: Mode,
    /// The last benchmark's results.
    benchmark: Option<benchmark::Results>,
    /// Since when the pointer should be captured (see `pointer`).
    capture_since: Option<Instant>,
    /// A mouse has been used in the game: phones capture it only then.
    mouse_used: bool,
    game: Option<Running>,
}

/// The game, once started.
struct Running {
    game: glue5_runtime::Game,
    /// The engine's input block, once it has published it.
    input: Option<InputBlock>,
    started: Started,
    /// The back buffer size last asked of the engine.
    back_buffer: (u32, u32),
    _audio: Option<audio::Audio>,
}

impl App {
    fn run(mut self) -> Result<()> {
        loop {
            if let Some(exit) = glue5_runtime::exit::requested() {
                self.settings.save(&self.user_dir);
                if exit == Exit::Restart {
                    if MOBILE {
                        // A phone app cannot start itself again: says so before it ends.
                        let text = "The new settings apply when Glue 5 is opened again.";
                        let flag = sdl3::messagebox::MessageBoxFlag::INFORMATION;
                        let _ = sdl3::messagebox::show_simple_message_box(flag, "Glue 5", text, &self.window);
                    }
                    relaunch(true);
                } else if self.mode == Mode::Benchmark {
                    self.benchmark_done();
                }
                return Ok(());
            }
            // A phone's driver can lose the GPU's context and go on answering
            // submissions (its queue reports every job failed, the frames
            // never come): the polls the renderer waits on succeed, so the
            // frames themselves are the only witness left.
            if MOBILE
                && self.settings.exit_on_lost_gpu
                && self.game.is_some()
                && !self.lifecycle.background.load(Ordering::Relaxed)
                && glue5_gpu::perf::since_present().is_some_and(|ago| ago >= std::time::Duration::from_secs(15))
            {
                glue5_runtime::exit::fail("the GPU stopped showing the game's frames (its queue died)");
            }
            if let Some(why) = glue5_runtime::exit::failed() {
                self.stopped(why);
                return Ok(());
            }
            if let Some(game) = &mut self.game
                && game.input.is_none()
            {
                game.input = game.game.input();
                if let Some(i) = &game.input {
                    i.focus(true);
                }
            }
            let page = self.ui.page();
            // Before the game the loop sleeps until there is input or the UI
            // animates; with it, it keeps the touch controls and pads current.
            let wait = if self.game.is_some() { Duration::from_millis(8) } else { self.ui.idle_for() };
            let first = wait_event(&mut self.events, wait);
            let events: Vec<Event> = first.into_iter().chain(self.events.poll_iter()).collect();
            for event in events {
                if self.event(event) {
                    self.settings.save(&self.user_dir);
                    return Ok(());
                }
            }
            if self.lifecycle.came_back.swap(false, Ordering::Relaxed) {
                self.came_back();
            }
            if self.update(page)? {
                self.settings.save(&self.user_dir);
                return Ok(());
            }
        }
    }

    /// One event; true to quit.
    fn event(&mut self, event: Event) -> bool {
        match &event {
            Event::Quit { .. } => return true,
            Event::GamepadAdded { which, .. } => self.pads.added(*which),
            Event::GamepadRemoved { which, .. } => self.pads.removed(*which),
            Event::KeyDown { .. } | Event::MouseButtonDown { .. } | Event::GamepadButtonDown { .. } => {
                self.touch_input = false
            }
            Event::GamepadAxisMotion { value, .. } if value.unsigned_abs() > 16000 => self.touch_input = false,
            Event::Window { win_event: WindowEvent::PixelSizeChanged(..) | WindowEvent::Resized(..), .. } => {
                let (w, h) = self.window.size_in_pixels();
                self.presentation.set_size(w, h);
                self.ui.repaint();
            }
            Event::Window { win_event: WindowEvent::Exposed, .. } => self.ui.repaint(),
            _ => {}
        }
        if let Event::FingerDown { x, y, .. } = event {
            // The settings button takes its finger before the touch controls
            // see it (or they would show, taking pad 0 from a controller).
            if !self.ui.is_open() && self.ui.gear_hit(x, y, &self.window) {
                self.ui.toggle_settings();
                return false;
            }
            self.touch_input = true;
            // A tap above the on-screen keyboard puts it away (it covers the
            // touch controls, and a phone's has no key to hide it).
            if MOBILE && self.typing && !self.ui.is_open() {
                self.keyboard_away = true;
                return false;
            }
        }
        if let Event::KeyDown { scancode: Some(Scancode::F1), repeat: false, .. } = event {
            self.ui.toggle_settings();
            return false;
        }
        if self.ui.is_open() {
            if let Event::KeyDown { scancode: Some(Scancode::Escape), .. } = event {
                self.ui.back();
            } else {
                self.ui.event(&event, &self.window);
            }
            return false;
        }
        match event {
            Event::MouseButtonDown { .. } => self.mouse_used = true,
            Event::FingerDown { finger_id, x, y, .. } => self.touch.finger_down(finger_id, x, y),
            Event::FingerMotion { finger_id, x, y, .. } => self.touch.finger_motion(finger_id, x, y),
            Event::FingerUp { finger_id, .. } => self.touch.finger_up(finger_id),
            _ => {}
        }
        if let Some(game) = &self.game
            && let Some(input) = &game.input
        {
            game_input(input, &event, &self.window, game.back_buffer);
        }
        false
    }

    /// Everything after the events: touch controls, pads, the UI and what
    /// it asks for. True to quit.
    fn update(&mut self, page_before: Option<Page>) -> Result<bool> {
        let in_game = self.game.is_some();
        // Touch controls: a virtual pad in slot 0 unless a controller has it.
        let show = in_game
            && !self.ui.is_open()
            && match self.settings.touch {
                TouchMode::Always => true,
                TouchMode::Off => false,
                TouchMode::Auto => self.touch_input,
            };
        if show != self.touch_shown {
            self.touch_shown = show;
            self.touch.clear();
            glue5_runtime::input::set_gamepad(0, Gamepad::default());
            if let Some(input) = self.game.as_ref().and_then(|g| g.input.as_ref()) {
                if self.touch_mouse.down {
                    input.mouse_button(mouse::LEFT, false);
                }
                if self.touch_mouse.back {
                    input.key(VK_ESCAPE, false);
                }
            }
            self.touch_mouse = Default::default();
        }
        glue5_runtime::input::set_touch_play(show);
        glue5_runtime::input::set_touch_targeting(self.settings.touch_aim.targeting());
        if show {
            self.touch.haptics = self.settings.haptics;
            self.touch.camera_speed = self.settings.touch_camera;
            self.touch.aim_locked = glue5_runtime::input::aim_locked();
            self.touch.layout(ui::screen_size(&self.window), ui::safe_rect(&self.window), self.settings.touch_size);
            self.touch.set_context(glue5_runtime::input::player_context());
            glue5_runtime::input::set_gamepad(0, self.touch.gamepad());
            let look = self.touch.take_look();
            let mouse = self.touch.take_mouse();
            if let Some(game) = &self.game
                && let Some(input) = &game.input
            {
                // The camera, dragged: as a mouse turns it.
                if look != (0, 0) {
                    input.mouse_motion(look.0, look.1);
                }
                // The pause menu: a finger is the mouse.
                if mouse.at != self.touch_mouse.at
                    && let Some(at) = mouse.at
                {
                    let (w, h) = game.back_buffer;
                    input.mouse_position((at.x * w as f32) as i32, (at.y * h as f32) as i32);
                }
                if mouse.down != self.touch_mouse.down {
                    input.mouse_button(mouse::LEFT, mouse.down);
                }
                if mouse.wheel != 0 {
                    input.mouse_wheel(mouse.wheel);
                }
                if mouse.back != self.touch_mouse.back {
                    input.key(VK_ESCAPE, mouse.back);
                }
                self.touch_mouse = mouse;
            }
            if self.settings.haptics {
                haptics::rumble(glue5_runtime::input::rumble(0));
            }
        }

        // While the touch controls are pad 0, a controller in slot 0 waits.
        let report = self.pads.update(self.ui.is_open(), show);
        if report.menu {
            self.ui.toggle_settings();
        }
        for nav in report.nav {
            self.ui.nav(nav);
        }

        // What applies at once (the engine's own frame limiter at its next start).
        self.presentation.set_vsync(self.settings.vsync);
        self.presentation.set_metal_hud(self.settings.overlay == PerfOverlay::MetalHud);
        glue5_gpu::perf::set_on(in_game && self.settings.overlay == PerfOverlay::Glue5);
        self.presentation.set_frame_rate(self.settings.max_fps);
        self.presentation.set_upscaler(upscaler(&self.settings));
        self.presentation.set_sharpness(self.settings.sharpness);
        self.presentation.set_frame_generation(self.settings.frame_generation);
        self.resize_game();

        // The UI: over the game at every turn, before it when something changed.
        let mut actions = Vec::new();
        if in_game || (!self.lifecycle.background.load(Ordering::Relaxed) && self.ui.wants_frame()) {
            let status = Status {
                game_dir: self.game_dir.as_deref(),
                problem: self.problem.as_deref(),
                started: self.game.as_ref().map(|g| &g.started),
                last_failure: self.last_failure.as_deref(),
                graphics_reset: self.graphics_reset,
                window: self.window.size_in_pixels(),
                back_buffer: self.game.as_ref().map(|g| g.back_buffer),
                upscalers: &self.upscalers,
                frame_generation: self.frame_generation,
                metal_hud: self.metal_hud,
                refresh_rate: self.refresh_rate,
                preparing: self.play_requested.then(|| self.screen.as_ref().and_then(Screen::progress)).flatten(),
                mode: self.mode,
                benchmark: self.benchmark.as_ref(),
            };
            // The settings button: on touch screens always, so that it stays
            // with a controller in use or the touch controls off.
            let gear =
                (in_game && (MOBILE || show)).then_some(if show { self.settings.touch_opacity } else { GEAR_ALONE });
            actions = self.ui.frame(&self.window, &mut self.settings, &status, show.then_some(&self.touch), gear);
            if let Some(screen) = &mut self.screen
                && !screen.show_ui()
            {
                // Not shown (the window not visible yet, ...): again soon.
                self.ui.repaint_in(Duration::from_millis(100));
            }
        }
        for action in actions {
            match action {
                Action::Quit => return Ok(true),
                Action::Play(mode) => {
                    self.mode = mode;
                    if mode != Mode::Benchmark {
                        self.settings.sandbox = mode == Mode::Sandbox;
                        self.settings.online = mode == Mode::Online;
                    }
                    self.play()?;
                }
                Action::ChooseGame => self.folder = Some(FolderPick::start(&self.window)),
                Action::FindGame => self.find_game(None),
                Action::ResetGraphics => self.reset_graphics(),
            }
        }
        if self.play_requested {
            self.play()?;
        }
        if let Some(choice) = self.folder.as_ref().and_then(FolderPick::poll) {
            self.folder = None;
            self.chosen(choice);
        }

        let page = self.ui.page();
        if page != page_before {
            if let Some(input) = self.game.as_ref().and_then(|g| g.input.as_ref()) {
                // Nothing stays pressed in the game behind a page.
                input.release_all();
            }
            if page_before == Some(Page::Settings) {
                self.settings.save(&self.user_dir);
            }
        }

        // Text input only while the game wants text: on touch devices it
        // brings up the system keyboard; on desktop it hands every key to
        // the system's input methods, whose accent menu (a held key, macOS),
        // dead keys and IME windows would otherwise get in the way of play.
        // Keys reach the game and the UI by scancode either way.
        let text_box = glue5_runtime::input::text_box_open();
        self.keyboard_away &= text_box;
        let want_typing = (text_box && !self.keyboard_away) || self.ui.wants_text();
        if want_typing != self.typing {
            self.typing = want_typing;
            if want_typing {
                self.text_input.start(&self.window);
            } else {
                self.text_input.stop(&self.window);
            }
        }
        self.pointer();
        Ok(false)
    }

    /// The mouse pointer, as the game has it on PC: captured in play (hidden,
    /// every motion reaching the game); free while the game shows its own
    /// cursor (the pause menu and its map, the phone's browser, ...), the
    /// system's hidden over the window then, as the game draws its own; free
    /// and shown while the window is not in front or a page of ours is
    /// open. It follows these as they change: Esc frees it with the pause
    /// menu and captures it again on leaving it, as does switching to
    /// another window and back; no click is needed.
    fn pointer(&mut self) {
        // Touch alone (phones without a mouse): nothing to do.
        if !self.mouse_used {
            return;
        }
        let Some(input) = self.game.as_ref().and_then(|g| g.input) else { return };
        let ours = self.ui.is_open() || !self.window.has_input_focus();
        let game_cursor = input.cursor_shown();
        let want = !ours && !game_cursor && self.mode != Mode::Benchmark;
        // Capturing waits a moment: back in front over a menu, the game
        // shows its cursor only a frame or two later.
        self.capture_since = want.then(|| self.capture_since.unwrap_or_else(Instant::now));
        let capture = self.capture_since.is_some_and(|t| t.elapsed() >= CAPTURE_DELAY);
        let captured = self.mouse.relative_mouse_mode(&self.window);
        if captured != capture && (capture || !want) {
            self.mouse.set_relative_mouse_mode(&self.window, capture);
            input.pointer_locked(capture);
        }
        let shown = ours || !game_cursor;
        if self.mouse.is_cursor_showing() != shown {
            self.mouse.show_cursor(shown);
        }
    }

    /// The game's back buffer follows the window and the render scale: asked
    /// of the engine when it differs. The engine waits for a size to hold for 20
    /// of its frames before rebuilding its render targets, and a window
    /// being resized by the system does not reach this loop until the end:
    /// one rebuild per resize. Phone windows never change size; only the
    /// scale moves their back buffer.
    fn resize_game(&mut self) {
        let Some(game) = &mut self.game else { return };
        let Some(input) = &game.input else { return };
        let (w, h) = self.window.size_in_pixels();
        if w == 0 || h == 0 {
            return;
        }
        let size = settings::render_size((w, h), self.settings.render_scale(), game.started.low_memory);
        if size != game.back_buffer {
            log::info!("back buffer {}x{}", size.0, size.1);
            input.resize(size.0, size.1);
            game.back_buffer = size;
        }
    }

    /// Starts the game once the pipelines known for it are prepared (the
    /// start page shows how far; called again until then); the renderer
    /// takes the screen over.
    fn play(&mut self) -> Result<()> {
        let Some(game_dir) = self.game_dir.clone() else { return Ok(()) };
        // A toggle made mid-session rides with the next game's device.
        self.settings.apply_env();
        let mut gpu_settings = glue5_gpu::Settings::for_game(&game_dir);
        gpu_settings.cache_dir = Some(self.user_dir.join("cache"));
        gpu_settings.exit_on_lost_gpu = self.settings.exit_on_lost_gpu;
        let Some(screen) = self.screen.as_mut() else { return Ok(()) };
        self.play_requested = !screen.prepare(&gpu_settings);
        if self.play_requested {
            return Ok(());
        }
        let Some(screen) = self.screen.take() else { return Ok(()) };
        // Graphics quality is the game's own (its menu); the low-memory
        // profile only lowers it once, after which the menu has the last word.
        if self.settings.low_memory && !self.settings.low_preset_written {
            match glue5_runtime::game_settings::apply_low_preset(&self.user_dir) {
                Ok(()) => {
                    self.settings.low_preset_written = true;
                    log::info!("the game's graphics settings: low-memory preset written");
                }
                Err(e) => log::warn!("cannot write the low-memory preset: {e}"),
            }
        }
        if MOBILE && !self.settings.low_memory && !self.settings.phone_preset_written {
            match glue5_runtime::game_settings::apply_phone_preset(&self.user_dir) {
                Ok(()) => {
                    self.settings.phone_preset_written = true;
                    log::info!("the game's graphics settings: phone preset written");
                }
                Err(e) => log::warn!("cannot write the phone preset: {e}"),
            }
        }
        if self.mode == Mode::Online && self.settings.online_name.trim().is_empty() {
            self.settings.online_name = settings::default_online_name();
        }
        self.settings.save(&self.user_dir);
        // A new attempt: the last one's failure is history.
        let _ = std::fs::remove_file(self.user_dir.join(FAILURE_FILE));
        log::info!("game: {}, user data: {}", game_dir.display(), self.user_dir.display());
        let s = &self.settings;
        let back_buffer = settings::render_size(self.window.size_in_pixels(), s.render_scale(), s.low_memory);
        let game = glue5_runtime::start(
            glue5_runtime::Config {
                game_dir: game_dir.clone(),
                user_dir: self.user_dir.clone(),
                width: back_buffer.0,
                height: back_buffer.1,
                // The browser page's start choices: 1 the story, 2 the sandbox.
                start_mode: if self.mode == Mode::Sandbox { 2 } else { 1 },
                benchmark: self.mode == Mode::Benchmark,
                low_memory: s.low_memory,
                limit_gpu_memory: s.limit_gpu_memory,
                gpu_budget_mb: s.gpu_budget_mb,
                max_fps: s.max_fps,
                online: (self.mode == Mode::Online)
                    .then(|| glue5_runtime::Online { name: s.online_name.trim().into(), lobby: s.lobby() }),
                language: preferred_language(),
            },
            glue5_gpu::backend(gpu_settings, Some(screen)),
        )?;
        let audio = match self.sdl.audio().map_err(|e| anyhow!("{e}")).and_then(|a| audio::start(&a, game.audio())) {
            Ok(audio) => Some(audio),
            Err(e) => {
                log::warn!("no audio: {e}");
                None
            }
        };
        let started =
            Started { game_dir, max_fps: s.max_fps, low_memory: s.low_memory, render_scale: s.render_scale() };
        // The liveness check's clock: the game's first frame is on its way.
        glue5_gpu::perf::mark_present();
        self.game = Some(Running { game, input: None, started, back_buffer, _audio: audio });
        self.ui.game_started();
        Ok(())
    }

    /// The benchmark is over (the engine asked to quit): on desktop the app
    /// opens again on its start page, which shows the results; a phone app
    /// cannot, so it shows them before it ends.
    fn benchmark_done(&mut self) {
        if !MOBILE {
            relaunch(false);
            return;
        }
        let text = match benchmark::last(&self.user_dir) {
            Some(r) => format!(
                "Benchmark: {:.0} fps on average, {:.0} at least; {:.0}% of the frames in under 33 ms.",
                r.avg_fps,
                r.min_fps,
                r.smooth * 100.0
            ),
            None => "The benchmark is over.".into(),
        };
        let flag = sdl3::messagebox::MessageBoxFlag::INFORMATION;
        let _ = sdl3::messagebox::show_simple_message_box(flag, "Glue 5", &text, &self.window);
    }

    /// The game has stopped working (a thread of the engine's failed): the
    /// window shows its last frame for good. Says so, and records it for the
    /// start page, which offers the way out of a graphics setting the engine
    /// cannot run with; on desktop it opens again on that page.
    fn stopped(&mut self, why: &str) {
        if let Err(e) = std::fs::write(self.user_dir.join(FAILURE_FILE), why) {
            log::warn!("cannot record the failure: {e}");
        }
        self.settings.save(&self.user_dir);
        self.mouse.set_relative_mouse_mode(&self.window, false);
        self.mouse.show_cursor(true);
        let page = if MOBILE { "when the app is opened again, its start page" } else { "the start page" };
        let text = format!(
            "The game has stopped: {why}.\n\nIf it stops again at the same point, one of its graphics settings \
             may be the cause: {page} can put them back to their defaults."
        );
        use sdl3::messagebox::{ButtonData, ClickedButton, MessageBoxButtonFlag as Button, MessageBoxFlag};
        let buttons: &[ButtonData] = if MOBILE {
            &[ButtonData { flags: Button::RETURNKEY_DEFAULT, button_id: 0, text: "OK" }]
        } else {
            &[
                ButtonData { flags: Button::ESCAPEKEY_DEFAULT, button_id: 0, text: "Quit" },
                ButtonData { flags: Button::RETURNKEY_DEFAULT, button_id: 1, text: "Start page" },
            ]
        };
        let shown =
            sdl3::messagebox::show_message_box(MessageBoxFlag::ERROR, buttons, "Glue 5", &text, &self.window, None);
        if let Ok(ClickedButton::CustomButton(ButtonData { button_id: 1, .. })) = shown {
            relaunch(false);
        }
    }

    /// The game's graphics settings back to the engine's defaults (asked for
    /// on the start page).
    fn reset_graphics(&mut self) {
        match glue5_runtime::game_settings::reset_graphics(&self.user_dir) {
            Ok(()) => {
                log::info!("the game's graphics settings: back to the defaults");
                self.graphics_reset = true;
                // The low-memory profile lowers them again at the next start.
                self.settings.low_preset_written = false;
                self.settings.save(&self.user_dir);
            }
            Err(e) => self.ui.note(format!("Cannot reset the game's graphics settings: {e}")),
        }
        self.ui.repaint();
    }

    /// Back in the foreground.
    fn came_back(&mut self) {
        // Android gave the window a new native surface (SDL has updated the
        // window's handle to it already).
        #[cfg(target_os = "android")]
        self.replace_surface();
        self.ui.repaint();
        // The game's files may have been copied in meanwhile (Finder, the
        // Files app, adb).
        if self.game.is_none() && self.game_dir.is_none() {
            self.find_game(None);
        }
    }

    /// A surface for the window's current native surface, for the renderer
    /// to present into from its next frame.
    #[cfg(target_os = "android")]
    fn replace_surface(&mut self) {
        // The window outlives the renderer: the process exits with it.
        let surface = unsafe {
            wgpu::SurfaceTargetUnsafe::from_display_and_window(&self.window, &self.window)
                .map_err(anyhow::Error::from)
                .and_then(|target| Ok(self.instance.create_surface_unsafe(target)?))
        };
        match surface {
            Ok(surface) => self.presentation.replace_surface(surface),
            Err(e) => log::error!("no surface for the window: {e:#}"),
        }
    }

    /// Looks for the game: the folder named on the command line or by
    /// `GLUE5_GAME_DIR`, the last one used, the platform's default places.
    fn find_game(&mut self, explicit: Option<&Path>) {
        self.game_dir = None;
        self.problem = None;
        self.ui.repaint();
        let explicit = explicit.map(PathBuf::from).or_else(|| std::env::var_os("GLUE5_GAME_DIR").map(PathBuf::from));
        if let Some(dir) = explicit {
            if is_game_dir(&dir) {
                self.game_dir = paths::canonical(&dir).ok();
                return;
            }
            self.problem = Some(format!("{} is not a game folder.", dir.display()));
        }
        let candidates = self.settings.game_dir.iter().cloned().chain(paths::default_game_dirs());
        if let Some(dir) = candidates.filter(|d| is_game_dir(d)).find_map(|d| paths::canonical(&d).ok()) {
            self.settings.game_dir = Some(dir.clone());
            self.game_dir = Some(dir);
        }
    }

    /// The folder picker's answer.
    fn chosen(&mut self, choice: Option<PathBuf>) {
        let Some(dir) = choice else { return };
        let Ok(dir) = paths::canonical(&dir) else { return };
        if !is_game_dir(&dir) {
            let problem = format!("{} is not a game folder.", dir.display());
            self.ui.note(problem.clone());
            if self.game.is_none() {
                self.problem = Some(problem);
            }
            return;
        }
        self.settings.game_dir = Some(dir.clone());
        self.settings.save(&self.user_dir);
        if self.game.is_none() {
            self.game_dir = Some(dir);
            self.problem = None;
        }
        self.ui.repaint();
    }
}

/// The next event, waiting up to `timeout` for one.
#[cfg(not(target_os = "ios"))]
fn wait_event(events: &mut sdl3::EventPump, timeout: Duration) -> Option<Event> {
    events.wait_event_timeout(timeout)
}

/// On iOS SDL cannot sleep until an event: its wait runs the run loop for
/// a moment every millisecond (a sixth of a core, all the time). The run
/// loop itself sleeps until a source fires (a touch, a controller, a timer)
/// or the time is up; SDL then has the events it queued.
#[cfg(target_os = "ios")]
fn wait_event(events: &mut sdl3::EventPump, timeout: Duration) -> Option<Event> {
    use std::ffi::c_void;
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
    }
    if let Some(event) = events.poll_event() {
        return Some(event);
    }
    unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, timeout.as_secs_f64(), 1) };
    events.poll_event()
}

/// The refresh rate of the window's display (60 if unknown).
fn window_refresh_rate(window: &sdl3::video::Window) -> f32 {
    let rate = window.get_display().ok().and_then(|d| d.get_mode().ok()).map_or(0.0, |m| m.refresh_rate);
    if rate > 0.0 { rate } else { 60.0 }
}

/// The system's preferred language as a tag like `it-IT`, for the game
/// (phones have no `$LANG`).
fn preferred_language() -> Option<String> {
    use std::ffi::CStr;
    let mut count = 0;
    let locales = unsafe { sdl3::sys::locale::SDL_GetPreferredLocales(&mut count) };
    if locales.is_null() {
        return None;
    }
    let first = unsafe { (count > 0).then(|| &**locales) };
    let tag = first.filter(|l| !l.language.is_null()).map(|l| {
        let language = unsafe { CStr::from_ptr(l.language) }.to_string_lossy();
        match l.country.is_null() {
            true => language.into_owned(),
            false => format!("{language}-{}", unsafe { CStr::from_ptr(l.country) }.to_string_lossy()),
        }
    });
    unsafe { sdl3::sys::stdinc::SDL_free(locales.cast()) };
    tag
}

fn upscaler(settings: &Settings) -> Upscaler {
    settings.upscaler.upscaler()
}

/// The upscalers, as the settings offer them.
const UPSCALERS: &[Upscaling] = &[Upscaling::Bicubic, Upscaling::MetalFx, Upscaling::MetalFxTemporal, Upscaling::Fsr];

/// Keyboard and mouse into the game's input block.
fn game_input(
    input: &InputBlock,
    event: &Event,
    window: &sdl3::video::Window,
    back_buffer: (u32, u32),
) {
    let (bw, bh) = (back_buffer.0 as f32, back_buffer.1 as f32);
    match *event {
        Event::KeyDown { scancode: Some(sc), repeat, .. } => {
            if !repeat {
                key(input, sc, true);
            }
            match sc {
                Scancode::Return | Scancode::KpEnter => input.text("\r"),
                Scancode::Backspace => input.text("\u{8}"),
                _ => {}
            }
        }
        Event::KeyUp { scancode: Some(sc), .. } => key(input, sc, false),
        Event::TextInput { ref text, .. } => input.text(text),
        Event::MouseMotion { x, y, xrel, yrel, .. } => {
            let (ww, wh) = window.size();
            input.mouse_position((x * bw / ww as f32) as i32, (y * bh / wh as f32) as i32);
            input.mouse_motion(xrel.round() as i32, yrel.round() as i32);
        }
        Event::MouseButtonDown { mouse_btn, .. } => input.mouse_button(mouse_bit(mouse_btn), true),
        Event::MouseButtonUp { mouse_btn, .. } => input.mouse_button(mouse_bit(mouse_btn), false),
        Event::MouseWheel { y, .. } => input.mouse_wheel(y.round() as i32),
        Event::Window { win_event: WindowEvent::FocusGained, .. } => input.focus(true),
        Event::Window { win_event: WindowEvent::FocusLost, .. } => input.focus(false),
        _ => {}
    }
}

/// Starts the application again, straight into the game with `play`, else on
/// its start page (desktop; a phone app cannot, and the player reopens it).
fn relaunch(play: bool) {
    if MOBILE {
        return;
    }
    let args: Vec<_> = std::env::args_os().skip(1).filter(|a| a != "--play").collect();
    let started = std::env::current_exe().and_then(|exe| {
        let mut command = std::process::Command::new(exe);
        if play {
            command.arg("--play");
        }
        command.args(args).spawn()
    });
    if let Err(e) = started {
        log::warn!("cannot start again: {e}");
    }
}

fn key(input: &InputBlock, sc: sdl3::keyboard::Scancode, down: bool) {
    if let Some(vk) = keymap::vk(sc) {
        input.key(vk, down);
        if let Some(generic) = keymap::generic(vk) {
            input.key(generic, down);
        }
    }
    if let Some(alias) = keymap::numpad_alias(sc) {
        input.key(alias, down);
    }
}

fn mouse_bit(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => mouse::LEFT,
        MouseButton::Right => mouse::RIGHT,
        MouseButton::Middle => mouse::MIDDLE,
        MouseButton::X1 => mouse::X1,
        MouseButton::X2 => mouse::X2,
        _ => 0,
    }
}

/// The system's folder picker, answered asynchronously (SDL calls back on
/// the main thread while events are pumped).
struct FolderPick(Arc<Mutex<Option<Option<PathBuf>>>>);

impl FolderPick {
    fn start(window: &sdl3::video::Window) -> FolderPick {
        let picked: Arc<Mutex<Option<Option<PathBuf>>>> = Default::default();
        let slot = picked.clone();
        sdl3::dialog::show_open_folder_dialog(
            None::<&str>,
            false,
            window,
            Box::new(move |result, _| *slot.lock().unwrap() = Some(result.ok().and_then(|mut v| v.pop()))),
        );
        FolderPick(picked)
    }

    /// `Some(choice)` once answered (`Some(None)`: cancelled).
    fn poll(&self) -> Option<Option<PathBuf>> {
        self.0.lock().unwrap().take()
    }
}

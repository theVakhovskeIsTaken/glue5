//! The application's own UI, drawn with egui over the game (before the game
//! runs, alone): the start page, the settings, the credits, and on touch
//! screens the settings button and the touch controls. The game's menus
//! stay the game's.
//!
//! One responsive layout, fitted to the safe area. The same widgets
//! everywhere, all reachable from a controller or a keyboard: the D-pad,
//! left stick or arrows move the focus, A / Space / Enter activates, B /
//! Esc goes back.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, FontId, Id, LayerId, Order, Pos2, Rect, SafeAreaInsets, Stroke, Vec2, ViewportId, pos2, vec2,
};
use sdl3::event::Event;
use sdl3::keyboard::Scancode;
use sdl3::mouse::MouseButton;
use sdl3::video::Window;

use crate::settings::{PerfOverlay, Settings};

mod credits;
mod pages;
mod perf;
mod theme;
mod widgets;
use credits::credits_page;
use pages::{settings_page, start_page};

use crate::touch::Touch;

/// A page of the application's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// Before the game: its files, the ways into it, Settings, Credits.
    Start,
    Settings,
    /// Who made what the application is built on, and their licenses.
    Credits,
}

/// How the game starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Story,
    /// The whole map, free roam, no missions.
    Sandbox,
    /// GTA Online's freemode, with the lobby's other players.
    Online,
    /// The game's fly-through of the map, its results on the start page.
    Benchmark,
}

/// What the pages ask the application to do.
pub enum Action {
    Play(Mode),
    Quit,
    /// The system's folder picker, for the game (desktop).
    ChooseGame,
    /// Look for the game's files again (mobile: copied in meanwhile).
    FindGame,
    /// The game's graphics settings back to the engine's defaults.
    ResetGraphics,
}

/// Navigation from a controller.
#[derive(Clone, Copy, Debug)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    Activate,
    Back,
}

/// What the running game started with: settings that differ apply when it
/// starts again.
pub struct Started {
    pub game_dir: PathBuf,
    pub max_fps: u32,
    pub low_memory: bool,
    pub render_scale: f32,
}

/// What the pages show about the application.
pub struct Status<'a> {
    /// The game's folder, if found.
    pub game_dir: Option<&'a Path>,
    /// Why it was not (or why it would not start).
    pub problem: Option<&'a str>,
    /// The game runs (and started with these).
    pub started: Option<&'a Started>,
    /// Why the last game stopped, if it did.
    pub last_failure: Option<&'a str>,
    /// The game's graphics settings went back to their defaults.
    pub graphics_reset: bool,
    /// The window, in pixels.
    pub window: (u32, u32),
    /// The game's back buffer, while it runs.
    pub back_buffer: Option<(u32, u32)>,
    /// The upscalers this GPU has.
    pub upscalers: &'a [crate::settings::Upscaling],
    /// This GPU can generate frames (with MetalFX Temporal).
    pub frame_generation: bool,
    /// Apple's Metal HUD can show (it was on at launch).
    #[cfg_attr(not(target_vendor = "apple"), allow(dead_code))]
    pub metal_hud: bool,
    /// The display's refresh rate at most, in Hz.
    pub refresh_rate: f32,
    /// Play waits for the game's pipelines: how far they are (0 to 1).
    pub preparing: Option<f32>,
    /// The mode Play starts (or the last one played).
    pub mode: Mode,
    /// The last benchmark's results.
    pub benchmark: Option<&'a crate::benchmark::Results>,
}

/// The settings button over the game: from the safe area's top right
/// corner, in points, sized like the touch controls (next to their Start
/// and Back buttons).
const GEAR_AT: Vec2 = vec2(-156.0, 40.0);
const GEAR_RADIUS: f32 = 20.0;

const WARN: Color32 = Color32::from_rgb(240, 180, 100);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    Video,
    Controls,
    Game,
    Experimental,
}

pub struct Ui {
    ctx: egui::Context,
    background: Option<egui::TextureHandle>,
    tab: SettingsTab,
    overlay: glue5_gpu::Overlay,
    events: Vec<egui::Event>,
    pointer: Pos2,
    start: Instant,
    page: Option<Page>,
    /// Where the settings go back to: the start page before the game.
    home: Option<Page>,
    /// Where the credits go back to: the page they were opened from.
    behind: Option<Page>,
    /// The last submission drew something (an empty one clears it).
    drawn: bool,
    note: Option<String>,
    /// The render scale while its slider is dragged (it applies when let go).
    scale_edit: Option<f32>,
    /// A mode was chosen: this frame shows that it starts, then it does.
    starting: Option<Mode>,
    /// GTA Online's card on the start page is open (its lobby and name).
    online_open: bool,
    /// The settings button as last drawn (centre, radius), in points.
    gear: Option<(Pos2, f32)>,
    /// When egui wants to draw again (before the game, frames are drawn
    /// only for input or for this).
    repaint_at: Option<Instant>,
    perf: perf::PerfHud,
}

impl Ui {
    pub fn new(overlay: glue5_gpu::Overlay) -> Self {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let background = theme::background(&ctx);
        Ui {
            ctx,
            background,
            tab: SettingsTab::default(),
            overlay,
            events: Vec::new(),
            pointer: Pos2::ZERO,
            start: Instant::now(),
            page: Some(Page::Start),
            home: Some(Page::Start),
            behind: Some(Page::Start),
            drawn: false,
            note: None,
            scale_edit: None,
            starting: None,
            online_open: false,
            gear: None,
            repaint_at: Some(Instant::now()),
            perf: Default::default(),
        }
    }

    pub fn page(&self) -> Option<Page> {
        self.page
    }

    pub fn is_open(&self) -> bool {
        self.page.is_some()
    }

    /// The game runs: no start page any more.
    pub fn game_started(&mut self) {
        self.home = None;
        self.starting = None;
        self.show(None);
    }

    /// The settings (from the game or the start page); again, back.
    pub fn toggle_settings(&mut self) {
        if self.page == Some(Page::Settings) {
            self.back();
        } else {
            self.show(Some(Page::Settings));
        }
    }

    /// From the settings back to the start page, or the game; from the
    /// credits back to where they were opened; on the start page, GTA
    /// Online's card closes.
    pub fn back(&mut self) {
        match self.page {
            Some(Page::Start) if self.online_open => {
                self.online_open = false;
                self.repaint();
            }
            Some(Page::Settings) => self.show(self.home),
            Some(Page::Credits) => self.show(self.behind),
            _ => {}
        }
    }

    fn show(&mut self, page: Option<Page>) {
        if page == Some(Page::Credits) && self.page != page {
            self.behind = self.page;
        }
        self.page = page;
        self.note = None;
        self.scale_edit = None;
        self.ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        self.repaint();
    }

    /// A message shown in the settings (e.g. "applies at the next start").
    pub fn note(&mut self, text: impl Into<String>) {
        self.note = Some(text.into());
        self.repaint();
    }

    /// Draw again at the next frame (the window changed, ...).
    pub fn repaint(&mut self) {
        self.repaint_in(Duration::ZERO);
    }

    pub fn repaint_in(&mut self, delay: Duration) {
        let at = Instant::now() + delay;
        self.repaint_at = Some(self.repaint_at.map_or(at, |t| t.min(at)));
    }

    /// Before the game: whether a frame should be drawn now, and how long
    /// the application can wait for input otherwise.
    pub fn wants_frame(&self) -> bool {
        !self.events.is_empty() || self.repaint_at.is_some_and(|t| Instant::now() >= t)
    }

    /// A text field of the app's has the focus: text input on (the system
    /// keyboard on touch screens).
    pub fn wants_text(&self) -> bool {
        self.page.is_some() && self.ctx.egui_wants_keyboard_input()
    }

    pub fn idle_for(&self) -> Duration {
        let most = Duration::from_millis(250);
        self.repaint_at.map_or(most, |t| t.saturating_duration_since(Instant::now()).min(most))
    }

    /// Whether a finger at `x`, `y` (0..1, as SDL gives them) is on the
    /// settings button.
    pub fn gear_hit(&self, x: f32, y: f32, window: &Window) -> bool {
        let size = screen_size(window);
        self.gear.is_some_and(|(c, r)| c.distance(pos2(x * size.x, y * size.y)) <= r * 1.4)
    }

    /// Input for a page while one is open.
    pub fn event(&mut self, event: &Event, window: &Window) {
        let size = screen_size(window);
        let scale = points_per_window_unit(window);
        let button = |b: &MouseButton| match b {
            MouseButton::Right => Some(egui::PointerButton::Secondary),
            MouseButton::Middle => Some(egui::PointerButton::Middle),
            MouseButton::Left => Some(egui::PointerButton::Primary),
            _ => None,
        };
        let modifiers = egui::Modifiers::default();
        match event {
            Event::MouseMotion { x, y, .. } => self.pointer_to(pos2(*x, *y) * scale),
            Event::MouseButtonDown { mouse_btn, x, y, .. } | Event::MouseButtonUp { mouse_btn, x, y, .. } => {
                let pressed = matches!(event, Event::MouseButtonDown { .. });
                if let Some(button) = button(mouse_btn) {
                    self.pointer_to(pos2(*x, *y) * scale);
                    self.events.push(egui::Event::PointerButton { pos: self.pointer, button, pressed, modifiers });
                }
            }
            Event::MouseWheel { x, y, .. } => self.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: vec2(*x, *y),
                modifiers,
                phase: egui::TouchPhase::Move,
            }),
            // A finger is a touch (egui scrolls by dragging only on a touch
            // screen) and the pointer.
            Event::FingerDown { touch_id, finger_id, x, y, pressure, .. }
            | Event::FingerUp { touch_id, finger_id, x, y, pressure, .. } => {
                let pressed = matches!(event, Event::FingerDown { .. });
                let phase = if pressed { egui::TouchPhase::Start } else { egui::TouchPhase::End };
                self.touch(*touch_id, *finger_id, phase, pos2(x * size.x, y * size.y), *pressure);
                self.pointer_to(pos2(x * size.x, y * size.y));
                self.events.push(egui::Event::PointerButton {
                    pos: self.pointer,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers,
                });
                if !pressed {
                    self.events.push(egui::Event::PointerGone);
                }
            }
            Event::FingerMotion { touch_id, finger_id, x, y, pressure, .. } => {
                let p = pos2(x * size.x, y * size.y);
                self.touch(*touch_id, *finger_id, egui::TouchPhase::Move, p, *pressure);
                self.pointer_to(p);
            }
            Event::TextInput { text, .. } => self.events.push(egui::Event::Text(text.clone())),
            // Pasting and copying into the app's text fields.
            Event::KeyDown { scancode: Some(sc @ (Scancode::V | Scancode::C | Scancode::X)), keymod, .. }
                if keymod.intersects(sdl3::keyboard::Mod::LCTRLMOD | sdl3::keyboard::Mod::RCTRLMOD)
                    || keymod.intersects(sdl3::keyboard::Mod::LGUIMOD | sdl3::keyboard::Mod::RGUIMOD) =>
            {
                self.events.push(match sc {
                    Scancode::V => match window.subsystem().clipboard().clipboard_text() {
                        Ok(text) => egui::Event::Paste(text),
                        Err(_) => return,
                    },
                    Scancode::C => egui::Event::Copy,
                    _ => egui::Event::Cut,
                });
            }
            Event::KeyDown { scancode: Some(sc), keymod, .. } | Event::KeyUp { scancode: Some(sc), keymod, .. } => {
                let pressed = matches!(event, Event::KeyDown { .. });
                if let Some(key) = key(*sc) {
                    let shift = keymod.intersects(sdl3::keyboard::Mod::LSHIFTMOD | sdl3::keyboard::Mod::RSHIFTMOD);
                    self.key(key, pressed, egui::Modifiers { shift, ..Default::default() });
                }
            }
            _ => {}
        }
    }

    fn touch(&mut self, device: u64, finger: u64, phase: egui::TouchPhase, pos: Pos2, force: f32) {
        self.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(device),
            id: egui::TouchId(finger),
            phase,
            pos,
            force: Some(force),
        });
    }

    fn pointer_to(&mut self, p: Pos2) {
        self.pointer = p;
        self.events.push(egui::Event::PointerMoved(p));
    }

    fn key(&mut self, key: egui::Key, pressed: bool, modifiers: egui::Modifiers) {
        self.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
    }

    pub fn nav(&mut self, nav: Nav) {
        let key = match nav {
            Nav::Up => egui::Key::ArrowUp,
            Nav::Down => egui::Key::ArrowDown,
            Nav::Left => egui::Key::ArrowLeft,
            Nav::Right => egui::Key::ArrowRight,
            Nav::Activate => egui::Key::Enter,
            Nav::Back => return self.back(),
        };
        // Nothing has the focus yet: the first move gives it to the page's
        // first control (egui moves it from a focused widget only).
        let key = if self.ctx.memory(|m| m.focused()).is_none() { egui::Key::Tab } else { key };
        self.key(key, true, Default::default());
        self.key(key, false, Default::default());
    }

    /// Draws the open page, or over the game the touch controls (if given),
    /// the settings button (if given, at this opacity) and the performance
    /// overlay (if on).
    pub fn frame(
        &mut self,
        window: &Window,
        settings: &mut Settings,
        status: &Status,
        touch: Option<&Touch>,
        gear: Option<f32>,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let gear = gear.filter(|_| self.page.is_none());
        let hud = status.started.is_some() && self.page.is_none() && settings.overlay == PerfOverlay::Glue5;
        if !hud {
            self.perf.hide();
        }
        if self.page.is_none() && touch.is_none() && gear.is_none() && (!hud || !self.perf.due(settings.overlay_detail))
        {
            if self.drawn && !hud {
                self.overlay.submit(Vec::new(), Default::default(), 1.0);
                self.drawn = false;
            }
            self.events.clear();
            self.gear = None;
            return actions;
        }
        // Play was chosen at the last frame: this one shows that the game
        // starts, then it does.
        let starting = std::mem::take(&mut self.starting);
        let ppp = pixels_per_point(window);
        let screen = Rect::from_min_size(Pos2::ZERO, screen_size(window));
        let safe = safe_rect(window);
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            safe_area_insets: Some(SafeAreaInsets(egui::epaint::MarginF32 {
                left: safe.min.x - screen.min.x,
                right: screen.max.x - safe.max.x,
                top: safe.min.y - screen.min.y,
                bottom: screen.max.y - safe.max.y,
            })),
            time: Some(self.start.elapsed().as_secs_f64()),
            events: std::mem::take(&mut self.events),
            focused: true,
            ..Default::default()
        };
        raw.viewports.entry(ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);

        let ctx = self.ctx.clone();
        // The page to show next, or back to the previous one.
        let mut go = None;
        let mut back = false;
        ctx.begin_pass(raw);
        if let Some(touch) = touch {
            touch.draw(&ctx.layer_painter(LayerId::background()), settings.touch_opacity);
        }
        self.gear = gear.map(|alpha| draw_gear(&ctx, safe, settings.touch_size, alpha));
        if hud {
            self.perf.draw(&ctx, safe, settings, status);
        }
        if self.page.is_some() && status.started.is_none() {
            theme::backdrop(&ctx, self.background.as_ref());
        }
        match self.page {
            Some(Page::Start) => {
                let open = &mut self.online_open;
                self.starting = start_page(&ctx, settings, status, starting, open, &mut actions, &mut go);
            }
            Some(Page::Settings) => {
                let note = self.note.as_deref();
                let (tab, scale_edit) = (&mut self.tab, &mut self.scale_edit);
                back = settings_page(&ctx, settings, status, note, tab, scale_edit, &mut actions, &mut go);
            }
            Some(Page::Credits) => back = credits_page(&ctx),
            None => {}
        }
        let mut output = ctx.end_pass();
        for command in std::mem::take(&mut output.platform_output.commands) {
            match command {
                egui::OutputCommand::OpenUrl(open) => {
                    if let Err(e) = sdl3::url::open_url(&open.url) {
                        log::warn!("cannot open {}: {e}", open.url);
                    }
                }
                egui::OutputCommand::CopyText(text) => {
                    let _ = window.subsystem().clipboard().set_clipboard_text(&text);
                }
                _ => {}
            }
        }
        let delay = output.viewport_output.get(&ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        self.repaint_at = (delay < Duration::from_secs(10)).then(|| Instant::now() + delay);
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        self.overlay.submit(primitives, output.textures_delta, output.pixels_per_point);
        self.drawn = true;
        if back {
            self.back();
        } else if let Some(page) = go {
            self.show(page);
        }
        if self.starting.is_some() {
            self.repaint();
        } else if status.preparing.is_some() {
            self.repaint_in(Duration::from_millis(200));
        }
        if let Some(mode) = starting {
            actions.push(Action::Play(mode));
        }
        actions
    }
}

// Points: the UI's unit, the same physical size everywhere. As SDL's
// README-highdpi describes, a point is the window's display scale in pixels;
// SDL's window coordinates are points on Apple platforms but pixels on
// Android and Windows (where the display's content scale enlarges the UI).

/// Pixels per point.
fn pixels_per_point(window: &Window) -> f32 {
    window.display_scale().max(0.5)
}

/// Points per unit of SDL's window coordinates (event and safe area
/// positions).
fn points_per_window_unit(window: &Window) -> f32 {
    window.pixel_density() / pixels_per_point(window)
}

/// The window's size in points.
pub fn screen_size(window: &Window) -> Vec2 {
    let (w, h) = window.size_in_pixels();
    vec2(w as f32, h as f32) / pixels_per_point(window)
}

/// The part of the window clear of notches and rounded corners, in points.
pub fn safe_rect(window: &Window) -> Rect {
    match window.safe_area() {
        Ok(r) if r.width() > 0 && r.height() > 0 => {
            let scale = points_per_window_unit(window);
            Rect::from_min_size(
                pos2(r.x() as f32, r.y() as f32) * scale,
                vec2(r.width() as f32, r.height() as f32) * scale,
            )
        }
        _ => Rect::from_min_size(Pos2::ZERO, screen_size(window)),
    }
}

/// The settings button: the touch controls' look, at their size.
fn draw_gear(ctx: &egui::Context, safe: Rect, scale: f32, alpha: f32) -> (Pos2, f32) {
    let c = pos2(safe.max.x + GEAR_AT.x * scale, safe.min.y + GEAR_AT.y * scale);
    let r = GEAR_RADIUS * scale;
    let white = |a: f32| Color32::from_white_alpha((a * alpha * 255.0) as u8);
    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("gear")));
    painter.circle(c, r, white(0.18), Stroke::new(1.5 * scale, white(0.6)));
    painter.text(c, Align2::CENTER_CENTER, "⚙", FontId::proportional(r * 0.95), white(0.9));
    (c, r)
}

fn key(sc: Scancode) -> Option<egui::Key> {
    Some(match sc {
        Scancode::Tab => egui::Key::Tab,
        Scancode::Return | Scancode::KpEnter => egui::Key::Enter,
        Scancode::Space => egui::Key::Space,
        Scancode::Backspace => egui::Key::Backspace,
        Scancode::Delete => egui::Key::Delete,
        Scancode::Home => egui::Key::Home,
        Scancode::End => egui::Key::End,
        Scancode::Up => egui::Key::ArrowUp,
        Scancode::Down => egui::Key::ArrowDown,
        Scancode::Left => egui::Key::ArrowLeft,
        Scancode::Right => egui::Key::ArrowRight,
        _ => return None,
    })
}

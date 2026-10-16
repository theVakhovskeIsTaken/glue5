//! Touch controls.
//!
//! - On foot and driving, controls made for touch, a layout for each: a
//!   stick where the left thumb lands (on foot, pushed past its ring, the
//!   player sprints), the camera turned by dragging anywhere else on the
//!   right, as a mouse turns it, and buttons named for what they do, the
//!   Enter button only next to a vehicle. Aim and Fire turn the camera too
//!   while held; the wheels (weapons, characters, radio) open while their
//!   button is held and pick by dragging from it, or on the right.
//!   With the phone out, its keys join the layout. Passengers and cutscenes
//!   have theirs too.
//! - In the pause menu, a finger is the mouse.
//!
//! They make a [`Gamepad`] (and the camera a mouse's motion): the game plays
//! as with a controller (its prompts, aim assist).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2, pos2, vec2};
use glue5_runtime::input::{Activity, Driving, Gamepad, PlayerContext, button};

use crate::haptics;

/// Stick travel, in points (scaled with the controls).
const STICK_RADIUS: f32 = 56.0;
/// Phones are held closer: their controls are this much smaller.
const PHONE_SCALE: f32 = 0.82;
/// Below this many points on its short side, a screen is a phone's.
const PHONE_SIDE: f32 = 500.0;

/// A position in the safe area, in points: from its left or right edge, or
/// from its middle (x), and from its top or bottom edge (y).
#[derive(Clone, Copy, Debug)]
enum At {
    LeftTop(f32, f32),
    LeftBottom(f32, f32),
    RightTop(f32, f32),
    RightBottom(f32, f32),
    MidTop(f32, f32),
}

/// Where the stick rests (drawn there until a thumb takes it).
const MOVE_REST: At = At::LeftBottom(150.0, 96.0);
/// A button of the touch layouts, named for what it does: it presses the pad
/// buttons the game maps that to (common/data/control/standard*.meta).
#[derive(Clone, Copy, Debug)]
struct Action {
    /// What it does (in English; the tests find buttons by it).
    #[cfg_attr(not(test), allow(dead_code))]
    label: &'static str,
    /// Its icon's name, or two names joined by `+` (side by side).
    icon: &'static str,
    bits: u16,
    at: At,
    radius: f32,
    kind: Kind,
    /// Shown only next to a vehicle the player can get into.
    near_vehicle: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Press,
    /// Held, the finger's drag also turns the camera.
    Turns,
    /// Held, it opens one of the game's wheels (weapons, characters, radio
    /// stations): the finger's drag picks in it, as the right stick does.
    Wheel,
}

const fn action(label: &'static str, icon: &'static str, bits: u16, at: At, radius: f32) -> Action {
    Action { label, icon, bits, at, radius, kind: Kind::Press, near_vehicle: false }
}

/// The buttons' icons: game-icons.net's (CC BY 3.0, authors in
/// assets/touch/README.md), white on transparent, square.
macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../assets/touch/", $name, ".png")))),*]
    };
}
const ICON_FILES: &[(&str, &[u8])] = icons![
    "action", "aim", "arrow-down", "arrow-left", "arrow-right", "arrow-up", "behind", "bell", "brake", "check",
    "cover", "crouch", "enter", "exit", "fire", "gas", "gear", "handbrake", "horn", "jump", "lights", "melee",
    "parachute", "pause", "pedal", "phone", "radio", "reload", "seat", "skip", "special", "switch", "throw",
    "turn-left", "turn-right", "view", "weapons",
];

/// The icons' textures, by name.
type Icons = HashMap<&'static str, egui::TextureHandle>;

fn load_icons(ctx: &egui::Context) -> Icons {
    let decode = |bytes: &[u8]| -> Result<egui::ColorImage, png::DecodingError> {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
        let mut pixels = vec![0; reader.output_buffer_size().unwrap_or(0)];
        let info = reader.next_frame(&mut pixels)?;
        let size = [info.width as usize, info.height as usize];
        Ok(egui::ColorImage::from_rgba_unmultiplied(size, &pixels[..info.buffer_size()]))
    };
    let mut icons = Icons::new();
    for (name, bytes) in ICON_FILES {
        match decode(bytes) {
            Ok(image) => {
                icons.insert(*name, ctx.load_texture(format!("touch-{name}"), image, egui::TextureOptions::LINEAR));
            }
            Err(e) => log::warn!("touch icon {name}: {e}"),
        }
    }
    icons
}

const fn turns(a: Action) -> Action {
    Action { kind: Kind::Turns, ..a }
}

const fn wheel(a: Action) -> Action {
    Action { kind: Kind::Wheel, ..a }
}

const fn near_vehicle(a: Action) -> Action {
    Action { near_vehicle: true, ..a }
}

// In every layout: pause and the camera view at the top's middle, the phone
// in the top right corner.
const PAUSE: Action = action("☰", "pause", button::START, At::MidTop(30.0, 30.0), 20.0);
const VIEW: Action = action("View", "view", button::SELECT, At::MidTop(-30.0, 30.0), 20.0);
const PHONE: Action = action("Phone", "phone", button::UP, At::RightTop(56.0, 46.0), 22.0);

/// The main buttons of the layouts in vehicles sit here: the right thumb's
/// throttle and brake, then around them.
const MAIN: At = At::RightBottom(78.0, 150.0);
const SECOND: At = At::RightBottom(184.0, 80.0);
const THIRD: At = At::RightBottom(190.0, 196.0);
const FOURTH: At = At::RightBottom(70.0, 262.0);
const FIFTH: At = At::RightBottom(280.0, 50.0);
const SIXTH: At = At::RightBottom(282.0, 128.0);

/// On foot (the stick sprints past its ring).
static ON_FOOT: &[Action] = &[
    turns(action("Fire", "fire", button::R2, At::RightBottom(78.0, 166.0), 34.0)),
    turns(action("Aim", "aim", button::L2, At::RightBottom(176.0, 104.0), 30.0)),
    action("Jump", "jump", button::WEST, At::RightBottom(64.0, 64.0), 28.0),
    near_vehicle(action("Enter", "enter", button::NORTH, At::RightBottom(180.0, 214.0), 28.0)),
    action("Melee\nReload", "melee+reload", button::EAST, At::RightBottom(268.0, 52.0), 24.0),
    action("Cover", "cover", button::R1, At::RightBottom(78.0, 262.0), 24.0),
    action("Crouch", "crouch", button::L3, At::LeftBottom(46.0, 200.0), 22.0),
    action("Special", "special", button::L3 | button::R3, At::LeftBottom(46.0, 256.0), 22.0),
    wheel(action("Weapons", "weapons", button::L1, At::LeftTop(56.0, 46.0), 22.0)),
    wheel(action("Switch", "switch", button::DOWN, At::LeftTop(112.0, 46.0), 22.0)),
    action("Throw", "throw", button::LEFT, At::LeftTop(168.0, 46.0), 22.0),
    action("Action", "action", button::RIGHT, At::RightTop(56.0, 104.0), 24.0),
    PHONE,
    VIEW,
    PAUSE,
];

/// Cars, motorbikes, quads, boats.
static CAR: &[Action] = &[
    action("Gas", "gas", button::R2, MAIN, 36.0),
    action("Brake", "brake", button::L2, SECOND, 32.0),
    action("Exit", "enter", button::NORTH, THIRD, 26.0),
    action("H.brake", "handbrake", button::R1, FOURTH, 26.0),
    action("Horn", "horn", button::L3, FIFTH, 22.0),
    // Drive-by: LB aims, RB fires.
    action("Shoot", "fire", button::L1 | button::R1, SIXTH, 22.0),
    wheel(action("Radio", "radio", button::LEFT, At::LeftTop(56.0, 46.0), 22.0)),
    action("Lights", "lights", button::RIGHT, At::LeftTop(112.0, 46.0), 22.0),
    action("Behind", "behind", button::R3, At::LeftTop(168.0, 46.0), 22.0),
    PHONE,
    VIEW,
    PAUSE,
];

static BICYCLE: &[Action] = &[
    action("Pedal", "pedal", button::SOUTH, MAIN, 36.0),
    action("Brake", "brake", button::L2, SECOND, 32.0),
    action("Exit", "enter", button::NORTH, THIRD, 26.0),
    action("Bell", "bell", button::L3, FIFTH, 22.0),
    action("Behind", "behind", button::R3, At::LeftTop(56.0, 46.0), 22.0),
    PHONE,
    VIEW,
    PAUSE,
];

/// Helicopters (the stick pitches and rolls).
static HELI: &[Action] = &[
    action("Up", "arrow-up", button::R2, MAIN, 34.0),
    action("Down", "arrow-down", button::L2, SECOND, 30.0),
    action("Exit", "enter", button::NORTH, FIFTH, 24.0),
    action("◀ Yaw", "turn-left", button::L1, At::RightBottom(160.0, 250.0), 24.0),
    action("Yaw ▶", "turn-right", button::R1, At::RightBottom(78.0, 262.0), 24.0),
    action("Fire", "fire", button::SOUTH, SIXTH, 24.0),
    PHONE,
    VIEW,
    PAUSE,
];

static PLANE: &[Action] = &[
    action("Throttle", "gas", button::R2, MAIN, 34.0),
    action("Brake", "brake", button::L2, SECOND, 30.0),
    action("Exit", "enter", button::NORTH, FIFTH, 24.0),
    action("◀ Yaw", "turn-left", button::L1, At::RightBottom(160.0, 250.0), 24.0),
    action("Yaw ▶", "turn-right", button::R1, At::RightBottom(78.0, 262.0), 24.0),
    action("Fire", "fire", button::SOUTH, SIXTH, 24.0),
    action("Gear", "gear", button::L3, At::LeftTop(56.0, 46.0), 22.0),
    PHONE,
    VIEW,
    PAUSE,
];

static SUBMARINE: &[Action] = &[
    action("Ahead", "gas", button::R2, MAIN, 34.0),
    action("Back", "brake", button::L2, SECOND, 30.0),
    action("Rise", "arrow-up", button::WEST, SIXTH, 24.0),
    action("Dive", "arrow-down", button::SOUTH, FIFTH, 24.0),
    action("Exit", "enter", button::NORTH, At::RightBottom(160.0, 250.0), 24.0),
    PHONE,
    VIEW,
    PAUSE,
];

/// A passenger: shooting from the window (LT aims, RT fires).
static PASSENGER: &[Action] = &[
    turns(action("Shoot", "fire", button::R2, MAIN, 34.0)),
    turns(action("Aim", "aim", button::L2, SECOND, 30.0)),
    action("Exit", "enter", button::NORTH, THIRD, 26.0),
    // The driver's seat, when free.
    action("Seat", "seat", button::RIGHT, FIFTH, 22.0),
    wheel(action("Radio", "radio", button::LEFT, At::LeftTop(56.0, 46.0), 22.0)),
    action("Behind", "behind", button::R3, At::LeftTop(112.0, 46.0), 22.0),
    PHONE,
    VIEW,
    PAUSE,
];

/// Skydiving, then under the parachute (the stick steers).
static PARACHUTE: &[Action] = &[
    action("Open", "parachute", button::SOUTH, MAIN, 34.0),
    action("Detach", "exit", button::NORTH, THIRD, 26.0),
    action("◀ Brake", "arrow-left", button::L1, At::RightBottom(160.0, 250.0), 24.0),
    action("Brake ▶", "arrow-right", button::R1, At::RightBottom(78.0, 262.0), 24.0),
    PHONE,
    VIEW,
    PAUSE,
];

/// A cutscene: its skip.
static CUTSCENE: &[Action] = &[action("Skip", "skip", button::SOUTH, MAIN, 30.0), PAUSE];

/// In the pause menu, a finger is the mouse (tap: a click, drag: the map
/// moves); two fingers turn the wheel (pinched: the map zooms, dragged
/// together: lists scroll); this button goes back (Esc).
const MENU_BACK: Action = action("Back", "turn-left", 0, At::LeftTop(56.0, 46.0), 24.0);
/// A pinch this many points wider or narrower turns the wheel a notch.
const PINCH_NOTCH: f32 = 40.0;
/// Two fingers dragged together this many points turn the wheel a notch (a
/// list's line, about).
const SCROLL_NOTCH: f32 = 28.0;
/// Two fingers pinch or scroll by whichever they do more of in their first
/// this many points.
const GESTURE_CLEAR: f32 = 10.0;
/// A tap's click or Back lasts this long at least: the engine reads the
/// mouse and keys once a frame.
const MIN_PRESS: Duration = Duration::from_millis(60);
/// A click starts this long after the pointer moves under the finger: the
/// menus take a press only where the pointer already was (the first tap's
/// was lost).
const CLICK_DELAY: Duration = Duration::from_millis(40);

/// While the phone is out, its keys (the D-pad, accept, back) join the
/// layout, in the free space right of the middle; the layout's own D-pad
/// buttons give way.
const DPAD_BITS: u16 = button::UP | button::DOWN | button::LEFT | button::RIGHT;
static PHONE_KEYS: &[Action] = &[
    action("▲", "arrow-up", button::UP, At::RightTop(250.0, 76.0), 20.0),
    action("▼", "arrow-down", button::DOWN, At::RightTop(250.0, 164.0), 20.0),
    action("◀", "arrow-left", button::LEFT, At::RightTop(294.0, 120.0), 20.0),
    action("▶", "arrow-right", button::RIGHT, At::RightTop(206.0, 120.0), 20.0),
    action("Select", "check", button::SOUTH, At::RightTop(370.0, 90.0), 24.0),
    action("Back", "turn-left", button::EAST, At::RightTop(370.0, 154.0), 24.0),
];

/// Past this many stick radii from where it landed, the thumb sprints (on
/// foot); past this many, the stick follows it.
const SPRINT_REACH: f32 = 1.3;
const FOLLOW_REACH: f32 = 1.7;
/// The mouse counts a finger dragged one point turns the camera by, at
/// camera speed 1 (the game's mouse sensitivity setting applies on top).
const LOOK_GAIN: f32 = 16.0;
/// While the aim is locked on, a finger dragged this many points in a frame
/// pushes the right stick all the way (a swipe switches targets; a thumb's
/// tremor stays in the game's dead zone). A mouse's motion would break the
/// lock instead.
const LOCKED_SWIPE: f32 = 8.0;

#[derive(Clone, Copy, Debug)]
enum Finger {
    /// A stick held: where the thumb landed and where it is.
    Move {
        origin: Pos2,
        at: Pos2,
    },
    /// The camera, turned by the finger's drag: where it landed (or was
    /// when a wheel opened), where it was last. While a wheel is open, it
    /// picks in it instead, as the right stick.
    Camera { origin: Pos2, last: Pos2 },
    /// A button of the touch layouts held: where the finger landed, where
    /// it was last.
    Action(&'static Action, Pos2, Pos2),
    /// In the pause menu: the mouse, or a second finger pinching, or Back.
    Mouse(Pos2),
    Pinch(Pos2),
    MenuBack,
}

/// What the fingers do with the mouse and keys, for the pause menu.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mouse {
    /// Where the pointer goes, 0..1 across the screen.
    pub at: Option<Pos2>,
    pub down: bool,
    /// Wheel notches, + = up (zoom in).
    pub wheel: i32,
    /// Esc held.
    pub back: bool,
}

pub struct Touch {
    fingers: HashMap<u64, Finger>,
    /// Buttons pressed since the last [`Touch::gamepad`]: a tap whose finger
    /// lifts before the gamepad is read (both in one batch of events) still
    /// presses its button once.
    pressed: u16,
    /// Points per unit of SDL's normalised finger coordinates.
    screen: Vec2,
    /// Where the controls go: clear of notches and rounded corners.
    area: Rect,
    scale: f32,
    /// A tap under each button or D-pad direction pressed.
    pub haptics: bool,
    /// The camera's speed (the setting).
    pub camera_speed: f32,
    /// The player's aim is locked on to a target: drags push the right
    /// stick instead of turning the camera.
    pub aim_locked: bool,
    /// What the player is doing: the layout follows.
    context: PlayerContext,
    /// The camera's turn since the last [`Touch::take_look`], in mouse
    /// counts.
    look: Vec2,
    /// While locked on: the drag since the last [`Touch::gamepad`], and the
    /// right stick it made (easing off once the finger stops).
    locked_drag: Vec2,
    locked_stick: Vec2,
    /// The buttons' icons, loaded when first drawn.
    icons: std::cell::OnceCell<Icons>,
    /// The pause menu's mouse: where it is, its click from and until when
    /// at least, Back held until when at least, wheel notches not taken
    /// yet, two fingers on it.
    mouse_at: Option<Pos2>,
    click: Option<(Instant, Instant)>,
    back_until: Option<Instant>,
    wheel: f32,
    two: Option<TwoFingers>,
}

/// Two fingers in the pause menu: how far apart they were last and the
/// height of their middle, how much of each changed while the gesture was
/// unclear, the gesture.
#[derive(Clone, Copy, Debug)]
struct TwoFingers {
    width: f32,
    middle: f32,
    unclear: Vec2,
    gesture: Option<Gesture>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    Pinch,
    Scroll,
}

impl Touch {
    pub fn new() -> Self {
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1.0, 1.0));
        Touch {
            fingers: HashMap::new(),
            pressed: 0,
            screen: area.size(),
            area,
            scale: 1.0,
            haptics: false,
            camera_speed: 1.0,
            aim_locked: false,
            context: PlayerContext {
                activity: Activity::OnFoot,
                vehicle_near: false,
                phone: false,
                menu: false,
                cutscene: false,
            },
            look: Vec2::ZERO,
            locked_drag: Vec2::ZERO,
            locked_stick: Vec2::ZERO,
            icons: std::cell::OnceCell::new(),
            mouse_at: None,
            click: None,
            back_until: None,
            wheel: 0.0,
            two: None,
        }
    }

    /// The window's size and safe area in points, and the controls' size
    /// setting.
    pub fn layout(&mut self, screen: Vec2, area: Rect, scale: f32) {
        self.screen = screen;
        self.area = area;
        let phone = screen.x.min(screen.y) < PHONE_SIDE;
        self.scale = scale * if phone { PHONE_SCALE } else { 1.0 };
    }

    /// What the player is doing now. Fingers down keep what they hold.
    pub fn set_context(&mut self, context: PlayerContext) {
        // In and out of the pause menu, fingers start over (a finger on a
        // button is not the mouse, nor the other way round).
        if context.menu != self.context.menu {
            self.clear();
        }
        self.context = context;
    }


    /// The touch layout for what the player is doing (kept from the last
    /// activity seen through the moments between), or none: the pause menu.
    fn touch_layout(&self) -> Option<&'static [Action]> {
        if self.context.menu {
            return None;
        }
        if self.context.cutscene {
            return Some(CUTSCENE);
        }
        Some(match self.context.activity {
            Activity::OnFoot => ON_FOOT,
            Activity::Driving(Driving::Car) => CAR,
            Activity::Driving(Driving::Bicycle) => BICYCLE,
            Activity::Driving(Driving::Heli) => HELI,
            Activity::Driving(Driving::Plane) => PLANE,
            Activity::Driving(Driving::Submarine) => SUBMARINE,
            Activity::Passenger => PASSENGER,
            Activity::Parachute => PARACHUTE,
        })
    }

    /// The layout has a stick (all but a cutscene's).
    fn has_stick(&self) -> bool {
        self.touch_layout().is_some_and(|l| !std::ptr::eq(l, CUTSCENE))
    }

    fn on_foot(&self) -> bool {
        self.touch_layout().is_some_and(|l| std::ptr::eq(l, ON_FOOT))
    }

    /// The touch layout's buttons shown now.
    fn actions(&self) -> impl Iterator<Item = &'static Action> + '_ {
        let phone = self.context.phone;
        let layout = self.touch_layout().unwrap_or_default();
        let keys = if phone && self.has_stick() { PHONE_KEYS } else { &[] };
        layout
            .iter()
            .filter(move |a| (!a.near_vehicle || self.context.vehicle_near) && !(phone && a.bits & DPAD_BITS != 0))
            .chain(keys)
    }

    /// The camera's turn since the last call, in whole mouse counts (the
    /// fraction left waits for the next).
    pub fn take_look(&mut self) -> (i32, i32) {
        let whole = self.look.round();
        self.look -= whole;
        (whole.x as i32, whole.y as i32)
    }

    fn pos(&self, at: At) -> Pos2 {
        let (s, a) = (self.scale, self.area);
        match at {
            At::LeftTop(x, y) => pos2(a.min.x + x * s, a.min.y + y * s),
            At::LeftBottom(x, y) => pos2(a.min.x + x * s, a.max.y - y * s),
            At::RightTop(x, y) => pos2(a.max.x - x * s, a.min.y + y * s),
            At::RightBottom(x, y) => pos2(a.max.x - x * s, a.max.y - y * s),
            At::MidTop(x, y) => pos2(a.center().x + x * s, a.min.y + y * s),
        }
    }

    fn stick_radius(&self) -> f32 {
        STICK_RADIUS * self.scale
    }

    pub fn finger_down(&mut self, id: u64, x: f32, y: f32) {
        let p = pos2(x * self.screen.x, y * self.screen.y);
        let finger = if self.context.menu { self.menu_finger(p) } else { self.touch_finger(p) };
        if let Finger::Action(a, ..) = finger {
            self.pressed |= a.bits;
        }
        if let Finger::Action(a, ..) = finger
            && a.kind == Kind::Wheel
        {
            // Fingers on the camera now pick in the wheel, from where they are.
            for f in self.fingers.values_mut() {
                if let Finger::Camera { origin, last } = f {
                    *origin = *last;
                }
            }
        }
        if self.haptics && matches!(finger, Finger::Action(..)) {
            haptics::tap();
        }
        self.fingers.insert(id, finger);
    }

    /// In a touch layout: what a finger landing at `p` takes.
    fn touch_finger(&self, p: Pos2) -> Finger {
        let hit = |a: &Action| self.pos(a.at).distance(p) <= a.radius * self.scale * 1.2;
        if let Some(a) = self.actions().find(|a| hit(a)) {
            Finger::Action(a, p, p)
        } else if p.x < self.screen.x / 2.0 && self.has_stick() {
            Finger::Move { origin: p, at: p }
        } else {
            Finger::Camera { origin: p, last: p }
        }
    }

    /// In the pause menu: Back, else the mouse, or a pinch with it.
    fn menu_finger(&mut self, p: Pos2) -> Finger {
        if self.pos(MENU_BACK.at).distance(p) <= MENU_BACK.radius * self.scale * 1.2 {
            self.back_until = Some(Instant::now() + MIN_PRESS);
            return Finger::MenuBack;
        }
        let mouse = self.fingers.values().find_map(|f| if let Finger::Mouse(at) = f { Some(*at) } else { None });
        match mouse {
            Some(at) => {
                // A second finger: a pinch or a scroll, not a click.
                let (width, middle) = (at.distance(p), (at.y + p.y) / 2.0);
                self.two = Some(TwoFingers { width, middle, unclear: Vec2::ZERO, gesture: None });
                self.click = None;
                Finger::Pinch(p)
            }
            None => {
                self.mouse_at = Some(p);
                let from = Instant::now() + CLICK_DELAY;
                self.click = Some((from, from + MIN_PRESS));
                Finger::Mouse(p)
            }
        }
    }

    /// Takes what the fingers do with the mouse and keys in the pause menu.
    pub fn take_mouse(&mut self) -> Mouse {
        let now = Instant::now();
        let held = |f: &Finger| matches!(f, Finger::Mouse(_));
        let pinching = self.fingers.values().any(|f| matches!(f, Finger::Pinch(_)));
        let down = !pinching
            && self.click.is_some_and(|(from, until)| now >= from && (now < until || self.fingers.values().any(held)));
        let back = self.fingers.values().any(|f| matches!(f, Finger::MenuBack)) || self.back_until.is_some_and(|t| now < t);
        let wheel = self.wheel.trunc();
        self.wheel -= wheel;
        let at = self.mouse_at.map(|p| pos2(p.x / self.screen.x, p.y / self.screen.y));
        Mouse { at, down, wheel: wheel as i32, back }
    }

    pub fn finger_motion(&mut self, id: u64, x: f32, y: f32) {
        let p = pos2(x * self.screen.x, y * self.screen.y);
        if self.context.menu {
            if let Some(Finger::Mouse(at) | Finger::Pinch(at)) = self.fingers.get_mut(&id) {
                *at = p;
            }
            if matches!(self.fingers.get(&id), Some(Finger::Mouse(_))) {
                self.mouse_at = Some(p);
            }
            let points: Vec<Pos2> = self
                .fingers
                .values()
                .filter_map(|f| match f {
                    Finger::Mouse(at) | Finger::Pinch(at) => Some(*at),
                    _ => None,
                })
                .collect();
            if let (&[a, b], Some(two)) = (&points[..], self.two.as_mut()) {
                let (width, middle) = (a.distance(b), (a.y + b.y) / 2.0);
                let (wider, lower) = (width - two.width, middle - two.middle);
                (two.width, two.middle) = (width, middle);
                if two.gesture.is_none() {
                    two.unclear += vec2(wider.abs(), lower.abs());
                    let u = two.unclear;
                    if u.max_elem() > GESTURE_CLEAR {
                        two.gesture = Some(if u.x >= u.y { Gesture::Pinch } else { Gesture::Scroll });
                    }
                }
                // Natural scrolling: fingers moving down bring up what is above.
                match two.gesture {
                    Some(Gesture::Pinch) => self.wheel += wider / PINCH_NOTCH,
                    Some(Gesture::Scroll) => self.wheel += lower / SCROLL_NOTCH,
                    None => {}
                }
            }
            return;
        }
        let follow = FOLLOW_REACH * self.stick_radius();
        let gain = LOOK_GAIN * self.camera_speed;
        let wheel = self.wheel_open();
        let mut turned = Vec2::ZERO;
        match self.fingers.get_mut(&id) {
            Some(Finger::Move { origin, at }) => {
                *at = p;
                // Far past its reach, the stick comes along under the thumb.
                let d = p - *origin;
                if d.length() > follow {
                    *origin = p - d.normalized() * follow;
                }
            }
            Some(Finger::Camera { last, .. }) => {
                if !wheel {
                    turned = p - *last;
                }
                *last = p;
            }
            Some(Finger::Action(a, _, last)) => {
                if a.kind == Kind::Turns {
                    turned = p - *last;
                }
                *last = p;
            }
            _ => {}
        }
        if turned != Vec2::ZERO {
            self.turn(turned, gain);
        }
    }

    /// A finger dragged by `d` turns the camera, or while locked on pushes
    /// the right stick.
    fn turn(&mut self, d: Vec2, gain: f32) {
        if self.aim_locked {
            self.locked_drag += d;
        } else {
            self.look += d * gain;
        }
    }

    pub fn finger_up(&mut self, id: u64) {
        if matches!(self.fingers.remove(&id), Some(Finger::Pinch(_))) {
            self.two = None;
        }
    }

    pub fn clear(&mut self) {
        self.fingers.clear();
        self.pressed = 0;
        self.look = Vec2::ZERO;
        self.locked_drag = Vec2::ZERO;
        self.locked_stick = Vec2::ZERO;
        self.click = None;
        self.back_until = None;
        self.wheel = 0.0;
        self.two = None;
    }

    /// The gamepad the fingers make now.
    pub fn gamepad(&mut self) -> Gamepad {
        let mut pad = Gamepad { connected: true, buttons: std::mem::take(&mut self.pressed), ..Default::default() };
        if self.context.menu {
            return Gamepad { buttons: 0, ..pad };
        }
        let radius = self.stick_radius();
        let stick = |origin: Pos2, at: Pos2| stick_value((at - origin) / radius);
        for finger in self.fingers.values() {
            match *finger {
                Finger::Move { origin, at } => {
                    pad.left = stick(origin, at);
                    // On foot, a thumb pushed past the ring sprints (A held).
                    if self.on_foot() && (at - origin).length() > SPRINT_REACH * radius {
                        pad.buttons |= button::SOUTH;
                    }
                }
                Finger::Camera { .. } | Finger::Mouse(_) | Finger::Pinch(_) | Finger::MenuBack => {}
                Finger::Action(a, origin, at) => {
                    if a.kind == Kind::Wheel {
                        pad.right = stick(origin, at);
                    }
                    pad.buttons |= a.bits;
                    if a.bits == button::L2 {
                        pad.l2 = 1.0;
                    }
                    if a.bits == button::R2 {
                        pad.r2 = 1.0;
                    }
                }
            }
        }
        // Locked on, the drags of this frame push the right stick.
        let drag = std::mem::take(&mut self.locked_drag);
        self.locked_stick = if !self.aim_locked {
            Vec2::ZERO
        } else if drag != Vec2::ZERO {
            drag / LOCKED_SWIPE
        } else {
            self.locked_stick * 0.5
        };
        if self.locked_stick.length() > 0.05 {
            pad.right = stick_value(self.locked_stick);
        }
        // An open wheel picks with the other thumb's drag too (the wheels'
        // buttons sit on the left).
        if self.wheel_open() {
            for finger in self.fingers.values() {
                if let Finger::Camera { origin, last } = *finger
                    && origin != last
                {
                    pad.right = stick(origin, last);
                }
            }
        }
        pad
    }

    /// A wheel's button is held: the wheel is open.
    fn wheel_open(&self) -> bool {
        self.fingers.values().any(|f| matches!(f, Finger::Action(a, ..) if a.kind == Kind::Wheel))
    }

    pub fn draw(&self, painter: &Painter, opacity: f32) {
        if self.context.menu {
            let back = self.fingers.values().any(|f| matches!(f, Finger::MenuBack));
            self.draw_button(painter, opacity, self.pos(MENU_BACK.at), MENU_BACK.radius, MENU_BACK.icon, back);
            return;
        }
        self.draw_touch(painter, opacity);
    }

    /// A round button of radius `radius` (unscaled) at `c`, with its icon
    /// (or two side by side).
    fn draw_button(&self, painter: &Painter, opacity: f32, c: Pos2, radius: f32, icon: &str, held: bool) {
        let alpha = |a: f32| (a * opacity * 255.0) as u8;
        let fill = Color32::from_white_alpha(alpha(if held { 0.45 } else { 0.18 }));
        let stroke = Stroke::new(1.5 * self.scale, Color32::from_white_alpha(alpha(0.6)));
        let r = radius * self.scale;
        painter.circle(c, r, fill, stroke);
        let icons = self.icons.get_or_init(|| load_icons(painter.ctx()));
        let names: Vec<&str> = icon.split('+').collect();
        let side = r * if names.len() > 1 { 0.78 } else { 1.15 };
        let tint = Color32::from_white_alpha(alpha(0.9));
        let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
        for (i, name) in names.iter().enumerate() {
            let dx = (i as f32 - (names.len() - 1) as f32 / 2.0) * side;
            if let Some(texture) = icons.get(name) {
                painter.image(texture.id(), Rect::from_center_size(c + vec2(dx, 0.0), Vec2::splat(side)), uv, tint);
            }
        }
    }

    fn draw_touch(&self, painter: &Painter, opacity: f32) {
        let alpha = |a: f32| (a * opacity * 255.0) as u8;
        let fill = Color32::from_white_alpha(alpha(0.18));
        let pressed = Color32::from_white_alpha(alpha(0.45));
        let stroke = Stroke::new(1.5 * self.scale, Color32::from_white_alpha(alpha(0.6)));
        for a in self.actions() {
            let held = self.fingers.values().any(|f| matches!(f, Finger::Action(b, ..) if std::ptr::eq(*b, a)));
            self.draw_button(painter, opacity, self.pos(a.at), a.radius, a.icon, held);
        }
        if !self.has_stick() {
            return;
        }
        // The stick, where held or at rest (on foot, with its sprint ring).
        let radius = self.stick_radius();
        let held = self.fingers.values().find_map(|f| match *f {
            Finger::Move { origin, at } => Some((origin, at)),
            _ => None,
        });
        let rest = self.pos(MOVE_REST);
        let (origin, at) = held.unwrap_or((rest, rest));
        let d = at - origin;
        let sprint = d.length() > SPRINT_REACH * radius;
        let knob = if d.length() > radius { origin + d.normalized() * radius } else { at };
        painter.circle(origin, radius, fill, stroke);
        if self.on_foot() {
            let ring = Color32::from_white_alpha(alpha(if sprint { 0.8 } else { 0.25 }));
            painter.circle_stroke(origin, SPRINT_REACH * radius, Stroke::new(1.5 * self.scale, ring));
        }
        painter.circle(knob, radius * 0.45, pressed, stroke);
    }
}

/// A stick's value for a thumb this far from where it landed (in stick
/// radii): the circle of its reach stretched onto the square, so that the
/// axis the thumb leans along reads as far as the thumb went. A thumb sliding
/// sideways drifts up or down as well; on the circle, 30 degrees of drift
/// left the steering at 0.87 at full reach.
fn stick_value(v: Vec2) -> [f32; 2] {
    let reach = v.length().min(1.0);
    let lean = v.x.abs().max(v.y.abs());
    if lean == 0.0 {
        return [0.0, 0.0];
    }
    let v = v * (reach / lean);
    [v.x, v.y]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An iPhone 13 Pro in landscape: 844 x 390 points, the notch's inset on
    /// the left, the home indicator's at the bottom.
    fn phone() -> Touch {
        let mut t = Touch::new();
        let screen = vec2(844.0, 390.0);
        t.layout(screen, Rect::from_min_max(pos2(47.0, 0.0), pos2(797.0, 369.0)), 1.0);
        t
    }

    #[test]
    fn locked_on_a_drag_pushes_the_right_stick() {
        let mut t = phone();
        t.set_context(context(Activity::OnFoot));
        let at = |t: &Touch, p: Pos2| (p.x / t.screen.x, p.y / t.screen.y);
        // Aim held, its thumb trembling: no turn, a stick in the dead zone.
        let aim = ON_FOOT.iter().find(|a| a.bits == button::L2).unwrap();
        let p = t.pos(aim.at);
        let (x, y) = at(&t, p);
        t.finger_down(1, x, y);
        t.aim_locked = true;
        let (x, y) = at(&t, p + vec2(1.0, 0.0));
        t.finger_motion(1, x, y);
        let pad = t.gamepad();
        assert!(pad.right[0] < 0.2 && pad.l2 == 1.0);
        assert_eq!(t.take_look(), (0, 0));
        // A swipe on the camera: the stick all the way, then easing off.
        let c = pos2(560.0, 160.0);
        let (x, y) = at(&t, c);
        t.finger_down(2, x, y);
        let (x, y) = at(&t, c + vec2(-2.0 * LOCKED_SWIPE, 0.0));
        t.finger_motion(2, x, y);
        assert_eq!(t.gamepad().right, [-1.0, 0.0]);
        assert_eq!(t.gamepad().right, [-1.0, 0.0]);
        assert!(t.gamepad().right[0] > -1.0);
        assert_eq!(t.take_look(), (0, 0));
        // Unlocked, drags turn the camera again.
        t.aim_locked = false;
        let (x, y) = at(&t, c);
        t.finger_motion(2, x, y);
        assert_eq!(t.gamepad().right, [0.0, 0.0]);
        assert_ne!(t.take_look(), (0, 0));
    }

    #[test]
    fn a_tap_lifting_before_the_pad_is_read_presses_once() {
        let mut t = phone();
        t.set_context(context(Activity::OnFoot));
        let jump = ON_FOOT.iter().find(|a| a.bits == button::WEST).unwrap();
        let p = t.pos(jump.at);
        t.finger_down(4, p.x / t.screen.x, p.y / t.screen.y);
        t.finger_up(4);
        assert_eq!(t.gamepad().buttons, button::WEST);
        assert_eq!(t.gamepad().buttons, 0);
    }

    fn context(activity: Activity) -> PlayerContext {
        PlayerContext { activity, vehicle_near: true, phone: false, menu: false, cutscene: false }
    }

    const ACTIVITIES: [Activity; 8] = [
        Activity::OnFoot,
        Activity::Driving(Driving::Car),
        Activity::Driving(Driving::Bicycle),
        Activity::Driving(Driving::Heli),
        Activity::Driving(Driving::Plane),
        Activity::Driving(Driving::Submarine),
        Activity::Passenger,
        Activity::Parachute,
    ];

    #[test]
    fn touch_layouts_fit_a_phone_without_overlapping() {
        let mut t = phone();
        let cases = ACTIVITIES.into_iter().flat_map(|a| [(a, false, false), (a, true, false)]);
        for (activity, phone, cutscene) in cases.chain([(Activity::OnFoot, false, true)]) {
            t.set_context(PlayerContext { phone, cutscene, ..context(activity) });
            let what = format!("{activity:?}{}{}", if phone { " phone" } else { "" }, if cutscene { " cutscene" } else { "" });
            let ring = if activity == Activity::OnFoot { SPRINT_REACH } else { 1.0 };
            let mut all: Vec<_> = t.actions().map(|a| (a.label, t.pos(a.at), a.radius * t.scale)).collect();
            if t.has_stick() {
                all.push(("stick", t.pos(MOVE_REST), ring * t.stick_radius()));
            }
            all.push(("gear", pos2(t.area.max.x - 156.0, t.area.min.y + 40.0), 20.0));
            for (i, (a, pa, ra)) in all.iter().enumerate() {
                let r = Rect::from_center_size(*pa, Vec2::splat(2.0 * ra));
                assert!(t.area.expand(1.0).contains_rect(r), "{what}: {a} leaves the safe area");
                for (b, pb, rb) in &all[i + 1..] {
                    assert!(pa.distance(*pb) > ra + rb, "{what}: {a} overlaps {b}");
                }
            }
        }
    }

    #[test]
    fn the_layout_follows_the_player() {
        let mut t = phone();
        let layout = |t: &Touch| t.touch_layout().map(|l| l[0].label);
        t.set_context(context(Activity::OnFoot));
        assert_eq!(layout(&t), Some("Fire"));
        t.set_context(context(Activity::Driving(Driving::Bicycle)));
        assert_eq!(layout(&t), Some("Pedal"));
        t.set_context(context(Activity::Parachute));
        assert_eq!(layout(&t), Some("Open"));
        t.set_context(PlayerContext { cutscene: true, ..context(Activity::Passenger) });
        assert_eq!(layout(&t), Some("Skip"));
        assert!(!t.has_stick());
        // The phone's keys join the layout, instead of its D-pad buttons.
        t.set_context(PlayerContext { phone: true, ..context(Activity::OnFoot) });
        assert_eq!(layout(&t), Some("Fire"));
        assert_eq!(t.actions().filter(|a| a.bits & DPAD_BITS != 0).count(), 4);
        assert!(t.actions().any(|a| a.label == "Back"));
        // Enter only next to a vehicle.
        t.set_context(PlayerContext { vehicle_near: false, ..context(Activity::OnFoot) });
        assert!(t.actions().all(|a| a.bits != button::NORTH));
        // The pause menu: no layout, nor pad.
        t.set_context(PlayerContext { menu: true, ..context(Activity::OnFoot) });
        assert_eq!(layout(&t), None);
        assert_eq!(t.gamepad().buttons, 0);
    }

    #[test]
    fn in_the_pause_menu_fingers_are_the_mouse() {
        let mut t = phone();
        t.set_context(PlayerContext { menu: true, ..context(Activity::OnFoot) });
        let at = |t: &Touch, p: Pos2| (p.x / t.screen.x, p.y / t.screen.y);
        // A tap: the pointer there, a click that lasts until the engine sees it.
        let (x, y) = at(&t, pos2(400.0, 200.0));
        t.finger_down(1, x, y);
        t.finger_up(1);
        let m = t.take_mouse();
        assert!(!m.down && !m.back, "the pointer moves first");
        assert!((m.at.unwrap().x - x).abs() < 1e-6);
        std::thread::sleep(CLICK_DELAY);
        assert!(t.take_mouse().down);
        // A drag moves it, button down; a second finger pinches: the wheel.
        t.finger_down(1, x, y);
        let (x2, y2) = at(&t, pos2(450.0, 220.0));
        t.finger_motion(1, x2, y2);
        assert!((t.take_mouse().at.unwrap().x - x2).abs() < 1e-6);
        let (px, py) = at(&t, pos2(450.0, 320.0));
        t.finger_down(2, px, py);
        let (px, py) = at(&t, pos2(450.0, 320.0 + 2.0 * PINCH_NOTCH));
        t.finger_motion(2, px, py);
        let m = t.take_mouse();
        assert_eq!((m.wheel, m.down), (2, false));
        // Two fingers dragged up together: the wheel scrolls down.
        t.clear();
        let (ax, ay) = at(&t, pos2(400.0, 300.0));
        t.finger_down(1, ax, ay);
        let (bx, by) = at(&t, pos2(500.0, 300.0));
        t.finger_down(2, bx, by);
        let up = 3.0 * SCROLL_NOTCH + GESTURE_CLEAR;
        for (id, x) in [(1, 400.0), (2, 500.0)] {
            for step in 1..=4 {
                let (x, y) = at(&t, pos2(x, 300.0 - up * step as f32 / 4.0));
                t.finger_motion(id, x, y);
            }
        }
        let m = t.take_mouse();
        assert_eq!((m.wheel, m.down), (-3, false));
        // Back: Esc.
        t.clear();
        let back = t.pos(MENU_BACK.at);
        let (x, y) = at(&t, back);
        t.finger_down(3, x, y);
        assert!(t.take_mouse().back);
    }

    #[test]
    fn on_foot_fingers_walk_sprint_and_turn_the_camera() {
        let mut t = phone();
        t.set_context(context(Activity::OnFoot));
        let at = |t: &Touch, p: Pos2| (p.x / t.screen.x, p.y / t.screen.y);
        let r = t.stick_radius();
        // The stick where the left thumb lands; past its ring, a sprint.
        let o = pos2(200.0, 250.0);
        let (x, y) = at(&t, o);
        t.finger_down(1, x, y);
        let (x, y) = at(&t, o + vec2(0.0, -0.5 * r));
        t.finger_motion(1, x, y);
        let pad = t.gamepad();
        assert!((pad.left[1] + 0.5).abs() < 1e-3 && pad.buttons & button::SOUTH == 0);
        let (x, y) = at(&t, o + vec2(0.0, -1.5 * r));
        t.finger_motion(1, x, y);
        let pad = t.gamepad();
        assert_eq!((pad.left, pad.buttons & button::SOUTH), ([0.0, -1.0], button::SOUTH));
        // Dragging on the right turns the camera by as much, nothing else.
        let c = pos2(560.0, 160.0);
        let (x, y) = at(&t, c);
        t.finger_down(2, x, y);
        let (x, y) = at(&t, c + vec2(10.0, -5.0));
        t.finger_motion(2, x, y);
        assert_eq!(t.take_look(), ((10.0 * LOOK_GAIN) as i32, (-5.0 * LOOK_GAIN) as i32));
        assert_eq!(t.take_look(), (0, 0));
        assert_eq!(t.gamepad().right, [0.0, 0.0]);
        // Fire held fires and turns the camera.
        let fire = ON_FOOT.iter().find(|a| a.bits == button::R2).unwrap();
        let p = t.pos(fire.at);
        let (x, y) = at(&t, p);
        t.finger_down(3, x, y);
        let (x, y) = at(&t, p + vec2(-4.0, 0.0));
        t.finger_motion(3, x, y);
        assert_eq!(t.gamepad().buttons & button::R2, button::R2);
        assert_eq!(t.take_look(), ((-4.0 * LOOK_GAIN) as i32, 0));
    }

    #[test]
    fn a_wheel_is_picked_by_dragging_from_its_button() {
        let mut t = phone();
        t.set_context(context(Activity::OnFoot));
        let at = |t: &Touch, p: Pos2| (p.x / t.screen.x, p.y / t.screen.y);
        let weapons = ON_FOOT.iter().find(|a| a.label == "Weapons").unwrap();
        let p = t.pos(weapons.at);
        let (x, y) = at(&t, p);
        t.finger_down(1, x, y);
        let (x, y) = at(&t, p + vec2(2.0 * t.stick_radius(), 0.0));
        t.finger_motion(1, x, y);
        let pad = t.gamepad();
        assert_eq!((pad.buttons & button::L1, pad.right), (button::L1, [1.0, 0.0]));
        assert_eq!(t.take_look(), (0, 0));
    }

    #[test]
    fn an_open_wheel_picks_with_the_other_thumb() {
        let mut t = phone();
        t.set_context(context(Activity::Driving(Driving::Car)));
        let at = |t: &Touch, p: Pos2| (p.x / t.screen.x, p.y / t.screen.y);
        // A thumb already on the camera, then the radio held with the other.
        let c = pos2(560.0, 160.0);
        let (x, y) = at(&t, c);
        t.finger_down(1, x, y);
        let (x, y) = at(&t, c + vec2(5.0, 0.0));
        t.finger_motion(1, x, y);
        let radio = CAR.iter().find(|a| a.label == "Radio").unwrap();
        let (x, y) = at(&t, t.pos(radio.at));
        t.finger_down(2, x, y);
        t.take_look();
        // Its drag picks from where it was, the camera stays.
        let (x, y) = at(&t, c + vec2(5.0, -2.0 * t.stick_radius()));
        t.finger_motion(1, x, y);
        let pad = t.gamepad();
        assert_eq!((pad.buttons & button::LEFT, pad.right), (button::LEFT, [0.0, -1.0]));
        assert_eq!(t.take_look(), (0, 0));
        // The radio let go: the camera again.
        t.finger_up(2);
        let (x, y) = at(&t, c + vec2(15.0, -2.0 * t.stick_radius()));
        t.finger_motion(1, x, y);
        assert_eq!(t.gamepad().right, [0.0, 0.0]);
        assert_eq!(t.take_look(), ((10.0 * LOOK_GAIN) as i32, 0));
    }

    #[test]
    fn driving_has_its_pedals() {
        let mut t = phone();
        t.set_context(context(Activity::Driving(Driving::Car)));
        let at = |t: &Touch, p: Pos2| (p.x / t.screen.x, p.y / t.screen.y);
        for (id, label) in [(1, "Gas"), (2, "Brake")] {
            let a = CAR.iter().find(|a| a.label == label).unwrap();
            let (x, y) = at(&t, t.pos(a.at));
            t.finger_down(id, x, y);
        }
        let pad = t.gamepad();
        assert_eq!((pad.r2, pad.l2), (1.0, 1.0));
        // Steering, without a sprint.
        let (x, y) = at(&t, pos2(200.0, 250.0));
        t.finger_down(3, x, y);
        let (x, y) = at(&t, pos2(200.0 - 2.0 * t.stick_radius(), 250.0));
        t.finger_motion(3, x, y);
        let pad = t.gamepad();
        assert_eq!((pad.left, pad.buttons & button::SOUTH), ([-1.0, 0.0], 0));
    }

    #[test]
    fn every_icon_loads() {
        let ctx = egui::Context::default();
        let icons = load_icons(&ctx);
        assert_eq!(icons.len(), ICON_FILES.len(), "an icon does not decode");
        let layouts = [ON_FOOT, CAR, BICYCLE, HELI, PLANE, SUBMARINE, PASSENGER, PARACHUTE, CUTSCENE, PHONE_KEYS];
        let names = layouts.iter().flat_map(|l| l.iter().flat_map(|a| a.icon.split('+')));
        for name in names.chain([MENU_BACK.icon]) {
            assert!(icons.contains_key(name), "no icon {name}");
        }
    }

    #[test]
    fn a_thumb_drifting_off_the_axis_keeps_its_reach() {
        let at = |deg: f32, reach: f32| stick_value(Vec2::angled(deg.to_radians()) * reach);
        let near = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4;
        // Sideways and 30 degrees up, all the way: full steering.
        assert!(near(at(-150.0, 2.0), [-1.0, -(30f32.to_radians().tan())]));
        // Half way: half.
        assert!((at(-30.0, 0.5)[0] - 0.5).abs() < 1e-4);
        assert!(near(at(90.0, 1.0), [0.0, 1.0]));
        assert!(near(at(45.0, 1.0), [1.0, 1.0]));
        assert_eq!(stick_value(Vec2::ZERO), [0.0, 0.0]);
    }
}

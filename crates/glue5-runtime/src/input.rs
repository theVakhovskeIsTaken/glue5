//! Input, platform independent.
//!
//! - Keyboard and mouse go to the engine's input block, which the browser page
//!   used to fill (layout from index.html / browser_input_wasm.cpp).
//! - Gamepads (physical or virtual, e.g. touch controls) feed the engine's own
//!   pad code: the `pad_update` hook writes [`Gamepad`] into `rage::ioPad`
//!   after the original update, exactly where XInput would on PC.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use glue5_game::{Env, orig};

use crate::memory::Mem;

// ---- gamepads ----------------------------------------------------------------

/// Pad buttons, with RAGE's bit values.
pub mod button {
    pub const L2: u16 = 0x0001;
    pub const R2: u16 = 0x0002;
    pub const L1: u16 = 0x0004;
    pub const R1: u16 = 0x0008;
    /// Y / Triangle
    pub const NORTH: u16 = 0x0010;
    /// B / Circle
    pub const EAST: u16 = 0x0020;
    /// A / Cross
    pub const SOUTH: u16 = 0x0040;
    /// X / Square
    pub const WEST: u16 = 0x0080;
    /// Back / Select
    pub const SELECT: u16 = 0x0100;
    pub const L3: u16 = 0x0200;
    pub const R3: u16 = 0x0400;
    pub const START: u16 = 0x0800;
    pub const UP: u16 = 0x1000;
    pub const RIGHT: u16 = 0x2000;
    pub const DOWN: u16 = 0x4000;
    pub const LEFT: u16 = 0x8000;
}

/// State of one pad. Sticks are -1..1 with +y down (screen convention);
/// triggers 0..1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gamepad {
    pub connected: bool,
    pub buttons: u16,
    pub left: [f32; 2],
    pub right: [f32; 2],
    pub l2: f32,
    pub r2: f32,
}

pub const MAX_PADS: usize = 4;

/// Rumble motor strengths the engine asked for, per pad: (low, high frequency).
static RUMBLE: Mutex<[(u16, u16); MAX_PADS]> = Mutex::new([(0, 0); MAX_PADS]);

/// The engine's current rumble request for pad `slot` (low, high frequency).
pub fn rumble(slot: usize) -> (u16, u16) {
    RUMBLE.lock().unwrap().get(slot).copied().unwrap_or_default()
}

static PADS: Mutex<[Gamepad; MAX_PADS]> =
    Mutex::new([Gamepad { connected: false, buttons: 0, left: [0.0; 2], right: [0.0; 2], l2: 0.0, r2: 0.0 }; MAX_PADS]);

/// Buttons pressed since the engine last read each pad. The engine reads
/// the pads once a frame; a press that begins and ends in between (a quick
/// tap on a touch button: START, a face button) would never reach it, so
/// it counts as held until the engine's next read.
static PRESSED: Mutex<[u16; MAX_PADS]> = Mutex::new([0; MAX_PADS]);

/// Sets what pad `slot` reports to the engine from its next update on.
pub fn set_gamepad(slot: usize, pad: Gamepad) {
    if slot < MAX_PADS {
        let mut pads = PADS.lock().unwrap();
        PRESSED.lock().unwrap()[slot] |= pad.buttons & !pads[slot].buttons;
        pads[slot] = pad;
    }
}

// rage::ioPad (136 bytes per pad, 4 pads).
const PAD_INDEX: u64 = 8;
const PAD_BUTTONS: u64 = 12;
const PAD_AXES: u64 = 20; // LX, LY, RX, RY: one byte each, 0x80 = centre
const PAD_ANALOG: u64 = 24; // analog button values; [0] = L2, [1] = R2
const PAD_HAS_ANALOG: u64 = 55; // 1: CPad reads the analog trigger values
const PAD_MOTORS: u64 = 66; // u16 low, u16 high (ioPad::SetActuatorValue)

/// Hook for `rage::ioPad::Update(bool)`: the original runs first (it resets the
/// pad and merges the engine's own injected input), then the host's state is
/// merged on top.
///
/// (START and Back as any button: the engine's debug pad, which took them,
/// is off; see `Config::engine_args`.)
pub(crate) unsafe extern "C" fn pad_update(env: *mut Env, pad: u64) {
    unsafe { orig::pad_update(env, pad) };
    let mem = unsafe { Mem::of(env) };
    let slot = mem.u32(pad + PAD_INDEX) as usize;
    if let Some(r) = RUMBLE.lock().unwrap().get_mut(slot) {
        *r = (mem.u16(pad + PAD_MOTORS), mem.u16(pad + PAD_MOTORS + 2));
    }
    let Some(mut state) = PADS.lock().unwrap().get(slot).copied() else { return };
    state.buttons |= std::mem::take(&mut PRESSED.lock().unwrap()[slot]);
    if !state.connected {
        return;
    }
    mem.set_u32(pad + PAD_BUTTONS, mem.u32(pad + PAD_BUTTONS) | state.buttons as u32);
    let axis = |v: f32| ((v.clamp(-1.0, 1.0) + 1.0) * 127.5).round() as u8;
    for (i, v) in [state.left[0], state.left[1], state.right[0], state.right[1]].into_iter().enumerate() {
        // Keep the original value when the host reports a centred stick.
        if v.abs() > 1.0 / 128.0 {
            mem.set_u8(pad + PAD_AXES + i as u64, axis(v));
        }
    }
    let trigger = |v: f32, bit: u16| {
        if state.buttons & bit != 0 { 255 } else { (v.clamp(0.0, 1.0) * 255.0).round() as u8 }
    };
    mem.set_u8(pad + PAD_ANALOG, trigger(state.l2, button::L2));
    mem.set_u8(pad + PAD_ANALOG + 1, trigger(state.r2, button::R2));
    mem.set_u8(pad + PAD_HAS_ANALOG, 1);
}

// ---- radio wheel ---------------------------------------------------------------

/// Hook for `CRadioWheel::GetInputAxis`. The engine reads the radio wheel's
/// own inputs (`INPUT_RADIO_WHEEL_LR`/`UD`), which the control files of this
/// game data, older than the code, map to no device: the selector never
/// moved, with the mouse or a pad. It reads the selection wheel's, as the
/// weapon wheel does: the mouse (`fixed.meta`), the right stick.
pub(crate) unsafe extern "C" fn radio_wheel_axes(env: *mut Env, this: u64, x: u64, y: u64) {
    unsafe { orig::selection_wheel_axes(env, this, x, y) }
}

// ---- web browser ---------------------------------------------------------------

/// The browser's input events (`SET_INPUT_EVENT`): a mouse click, accept.
const MOUSE_ACCEPT: f64 = 1001.0;
const ACCEPT: f64 = 16.0;

thread_local! {
    /// Whether this thread is building a `SET_INPUT_EVENT` call.
    static INPUT_EVENT: Cell<bool> = const { Cell::new(false) };
}

/// Hook for `CScaleformMgr::BeginMethod`, how scripts start a method call.
pub(crate) unsafe extern "C" fn scaleform_begin_method(env: *mut Env, movie: u32, base: u32, name: u64, index: u32) -> u32 {
    let mem = unsafe { Mem::of(env) };
    INPUT_EVENT.set(mem.bytes(name, 16) == b"SET_INPUT_EVENT\0");
    unsafe { orig::scaleform_begin_method(env, movie, base, name, index) }
}

/// Hook for `CScaleformMgr::AddParamFloat`. The phone's internet app sends a
/// left click as `SET_INPUT_EVENT(1001)`, which the browser movie of this
/// game data does not handle: links opened only with Enter, accept (16),
/// which opens what the pointer is on. A click is sent as accept.
pub(crate) unsafe extern "C" fn scaleform_add_param_float(env: *mut Env, value: f64) {
    let value = if INPUT_EVENT.get() && value == MOUSE_ACCEPT { ACCEPT } else { value };
    unsafe { orig::scaleform_add_param_float(env, value) }
}

// ---- text entry ----------------------------------------------------------------

static TEXT_BOX: AtomicBool = AtomicBool::new(false);

/// Whether the game's text box is open (it wants typed text): touch devices
/// show the system keyboard meanwhile.
pub fn text_box_open() -> bool {
    TEXT_BOX.load(Ordering::Relaxed)
}

/// Hook for `CTextInputBox::Open`.
pub(crate) unsafe extern "C" fn text_box_open_hook(env: *mut Env, this: u64, params: u64) {
    unsafe { orig::text_box_open(env, this, params) };
    TEXT_BOX.store(true, Ordering::Relaxed);
}

/// Hook for `CTextInputBox::RemoveTextInputBoxMovie`: the text box closes
/// (accepted, cancelled or closed by the game).
pub(crate) unsafe extern "C" fn text_box_close_hook(env: *mut Env, this: u64) {
    unsafe { orig::text_box_close(env, this) };
    TEXT_BOX.store(false, Ordering::Relaxed);
}

// ---- touch play ------------------------------------------------------------------

/// The device the engine saw used last (an `ioSource`'s device index): a
/// pad's index, or -4 for keyboard and mouse. It picks the prompts' icons,
/// aim assist, the menus' behaviour.
const LAST_DEVICE: u64 = 24627136;
/// A task's ped, a ped's position.
const TASK_PED: u64 = 16;
const PED_POSITION: u64 = 112;
/// A vehicle's type (`VEHICLE_TYPE_*`), and a bicycle's.
const VEHICLE_TYPE: u64 = 3088;
const BICYCLE: i32 = 12;
/// The player's surroundings are scanned for a vehicle to enter every this
/// many frames on foot.
const SCAN_EVERY: u32 = 6;
/// The engine's per-frame signals below count as now for this long.
const FRESH: Duration = Duration::from_millis(250);

static TOUCH_PLAY: AtomicBool = AtomicBool::new(false);
static SIGNALS: Mutex<Signals> = Mutex::new(Signals {
    on_foot: None,
    in_vehicle: None,
    driving: None,
    parachute: None,
    cutscene: None,
    activity: Activity::OnFoot,
});
static VEHICLE_NEAR: AtomicBool = AtomicBool::new(false);
static SCAN_FRAME: AtomicU32 = AtomicU32::new(0);
static PHONE_OUT: AtomicBool = AtomicBool::new(false);
static PAUSE_MENU: AtomicBool = AtomicBool::new(false);

/// When the engine last ran each of the player's tasks.
struct Signals {
    on_foot: Option<Instant>,
    in_vehicle: Option<Instant>,
    driving: Option<(Driving, Instant)>,
    parachute: Option<Instant>,
    cutscene: Option<Instant>,
    /// The activity last seen, kept while none is (a ragdoll, a scripted
    /// scene, the moment between two tasks).
    activity: Activity,
}

/// The touch controls are in use: the camera turns with the mouse (a
/// finger's drag) while the engine keeps playing as with a pad, out of the
/// pause menu (where they act as a mouse).
pub fn set_touch_play(on: bool) {
    TOUCH_PLAY.store(on, Ordering::Relaxed);
}

/// The pause menu's Targeting Mode (`PREF_TARGET_CONFIG`, in its array of
/// u32 preferences): the aim assist with a pad. Keyboard and mouse always
/// aim freely.
const PREF_TARGETING: u64 = 19566112 + 4 * 3;
/// The aim assist during touch play (a [`Targeting`]), `NO_TARGETING` until
/// the app sets it.
static TOUCH_TARGETING: AtomicU32 = AtomicU32::new(NO_TARGETING);
const NO_TARGETING: u32 = u32::MAX;
/// The menu's Targeting Mode while touch play's stands in for it.
static MENU_TARGETING: Mutex<Option<i32>> = Mutex::new(None);

/// The game's targeting modes (its menu's names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Targeting {
    /// Assisted Aim - Full: aiming locks on to the nearest target and
    /// follows it; the right stick switches between targets.
    Full = 0,
    /// Assisted Aim - Partial: aiming snaps to the target, then the right
    /// stick aims freely around it.
    Partial = 1,
    /// Free Aim - Assisted: the camera slows down over targets.
    FreeAssisted = 2,
    /// Free Aim.
    Free = 3,
}

/// The aim assist during touch play, in place of the menu's (shown and
/// kept there) out of the pause menu.
pub fn set_touch_targeting(targeting: Targeting) {
    TOUCH_TARGETING.store(targeting as u32, Ordering::Relaxed);
}

/// A player ped's `CPlayerInfo`, its targeting (`CPlayerPedTargeting`), the
/// target that is locked on to.
const PED_PLAYER_INFO: u64 = 4328;
const PLAYER_TARGETING: u64 = 1024;
const TARGETING_LOCK_ON: u64 = 48;
static AIM_LOCKED: AtomicBool = AtomicBool::new(false);

/// The player's aim is locked on to a target (the assisted targeting modes,
/// aiming): the camera follows it, the right stick switches targets.
pub fn aim_locked() -> bool {
    AIM_LOCKED.load(Ordering::Relaxed)
}

/// Whether the player's aim is locked on now.
fn update_aim_locked(env: *mut Env, mem: Mem) {
    let ped = unsafe { orig::find_player_ped(env) };
    let info = if ped != 0 { mem.u64(ped + PED_PLAYER_INFO) } else { 0 };
    let locked = info != 0 && mem.u64(info + PLAYER_TARGETING + TARGETING_LOCK_ON) != 0;
    AIM_LOCKED.store(locked, Ordering::Relaxed);
}

/// Puts touch play's targeting mode in place of the menu's while it plays,
/// the menu's back otherwise (the pause menu shows and saves it).
fn update_targeting(mem: Mem, touch_play: bool) {
    let touch = TOUCH_TARGETING.load(Ordering::Relaxed);
    let mut menu = MENU_TARGETING.lock().unwrap();
    if touch_play && touch != NO_TARGETING {
        menu.get_or_insert_with(|| mem.i32(PREF_TARGETING));
        mem.set_i32(PREF_TARGETING, touch as i32);
    } else if let Some(mode) = menu.take() {
        mem.set_i32(PREF_TARGETING, mode);
    }
}

/// What the player is doing, for the touch controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerContext {
    pub activity: Activity,
    /// On foot, next to a vehicle they can get into.
    pub vehicle_near: bool,
    /// The phone is out.
    pub phone: bool,
    /// The pause menu (the map, the store, ...) is open.
    pub menu: bool,
    /// A cutscene plays.
    pub cutscene: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    OnFoot,
    Driving(Driving),
    /// In a vehicle, not driving it.
    Passenger,
    /// Skydiving or under a parachute.
    Parachute,
}

/// The kinds of vehicles driven alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driving {
    /// Cars, motorbikes, quads, boats: steered, a throttle and a brake.
    Car,
    /// Pedalled.
    Bicycle,
    /// Helicopters, blimps, autogyros: up and down, yaw.
    Heli,
    /// A throttle, yaw, landing gear.
    Plane,
    /// Up and down, a throttle.
    Submarine,
}

pub fn player_context() -> PlayerContext {
    let mut signals = SIGNALS.lock().unwrap();
    let fresh = |t: Option<Instant>| t.is_some_and(|t| t.elapsed() < FRESH);
    // The most particular of what runs now.
    let now = match signals.driving {
        Some((driving, t)) if fresh(Some(t)) => Some(Activity::Driving(driving)),
        _ if fresh(signals.parachute) => Some(Activity::Parachute),
        _ if fresh(signals.in_vehicle) => Some(Activity::Passenger),
        _ if fresh(signals.on_foot) => Some(Activity::OnFoot),
        _ => None,
    };
    if let Some(activity) = now {
        signals.activity = activity;
    }
    PlayerContext {
        activity: signals.activity,
        vehicle_near: signals.activity == Activity::OnFoot && VEHICLE_NEAR.load(Ordering::Relaxed),
        phone: PHONE_OUT.load(Ordering::Relaxed),
        menu: PAUSE_MENU.load(Ordering::Relaxed),
        cutscene: fresh(signals.cutscene),
    }
}

/// Hook for `CControl::Update`: during touch play out of the pause menu, the
/// engine goes on seeing the pad as the device in use, though the camera
/// reads the mouse.
pub(crate) unsafe extern "C" fn control_update(env: *mut Env, this: u64) {
    unsafe { orig::control_update(env, this) };
    // Every part of it (the store, Social Club, the video editor).
    let menu = unsafe { orig::pause_menu_is_active(env, 0) } != 0;
    PAUSE_MENU.store(menu, Ordering::Relaxed);
    let touch_play = TOUCH_PLAY.load(Ordering::Relaxed) && !menu;
    let mem = unsafe { Mem::of(env) };
    if touch_play {
        mem.set_i32(LAST_DEVICE, 0);
    }
    update_targeting(mem, touch_play);
    update_aim_locked(env, mem);
}

/// Hook for `CTaskPlayerOnFoot::ProcessPreFSM`: the player is on foot.
pub(crate) unsafe extern "C" fn player_on_foot(env: *mut Env, task: u64) -> u32 {
    let result = unsafe { orig::player_on_foot(env, task) };
    SIGNALS.lock().unwrap().on_foot = Some(Instant::now());
    if TOUCH_PLAY.load(Ordering::Relaxed) && SCAN_FRAME.fetch_add(1, Ordering::Relaxed).is_multiple_of(SCAN_EVERY) {
        let ped = unsafe { Mem::of(env) }.u64(task + TASK_PED);
        if ped != 0 {
            // As the Enter button would (CTaskPlayerOnFoot::CheckShouldEnterVehicle).
            let vehicle = unsafe { orig::scan_for_vehicle_to_enter(env, ped, ped + PED_POSITION, 0) };
            VEHICLE_NEAR.store(vehicle != 0, Ordering::Relaxed);
        }
    }
    result
}

/// Hook for `CTaskPlayerDrive::ProcessPreFSM`: the player is in a vehicle.
pub(crate) unsafe extern "C" fn player_in_vehicle(env: *mut Env, task: u64) -> u32 {
    let result = unsafe { orig::player_in_vehicle(env, task) };
    SIGNALS.lock().unwrap().in_vehicle = Some(Instant::now());
    result
}

/// Hook for `CTaskParachute::UpdateFSM`, any ped's: the player's skydives
/// or is under a parachute.
pub(crate) unsafe extern "C" fn parachute_update(env: *mut Env, task: u64, state: u32, event: u32) -> u32 {
    let result = unsafe { orig::parachute_update(env, task, state, event) };
    let ped = unsafe { Mem::of(env) }.u64(task + TASK_PED);
    if ped != 0 && ped == unsafe { orig::find_player_ped(env) } {
        SIGNALS.lock().unwrap().parachute = Some(Instant::now());
    }
    result
}

/// Hook for `CutSceneManager::UpdateSkip`: a cutscene plays.
pub(crate) unsafe extern "C" fn cutscene_update_skip(env: *mut Env, this: u64) {
    unsafe { orig::cutscene_update_skip(env, this) };
    SIGNALS.lock().unwrap().cutscene = Some(Instant::now());
}

/// Hooks for the `CREATE_MOBILE_PHONE` and `DESTROY_MOBILE_PHONE` natives.
pub(crate) unsafe extern "C" fn phone_create(env: *mut Env, info: u64) {
    unsafe { orig::phone_create(env, info) };
    PHONE_OUT.store(true, Ordering::Relaxed);
}

pub(crate) unsafe extern "C" fn phone_destroy(env: *mut Env, info: u64) {
    unsafe { orig::phone_destroy(env, info) };
    PHONE_OUT.store(false, Ordering::Relaxed);
}

/// The player drives `vehicle`, a vehicle driven as `kind`.
fn driving(env: *mut Env, vehicle: u64, kind: Driving) {
    let bicycle = vehicle != 0 && unsafe { Mem::of(env) }.i32(vehicle + VEHICLE_TYPE) == BICYCLE;
    SIGNALS.lock().unwrap().driving = Some((if bicycle { Driving::Bicycle } else { kind }, Instant::now()));
}

/// Hooks for the player's driving tasks (`ProcessDriverInputsForPlayerOnUpdate`,
/// once a frame while the player drives).
macro_rules! drive_hooks {
    ($($name:ident: $kind:ident),* $(,)?) => {$(
        pub(crate) unsafe extern "C" fn $name(env: *mut Env, task: u64, ped: u64, control: u64, vehicle: u64) {
            unsafe { orig::$name(env, task, ped, control, vehicle) };
            driving(env, vehicle, Driving::$kind);
        }
    )*};
}

drive_hooks! {
    drive_automobile: Car,
    drive_bike: Car,
    drive_boat: Car,
    drive_heli: Heli,
    drive_autogyro: Heli,
    drive_plane: Plane,
    drive_submarine: Submarine,
}

// ---- keyboard & mouse --------------------------------------------------------

// The input block.
const KEYS: u64 = 0; // u8[256] by Windows virtual-key code, 0x80 = down
const MOUSE_X: u64 = 256;
const MOUSE_Y: u64 = 260;
const MOUSE_DX: u64 = 264; // accumulated; the engine swaps them with 0
const MOUSE_DY: u64 = 268;
const MOUSE_DZ: u64 = 272; // wheel notches, + = up
const MOUSE_BUTTONS: u64 = 276;
const FOCUS: u64 = 280;
const LOCKED: u64 = 284;
const TEXT_HEAD: u64 = 288;
const TEXT_TAIL: u64 = 292;
const TEXT: u64 = 296; // u16[64] ring of UTF-16 units
const TEXT_LEN: u32 = 64;
const WIN_W: u64 = 432;
const WIN_H: u64 = 436;

/// `CMousePointer`'s "shown last frame" in this build: what its `Update`
/// decided (the pause menu or another screen with a cursor, the window in
/// front), kept from one update to the next.
const CURSOR_SHOWN: u64 = 19505961;

/// Mouse buttons in the input block.
pub mod mouse {
    pub const LEFT: u32 = 1;
    pub const RIGHT: u32 = 2;
    pub const MIDDLE: u32 = 4;
    pub const X1: u32 = 8;
    pub const X2: u32 = 16;
}

/// Writes keyboard and mouse input into the engine's input block.
#[derive(Clone, Copy)]
pub struct InputBlock {
    mem: Mem,
    at: u64,
}

impl InputBlock {
    pub(crate) fn new(mem: Mem, at: u64) -> Self {
        InputBlock { mem, at }
    }

    /// A key by Windows virtual-key code.
    pub fn key(&self, vk: u8, down: bool) {
        self.mem.set_u8(self.at + KEYS + vk as u64, if down { 0x80 } else { 0 });
    }

    /// Releases every key and button (e.g. on focus loss).
    pub fn release_all(&self) {
        self.mem.bytes_mut(self.at + KEYS, 256).fill(0);
        self.mem.atomic_u32(self.at + MOUSE_BUTTONS).store(0, Ordering::Relaxed);
    }

    /// Absolute position in back-buffer pixels.
    pub fn mouse_position(&self, x: i32, y: i32) {
        self.mem.set_i32(self.at + MOUSE_X, x);
        self.mem.set_i32(self.at + MOUSE_Y, y);
    }

    /// Relative motion (accumulates until the engine reads it).
    pub fn mouse_motion(&self, dx: i32, dy: i32) {
        self.mem.atomic_u32(self.at + MOUSE_DX).fetch_add(dx as u32, Ordering::AcqRel);
        self.mem.atomic_u32(self.at + MOUSE_DY).fetch_add(dy as u32, Ordering::AcqRel);
    }

    pub fn mouse_wheel(&self, notches: i32) {
        self.mem.atomic_u32(self.at + MOUSE_DZ).fetch_add(notches as u32, Ordering::AcqRel);
    }

    pub fn mouse_button(&self, button: u32, down: bool) {
        let word = self.mem.atomic_u32(self.at + MOUSE_BUTTONS);
        if down {
            word.fetch_or(button, Ordering::AcqRel);
        } else {
            word.fetch_and(!button, Ordering::AcqRel);
        }
    }

    pub fn focus(&self, focused: bool) {
        if !focused {
            self.release_all();
        }
        self.mem.set_u32(self.at + FOCUS, focused as u32);
    }

    pub fn pointer_locked(&self, locked: bool) {
        self.mem.set_u32(self.at + LOCKED, locked as u32);
    }

    /// Whether the game shows its own mouse cursor (the pause menu, the
    /// map, the phone's browser, ...: wherever it is clicked on). The game
    /// draws it itself; on PC it then frees the pointer.
    pub fn cursor_shown(&self) -> bool {
        self.mem.u8(CURSOR_SHOWN) & 1 != 0
    }

    /// Typed text (Enter = 13, Backspace = 8 also go through here).
    pub fn text(&self, s: &str) {
        let head = self.mem.atomic_u32(self.at + TEXT_HEAD);
        let tail = self.mem.atomic_u32(self.at + TEXT_TAIL);
        for unit in s.encode_utf16() {
            let h = head.load(Ordering::Acquire);
            if h.wrapping_sub(tail.load(Ordering::Acquire)) >= TEXT_LEN {
                break; // full
            }
            self.mem.set_u16(self.at + TEXT + (h % TEXT_LEN) as u64 * 2, unit);
            head.store(h.wrapping_add(1), Ordering::Release);
        }
    }

    /// Asks the engine to resize its back buffer.
    pub fn resize(&self, width: u32, height: u32) {
        self.mem.set_u32(self.at + WIN_W, width);
        self.mem.set_u32(self.at + WIN_H, height);
    }
}

/// This module's hooks.
pub(crate) fn hooks(hooks: &mut glue5_game::Hooks) {
    hooks.pad_update = Some(pad_update);
    hooks.radio_wheel_axes = Some(radio_wheel_axes);
    hooks.scaleform_begin_method = Some(scaleform_begin_method);
    hooks.scaleform_add_param_float = Some(scaleform_add_param_float);
    hooks.text_box_open = Some(text_box_open_hook);
    hooks.text_box_close = Some(text_box_close_hook);
    hooks.control_update = Some(control_update);
    hooks.player_on_foot = Some(player_on_foot);
    hooks.phone_create = Some(phone_create);
    hooks.phone_destroy = Some(phone_destroy);
    hooks.drive_automobile = Some(drive_automobile);
    hooks.drive_bike = Some(drive_bike);
    hooks.drive_boat = Some(drive_boat);
    hooks.drive_heli = Some(drive_heli);
    hooks.drive_autogyro = Some(drive_autogyro);
    hooks.drive_plane = Some(drive_plane);
    hooks.drive_submarine = Some(drive_submarine);
    hooks.player_in_vehicle = Some(player_in_vehicle);
    hooks.parachute_update = Some(parachute_update);
    hooks.cutscene_update_skip = Some(cutscene_update_skip);
}

//! The engine's Bink movies play off the host: its Bink SDK is stripped from
//! the wasm, so the host decodes the file with FFmpeg (see `bink_shim.c`)
//! and feeds the pixels into the game's own movie path — the same `bwMovie`
//! state, textures and shader binding it was built for, minus the decoder.
//!
//! `SetMovieSync` (a stub in the wasm, runs on the engine's async queue thread)
//! is replaced wholesale: it opens the movie, builds the fake `BINK` and
//! `BINKFRAMEBUFFERS` in the engine's heap and does the engine's texture
//! dance. `BeginDraw` (render thread) is wrapped: the host decodes and uploads
//! the next frame at the movie's rate, then the original binds the textures
//! and draws with its inlined `Update`/effect logic skipped behind a playing
//! flag. `Play`/`Stop` (the async queue's own handlers store the fields) stay
//! original, with the host keeping the decoder state in step.

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use glue5_game::{Env, orig};

use crate::layout;
use crate::rt;
use crate::memory::Mem;

type CInt = i32;

// FFI to the FFmpeg shim (`bink_shim.c`).
#[repr(C)]
struct BinkPlane {
    data: *const u8,
    pitch: CInt,
}

unsafe extern "C" {
    fn bink_open_movie(path: *const c_char, out: *mut *mut c_void) -> CInt;
    fn bink_last_error() -> *const c_char;
    // out: i32[6] = w, h, fps_num, fps_den, nb_frames, alpha
    fn bink_info(m: *const c_void, out: *mut CInt);
    fn bink_decode_frame(m: *mut c_void, planes: *mut BinkPlane) -> CInt;
    fn bink_close_movie(m: *mut c_void);
    fn bink_probe(path: *const c_char, count: CInt, out: *mut c_char, outsz: CInt) -> CInt;
}

/// A live movie: the engine object (`this`) and its FFmpeg decoder.
struct Player {
    this: u64,
    movie: *mut c_void,
    bink: u64,
    fbs: u64,
    w: u32,
    h: u32,
    cw: u32,
    ch: u32,
    alpha: bool,
    frames: u32,
    period: Duration,
    next_at: Option<Instant>,
    done: bool,
    decoded: u32,
    draws: u32,
}

unsafe impl Send for Player {}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { bink_close_movie(self.movie) };
    }
}

fn players() -> &'static Mutex<Vec<Player>> {
    static PLAYERS: OnceLock<Mutex<Vec<Player>>> = OnceLock::new();
    PLAYERS.get_or_init(|| Mutex::new(Vec::new()))
}

// bwMovie fields (wasm64 structure):
//   +0 m_hBink, +8 m_pFrameBuffers, then three atArrays: m_TexturePointers
//   (+16 data, +24/+26 u16 count/cap), m_RawTextureMemPointers (+32, +40/+42)
//   and m_TextureLocks (+48, +56/+58, 40-byte entries).
const H_BINK: u64 = 0;
const H_FBS: u64 = 8;
const TEXTURES: u64 = 16;
const TEXTURES_COUNT: u64 = 24;
const TEXTURES_CAP: u64 = 26;
const RAW_MEM: u64 = 32;
const RAW_MEM_COUNT: u64 = 40;
const RAW_MEM_CAP: u64 = 42;
const LOCKS: u64 = 48;
const LOCKS_COUNT: u64 = 56;
const LOCKS_CAP: u64 = 58;
const M_LOADED: u64 = 171;
const M_PLAYING: u64 = 172;
const M_HAS_AUDIO: u64 = 175;
const M_ERR_LOGGED: u64 = 176;
const M_PLAY_RATE: u64 = 212;
const M_FPS: u64 = 216;
const M_FPS_DIV: u64 = 220;
const M_CTRL_RATE: u64 = 240;

// bwMovieParams: pFileName[80] at +40.
const P_FILENAME: u64 = 40;
const P_FILENAME_SIZE: u64 = 80;

// BINK (1024 bytes were allocated for it and its deep internals): Width +0,
// Height +4, Frames +8, FrameNum +12, LastFrameNum +16, FrameRate +20,
// FrameRateDiv +24, soundon +64, videoon +68, needio +72, closing +76, and
// internal state from +764 on (the engine's decode driver, all zero).
const B_W: u64 = 0;
const B_H: u64 = 4;
const B_FRAMES: u64 = 8;
const B_FRAME_NUM: u64 = 12;
const B_LAST_FRAME_NUM: u64 = 16;
const B_FRAME_RATE: u64 = 20;
const B_FRAME_RATE_DIV: u64 = 24;
const B_NEEDIO: u64 = 72;
const B_CLOSING: u64 = 76;
const BINK_SIZE: u64 = 1024;

// BINKFRAMEBUFFERS (256 bytes): TotalFrames +0, YA sizes +4/+8, cRcB sizes
// +12/+16, FrameNum +20, Frames[2] from +24 (96-byte entries, 24-byte planes;
// per plane Allocate +0, Buffer +8, BufferPitch +16; Y/cR/cB/A at offsets
// 0/24/48/72 in each frame).
const FBS_SIZE: u64 = 256;
const FB_TOTAL: u64 = 0;
const FB_YA_W: u64 = 4;
const FB_YA_H: u64 = 8;
const FB_CRCB_W: u64 = 12;
const FB_CRCB_H: u64 = 16;
const FB_FRAME_NUM: u64 = 20;
const FB_FRAMES: u64 = 24;
const FB_STRIDE: u64 = 96;
const FB_PLANES: u64 = 4;

// grcTextureLock (40 bytes): MipLevel +0, Base +8, Pitch +16, Layer +36.
const LOCK_SIZE: u64 = 40;
const LOCK_BASE: u64 = 8;
const LOCK_PITCH: u64 = 16;

// The grcTextureFactory singleton pointer (static data).
const FACTORY: u64 = 12372136;
// grcTextureFactory::TextureCreateParams (44 bytes): Memory +0 (grcsVideo),
// Format +4 (linear), LockFlags +8 (write|discard(+read)), Buffer* +16, Type
// +24, MSAA_Type +28, StereoRTMode +32, MipLevels +36, ThreadUseHint +40.
const TCP_SIZE: u64 = 64;
const TCP_MEMORY: u64 = 0;
const TCP_FORMAT: u64 = 4;
const TCP_LOCK_FLAGS: u64 = 8;
const TCP_TYPE: u64 = 24;
const TCP_MSAA: u64 = 28;
const TCP_STEREO: u64 = 32;
const TCP_MIPS: u64 = 36;
const TCP_HINT: u64 = 40;
const X5680: u32 = 13; // grctfL8

/// Resolves the engine's movie name (`platform:/movies/<name>`, the engine
/// strips the extension) to the movie file under the game's data directory.
fn resolve_name(name: &str) -> Option<PathBuf> {
    let rest = name.strip_prefix("platform:").unwrap_or(name);
    let rest = rest.trim_start_matches('/');
    let with_ext = if rest.rfind('.').is_some() {
        rest.to_string()
    } else {
        format!("{rest}.bik")
    };
    let stem = Path::new(rest).file_name()?;
    let stem_ext = format!("{}.bik", stem.to_string_lossy());
    let data = layout::data_dir(&rt().config.game_dir);
    let candidates = [
        data.join("x64").join(&with_ext),
        data.join(&with_ext),
        data.join("x64").join("dlcMovies").join(&stem_ext),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Reads the engine object's name (`pFileName[80]` at `params + 40`).
unsafe fn movie_name(env: *mut Env, params: u64) -> String {
    let mem = Mem::of(env);
    let bytes = mem.bytes(params + P_FILENAME, P_FILENAME_SIZE as usize);
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Plans a plane: its texture size and what to fill it with. Bink planes are
/// Y, cR, cB and A, all 8-bit, chroma at half resolution.
struct PlanePlan {
    w: u32,
    h: u32,
    fill: u8,
}

fn plane_plan(p: u64, w: u32, h: u32) -> PlanePlan {
    match p {
        1 | 2 => PlanePlan { w: (w + 1) / 2, h: (h + 1) / 2, fill: 0x80 },
        _ => PlanePlan { w, h, fill: 0 },
    }
}

/// The `atArray` fields the engine reads; a fresh array is empty, so this
/// grows into one from an easy `operator new` (leaked on the way out, like
/// the engine's own arrays).
unsafe fn array(env: *mut Env, this: u64, data: u64, count: u64, cap: u64, n: u32, elem: u64) -> u64 {
    let mem = Mem::of(env);
    let has = (mem.u16(this + cap) as u64) >= n as u64;
    let at = if has {
        mem.u64(this + data)
    } else {
        let at = orig::op_new_align(env, n as u64 * elem, 16);
        if at != 0 {
            mem.set_u16(this + cap, n as u16);
            mem.set_u64(this + data, at);
        }
        at
    };
    mem.set_u16(this + count, n as u16);
    at
}

/// Locks a texture briefly (the engine's lock context is held for the whole
/// plane loop) and points the frame buffers' plane at its staging memory.
unsafe fn lock_plane(
    env: *mut Env,
    mem: Mem,
    tex: u64,
    lock_addr: u64,
    fbs_plane: u64,
) -> Option<(u64, u64)> {
    let bytes = mem.bytes_mut(lock_addr, LOCK_SIZE as usize);
    bytes.fill(0);
    if orig::lock_rect(env, tex, 0, 0, lock_addr, 3) == 0 {
        return None;
    }
    let base = mem.u64(lock_addr + LOCK_BASE);
    let pitch = mem.i32(lock_addr + LOCK_PITCH);
    if pitch <= 0 {
        orig::unlock_rect(env, tex, lock_addr);
        return None;
    }
    mem.set_u64(fbs_plane + 8, base);
    mem.set_u32(fbs_plane + 16, pitch as u32);
    Some((base, pitch as u64))
}

/// Copies rows of one plane into its texture's staging memory.
unsafe fn copy_plane(mem: Mem, plan: &PlanePlan, base: u64, pitch: u64, src: &BinkPlane) {
    if src.data.is_null() {
        return;
    }
    for r in 0..plan.h as usize {
        let dst = mem.bytes_mut(base + r as u64 * pitch, plan.w as usize);
        // TEMP DIAGNOSTIC: animated diagonal stripes tied to the decode count.
        let phase = (r as u32).wrapping_add(PATTERN_PHASE.load(Ordering::Relaxed));
        for (c, b) in dst.iter_mut().enumerate() {
            let _ = c;
            *b = 0xFF; // TEMP: flat white
        }
    }
}

static PATTERN_PHASE: AtomicU32 = AtomicU32::new(0);

unsafe fn zero_plane(mem: Mem, plan: &PlanePlan, base: u64, pitch: u64, fill: u8) {
    for r in 0..plan.h {
        let dst = mem.bytes_mut(base + r as u64 * pitch, plan.w as usize);
        dst.fill(fill);
    }
}

/// Hook for `bwMovie::SetMovieSync(bwMovieParams&)`: the whole body. Returns
/// whether the movie is ready.
unsafe extern "C" fn set_movie_sync(env: *mut Env, this: u64, params: u64) -> u32 {
    unsafe {
        let mem = Mem::of(env);
        let name = movie_name(env, params);
        let Some(path) = resolve_name(&name) else {
            log::warn!("bink: no movie file for {name}");
            return 0;
        };
        let Ok(c_path) = CString::new(path.to_string_lossy().as_bytes().to_vec()) else {
            log::warn!("bink: bad path for {name}");
            return 0;
        };
        let mut movie: *mut c_void = std::ptr::null_mut();
        if unsafe { bink_open_movie(c_path.as_ptr(), &mut movie) } < 0 {
            let err = CStr::from_ptr(bink_last_error());
            log::warn!("bink: {} failed to open: {:?} ({:#x})", name, err.to_string_lossy(), this);
            return 0;
        }
        let mut info = [0 as CInt; 6];
        bink_info(movie, info.as_mut_ptr());
        let w = info[0] as u32;
        let h = info[1] as u32;
        let fps_num = info[2] as u32;
        let fps_den = info[3] as u32;
        let frames = (info[4] as u32 as u64).clamp(1, 1 << 30) as u32;
        let alpha = info[5] != 0;
        let period = if fps_num > 0 {
            Duration::from_secs_f64(fps_den as f64 / fps_num as f64)
        } else {
            Duration::from_millis(33)
        };
        log::info!("bink: {name} -> {} ({}x{} @{}/{} frames, alpha {})", path.display(), w, h, fps_num, fps_den, alpha);
        // TEMP DIAGNOSTIC: dump the decoder's own output (Y averages).
        let mut report = [0 as c_char; 2048];
        let probed = unsafe {
            bink_probe(c_path.as_ptr(), 250, report.as_mut_ptr(), (report.len() as CInt).max(0))
        };
        let text = unsafe { CStr::from_ptr(report.as_ptr()) };
        log::info!("[dbg] probe({probed}): {:?}", text.to_string_lossy());


        // Replace any old player for this object (the destructor closes the
        // FFmpeg decoder).
        let mut players = players().lock().unwrap();
        players.retain(|p| p.this != this);

        // The fake BINK and BINKFRAMEBUFFERS live in the engine's own heap, so
        // the engine's lifetime management (and its other code paths that read
        // them) work unchanged.
        let bink = orig::op_new(env, BINK_SIZE);
        let fbs = orig::op_new(env, FBS_SIZE);
        let tcp = orig::op_new(env, TCP_SIZE);
        if bink == 0 || fbs == 0 || tcp == 0 {
            log::warn!("bink: out of engine memory for {name}");
            drop(Player { this, movie, bink, fbs, w, h, cw: 0, ch: 0, alpha, frames, period, next_at: None, done: true, decoded: 0, draws: 0 });
            return 0;
        }
        mem.bytes_mut(bink, BINK_SIZE as usize).fill(0);
        mem.set_u32(bink + B_W, w);
        mem.set_u32(bink + B_H, h);
        mem.set_u32(bink + B_FRAMES, frames);
        mem.set_u32(bink + B_FRAME_NUM, 0);
        mem.set_u32(bink + B_LAST_FRAME_NUM, 0);
        mem.set_u32(bink + B_FRAME_RATE, fps_num.max(1));
        mem.set_u32(bink + B_FRAME_RATE_DIV, fps_den.max(1));
        mem.set_u32(bink + B_NEEDIO, 1);
        mem.set_u32(bink + B_CLOSING, 0);
        mem.bytes_mut(fbs, FBS_SIZE as usize).fill(0);
        mem.set_u32(fbs + FB_TOTAL, 2);
        mem.set_u32(fbs + FB_YA_W, w);
        mem.set_u32(fbs + FB_YA_H, h);
        mem.set_u32(fbs + FB_CRCB_W, (w + 1) / 2);
        mem.set_u32(fbs + FB_CRCB_H, (h + 1) / 2);
        for f in 0..2 {
            let fb = fbs + FB_FRAMES + f * FB_STRIDE;
            for p in 0..FB_PLANES {
                mem.set_u32(fb + p * 24, 1); // Allocate
                // Buffer/BufferPitch are filled by the lock below.
            }
        }
        // The texture factory's parameters blob: one flat 8-bit-luma target
        // per plane, writable, no mips.
        mem.set_u32(tcp + TCP_MEMORY, 1);
        mem.set_u32(tcp + TCP_FORMAT, 0);
        mem.set_u32(tcp + TCP_LOCK_FLAGS, 11);
        mem.set_u32(tcp + TCP_TYPE, 0);
        mem.set_u32(tcp + TCP_MSAA, 0);
        mem.set_u32(tcp + TCP_STEREO, 2);
        mem.set_u32(tcp + TCP_MIPS, 1);
        mem.set_u32(tcp + TCP_HINT, 0);

        // The three pointer arrays (textures, their backing memory and
        // locks); TotalFrames x 4 planes of textures.
        let n = 8;
        let textures = array(env, this, TEXTURES, TEXTURES_COUNT, TEXTURES_CAP, n, 8);
        let raw_mems = array(env, this, RAW_MEM, RAW_MEM_COUNT, RAW_MEM_CAP, n, 8);
        let locks = array(env, this, LOCKS, LOCKS_COUNT, LOCKS_CAP, n, LOCK_SIZE);
        if textures == 0 || raw_mems == 0 || locks == 0 {
            log::warn!("bink: out of engine memory for the texture arrays ({name})");
            drop(Player { this, movie, bink, fbs, w, h, cw: 0, ch: 0, alpha, frames, period, next_at: None, done: true, decoded: 0, draws: 0 });
            return 0;
        }

        // The engine's texture dance, per plane: engine-allocated memory the
        // GPU reads its staging data from, then a texture pointing at it,
        // then a lock to fill the planes' initial content.
        let factory = mem.u64(FACTORY);
        orig::lock_context(env);
        let mut ok = true;
        for i in 0..n {
            let f = i as u64 / 4;
            let p = i as u64 % 4;
            let plan = plane_plan(p, w, h);
            let size = plan.w as u64 * plan.h as u64;
            let raw = orig::op_new_align(env, size, 4096);
            if raw == 0 {
                log::warn!("bink: out of engine memory for plane {p} of {name}");
                ok = false;
                break;
            }
            let tex = orig::factory_create(env, factory, plan.w, plan.h, X5680, raw, 1, tcp);
            if tex == 0 {
                log::warn!("bink: texture {} for {name} failed", i);
                ok = false;
                break;
            }
            mem.set_u64(textures + i as u64 * 8, tex);
            mem.set_u64(raw_mems + i as u64 * 8, raw);
            let lock_addr = locks + i as u64 * LOCK_SIZE;
            let _ = match lock_plane(env, mem, tex, lock_addr, fbs + FB_FRAMES + f * FB_STRIDE + p * 24) {
                Some((base, pitch)) => {
                    zero_plane(mem, &plan, base, pitch, plan.fill);
                    orig::unlock_rect(env, tex, lock_addr);
                    lock_addr
                }
                None => {
                    log::warn!("bink: plane {p} lock for {name} failed");
                    ok = false;
                    break;
                }
            };
        }
        orig::unlock_context(env);
        if !ok {
            drop(Player { this, movie, bink, fbs, w, h, cw: 0, ch: 0, alpha, frames, period, next_at: None, done: true, decoded: 0, draws: 0 });
            return 0;
        }

        // The bwMovie's own fields: loaded, the engine's bookkeeping (playing
        // is set by the async queue's handler; the host drives the timing
        // itself), and the fields the wasm's BeginDraw reads for its effect
        // logic.
        mem.set_u64(this + H_BINK, bink);
        mem.set_u64(this + H_FBS, fbs);
        mem.set_u8(this + M_LOADED, 1);
        mem.set_u8(this + M_HAS_AUDIO, 0);
        mem.set_u8(this + 241, 0);
        mem.set_u8(this + M_ERR_LOGGED, 1);
        mem.set_u32(this + 168, 1);
        mem.set_u32(this + M_PLAY_RATE, (1.0f32).to_bits());
        mem.set_u32(this + M_FPS, fps_num.max(1));
        mem.set_u32(this + M_FPS_DIV, fps_den.max(1));
        mem.set_u8(this + M_CTRL_RATE, 0);
        for at in [224, 228, 232] {
            mem.set_u32(this + at, 0);
        }
        players.push(Player { this, movie, bink, fbs, w, h, cw: (w + 1) / 2, ch: (h + 1) / 2, alpha, frames, period, next_at: None, done: false, decoded: 0, draws: 0 });
        1
    }
}

/// Decodes and uploads the next frame's planes into the engine's textures,
/// then makes those textures current (`FrameNum` in both the frame buffers
/// and the fake BINK).
unsafe fn step(env: *mut Env, mem: Mem, p: &mut Player) {
    if p.done {
        return;
    }
    let mut planes: [BinkPlane; 4] = [
        BinkPlane { data: std::ptr::null(), pitch: 0 },
        BinkPlane { data: std::ptr::null(), pitch: 0 },
        BinkPlane { data: std::ptr::null(), pitch: 0 },
        BinkPlane { data: std::ptr::null(), pitch: 0 },
    ];
    let got = bink_decode_frame(p.movie, planes.as_mut_ptr());
    if got <= 0 {
        // The movie's end (or an error): the textures keep the last frame and
        // the engine sees a finished FrameNum.
        p.done = true;
        log::info!("bink: movie {:#x} finished after {} frames ({:#x})", p.this, p.decoded, p.movie as u64);
        mem.set_u32(p.bink + B_FRAME_NUM, p.frames);
        mem.set_u32(p.bink + B_LAST_FRAME_NUM, mem.u32(p.fbs + FB_FRAME_NUM));
        return;
    }
    p.decoded += 1;
    PATTERN_PHASE.store(p.decoded.wrapping_mul(9), Ordering::Relaxed);
    let cur = mem.u32(p.fbs + FB_FRAME_NUM);
    let next = (cur + 1) % 2;
    let base_idx = next as u64 * FB_PLANES;
    let textures = mem.u64(p.this + TEXTURES);
    let locks = mem.u64(p.this + LOCKS);
    orig::lock_context(env);
    for j in 0..FB_PLANES {
        let plan = plane_plan(j as u64, p.w, p.h);
        if j == 3 && !p.alpha {
            continue;
        }
        // TEMP DIAGNOSTIC: copy into BOTH frame sets (any texture parity works).
        for f in 0..2 {
            let element = f * FB_PLANES + j;
            let tex = mem.u64(textures + element * 8);
            if tex == 0 {
                continue;
            }
            let lock_addr = locks + element * LOCK_SIZE;
            let plane_addr = p.fbs + FB_FRAMES + f as u64 * FB_STRIDE + j * 24;
            if let Some((base, pitch)) = lock_plane(env, mem, tex, lock_addr, plane_addr) {
                copy_plane(mem, &plan, base, pitch, &planes[j as usize]);
                // TEMP DIAGNOSTIC: force the GPU upload from here (game
                // thread), bypassing the unlock path's thread gating.
                if p.decoded < 3 {
                    log::info!("[dbg] update_gpu_copy i{next} f{f} j{j} tex {tex:#x} base {base:#x}");
                }
                orig::update_gpu_copy(env, tex, 0, 0, base);
                orig::unlock_rect(env, tex, lock_addr);
            }
        }
    }
    orig::unlock_context(env);
    mem.set_u32(p.fbs + FB_FRAME_NUM, next);
    mem.set_u32(p.bink + B_FRAME_NUM, next);
    mem.set_u32(p.bink + B_LAST_FRAME_NUM, cur);
}

/// Hook for `bwMovie::BeginDraw(bool)`: the host decodes the frame the engine
/// is about to draw, then the original runs with the playing flag down (its
/// SDK-driven update path does nothing, and its effect block binds these
/// textures).
pub(crate) unsafe extern "C" fn sprite_draw_switch(
    env: *mut Env,
    b0: u32,
    b1: u32,
    v0: u64,
    v1: u64,
    v2: u64,
    v3: u64,
    v4: u64,
    v5: u64,
    v6: u64,
    v7: u64,
    color: u64,
) {
    unsafe { orig::sprite_draw_switch(env, b0, b1, v0, v1, v2, v3, v4, v5, v6, v7, color) };
}

/// Hook for `bwMovie::BeginDraw(bool)`: decodes the next frame, then runs the
/// original, which binds the textures to the movie's shader instance.
unsafe extern "C" fn begin_draw(env: *mut Env, this: u64) -> u32 {
    unsafe {
        let mut players = players().lock().unwrap();
        if let Some(p) = players.iter_mut().find(|p| p.this == this) {
            let mem = Mem::of(env);
            let now = Instant::now();
            let due = p.next_at.map_or(true, |at| now >= at);
            if due {
                step(env, mem, p);
                p.next_at = Some(now + p.period);
            }
            let playing = mem.u8(this + M_PLAYING);
            mem.set_u8(this + M_PLAYING, 0);
            let out = orig::begin_draw(env, this);
            // TEMP DIAGNOSTIC: find the UVScalar (1,1,1,1) in the instance and
            // dump the cbuffer region around it.
            if p.draws == 2 {
                let inst = mem.u64(12842800);
                for off in (0..4096usize).step_by(4) {
                    let v = mem.u32(inst + off as u64);
                    if v == 0x3F800000 {
                        // require four consecutive 1.0s
                        if mem.u32(inst + off as u64 + 4) == 0x3F800000
                            && mem.u32(inst + off as u64 + 8) == 0x3F800000
                            && mem.u32(inst + off as u64 + 12) == 0x3F800000
                        {
                            let start = off.saturating_sub(64);
                            let mut dump = String::new();
                            for i in (start..start + 96).step_by(4) {
                                if i == start + 64 {
                                    dump.push_str(" | ");
                                }
                                let w = mem.u32(inst + i as u64);
                                let f = f32::from_bits(w);
                                dump.push_str(&format!("{}({:.4}) ", w, f));
                            }
                            log::info!("[dbg] uvscalar at inst+{off:#x}: {dump}");
                        }
                    }
                }
            }
            if p.draws < 8 || out == 0 {
                log::info!(
                    "[dbg] begin_draw {:#x} -> {} (draws {}, frame {}, effect {:#x} tOpaque {} tAlpha {} yid {} uvid {} avid {} uvsc {} tex {})",
                    this, out, p.draws, mem.u32(p.fbs + FB_FRAME_NUM),
                    mem.u64(12842800), mem.u32(12842828), mem.u32(12842832),
                    mem.u32(12842808), mem.u32(12842812), mem.u32(12842820), mem.u32(12842824),
                    mem.u64(mem.u64(this + TEXTURES) + mem.u32(p.fbs + FB_FRAME_NUM) as u64 * 32)
                );
            }
            p.draws += 1;
            mem.set_u8(this + M_PLAYING, playing);
            out
        } else {
            orig::begin_draw(env, this)
        }
    }
}

/// Hook for `bwMovie::Play()`: the engine's own handler sets the playing
/// flag from the async queue; the host only resets its throttle.
unsafe extern "C" fn bink_play(env: *mut Env, this: u64) {
    unsafe {
        let mut players = players().lock().unwrap();
        let mem = Mem::of(env);
        if let Some(p) = players.iter_mut().find(|p| p.this == this) {
            p.next_at = None;
            p.done = false;
            mem.set_u32(p.bink + B_FRAME_NUM, 0);
            mem.set_u32(p.fbs + FB_FRAME_NUM, 0);
        }
        drop(players);
        orig::bink_play(env, this);
    }
}

/// Hook for `bwMovie::Stop()`: the engine's own handler clears the playing
/// flag from the async queue; the host drops the decoder state.
unsafe extern "C" fn bink_stop(env: *mut Env, this: u64) {
    unsafe {
        players().lock().unwrap().retain(|p| p.this != this);
        orig::bink_stop(env, this);
    }
}

/// This module's hooks.
pub(crate) fn hooks(hooks: &mut glue5_game::Hooks) {
    if !cfg!(target_os = "android") {
        return;
    }
    hooks.set_movie_sync = Some(set_movie_sync);
    hooks.begin_draw = Some(begin_draw);
    hooks.bink_play = Some(bink_play);
    hooks.bink_stop = Some(bink_stop);
    hooks.sprite_draw_switch = Some(sprite_draw_switch);
}

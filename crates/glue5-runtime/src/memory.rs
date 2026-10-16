//! The module's linear memory: reserved once (so it never moves while other
//! threads run) and accessed by host code through [`Mem`].

use std::ffi::CStr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use anyhow::{Result, bail};
use glue5_game::{Env, MEMORY_INITIAL_PAGES, MEMORY_MAX_PAGES, Memory, PAGE_SIZE, orig};

pub struct WasmMemory {
    pub desc: *mut Memory,
    base: *mut u8,
    max_pages: u64,
}

unsafe impl Send for WasmMemory {}
unsafe impl Sync for WasmMemory {}

impl WasmMemory {
    /// Reserves address space for the largest memory the module may grow to
    /// (or the most the OS grants, but at least the initial size). Pages count
    /// once written; on Windows, which does not overcommit, the memory's
    /// current size is committed too ([`commit`]).
    pub fn reserve() -> Result<Self> {
        let mut max_pages = MEMORY_MAX_PAGES;
        loop {
            let bytes = (max_pages * PAGE_SIZE) as usize;
            if let Some(base) = map(bytes) {
                if !commit(base, 0, MEMORY_INITIAL_PAGES * PAGE_SIZE) {
                    bail!("cannot commit {} MiB for the wasm memory", (MEMORY_INITIAL_PAGES * PAGE_SIZE) >> 20);
                }
                log::info!("wasm memory: {} MiB reserved at {base:p}", bytes >> 20);
                let desc = unsafe { Memory::new(base, MEMORY_INITIAL_PAGES, max_pages) };
                return Ok(WasmMemory { desc, base, max_pages });
            }
            if max_pages / 2 < MEMORY_INITIAL_PAGES {
                bail!("cannot reserve {} MiB for the wasm memory", (MEMORY_INITIAL_PAGES * PAGE_SIZE) >> 20);
            }
            max_pages /= 2;
        }
    }

    pub fn mem(&self) -> Mem {
        Mem { base: self.base }
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_pages * PAGE_SIZE
    }

    pub fn size(&self) -> u64 {
        unsafe { (*self.desc).size }
    }

    /// `emscripten_resize_heap`: memory is already reserved, growing only
    /// publishes the new size (and commits it, on Windows).
    pub fn grow_to(&self, bytes: u64) -> bool {
        if bytes > self.max_bytes() {
            return false;
        }
        let pages = bytes.div_ceil(PAGE_SIZE);
        // Committed before it is published: committing pages that are
        // already is harmless, so racing growers need no lock.
        if !commit(self.base, self.size(), pages * PAGE_SIZE) {
            return false;
        }
        unsafe {
            if pages > (*self.desc).pages {
                (*self.desc).pages = pages;
                (*self.desc).size = pages * PAGE_SIZE;
            }
        }
        true
    }
}

#[cfg(unix)]
fn map(bytes: usize) -> Option<*mut u8> {
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    (ptr != libc::MAP_FAILED).then_some(ptr as *mut u8)
}

/// Makes `[from, to)` of the mapping usable. Unix systems overcommit: the
/// whole mapping is, already.
#[cfg(unix)]
fn commit(_base: *mut u8, _from: u64, _to: u64) -> bool {
    true
}

/// Address space only: committed memory counts against the system's commit
/// limit (RAM and page file), however little of it is touched.
#[cfg(windows)]
fn map(bytes: usize) -> Option<*mut u8> {
    use windows_sys::Win32::System::Memory::{MEM_RESERVE, PAGE_NOACCESS, VirtualAlloc};
    let ptr = unsafe { VirtualAlloc(std::ptr::null(), bytes, MEM_RESERVE, PAGE_NOACCESS) };
    (!ptr.is_null()).then_some(ptr as *mut u8)
}

/// Commits `[from, to)` of the reservation, as the memory grows: its only
/// growth is `emscripten_resize_heap` ([`WasmMemory::grow_to`]). Committed
/// pages read as zeros, as wasm's do.
#[cfg(windows)]
fn commit(base: *mut u8, from: u64, to: u64) -> bool {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, PAGE_READWRITE, VirtualAlloc};
    if to <= from {
        return true;
    }
    let ptr = unsafe { VirtualAlloc(base.add(from as usize).cast(), (to - from) as usize, MEM_COMMIT, PAGE_READWRITE) };
    !ptr.is_null()
}

/// A view of wasm memory for host code. Addresses are wasm (linear memory)
/// addresses. The memory is shared with running wasm threads; plain accessors
/// are unordered, like non-atomic wasm accesses.
#[derive(Clone, Copy)]
pub struct Mem {
    base: *mut u8,
}

unsafe impl Send for Mem {}
unsafe impl Sync for Mem {}

impl Mem {
    /// # Safety
    /// `env` must be a live instance's environment.
    pub unsafe fn of(env: *mut Env) -> Mem {
        Mem { base: unsafe { (*(*env).memory).data } }
    }

    pub fn ptr(self, addr: u64) -> *mut u8 {
        unsafe { self.base.add(addr as usize) }
    }

    pub fn u8(self, addr: u64) -> u8 {
        unsafe { *self.ptr(addr) }
    }
    pub fn u16(self, addr: u64) -> u16 {
        unsafe { self.ptr(addr).cast::<u16>().read_unaligned() }
    }
    pub fn u32(self, addr: u64) -> u32 {
        unsafe { self.ptr(addr).cast::<u32>().read_unaligned() }
    }
    pub fn i32(self, addr: u64) -> i32 {
        self.u32(addr) as i32
    }
    pub fn u64(self, addr: u64) -> u64 {
        unsafe { self.ptr(addr).cast::<u64>().read_unaligned() }
    }
    pub fn f64(self, addr: u64) -> f64 {
        f64::from_bits(self.u64(addr))
    }

    pub fn set_u8(self, addr: u64, v: u8) {
        unsafe { *self.ptr(addr) = v }
    }
    pub fn set_u16(self, addr: u64, v: u16) {
        unsafe { self.ptr(addr).cast::<u16>().write_unaligned(v) }
    }
    pub fn set_u32(self, addr: u64, v: u32) {
        unsafe { self.ptr(addr).cast::<u32>().write_unaligned(v) }
    }
    pub fn set_i32(self, addr: u64, v: i32) {
        self.set_u32(addr, v as u32)
    }
    pub fn set_u64(self, addr: u64, v: u64) {
        unsafe { self.ptr(addr).cast::<u64>().write_unaligned(v) }
    }
    pub fn set_i64(self, addr: u64, v: i64) {
        self.set_u64(addr, v as u64)
    }

    pub fn bytes<'a>(self, addr: u64, len: usize) -> &'a [u8] {
        unsafe { std::slice::from_raw_parts(self.ptr(addr), len) }
    }
    #[allow(clippy::mut_from_ref)]
    pub fn bytes_mut<'a>(self, addr: u64, len: usize) -> &'a mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr(addr), len) }
    }

    /// NUL-terminated string at `addr` (lossy UTF-8).
    pub fn str(self, addr: u64) -> String {
        if addr == 0 {
            return String::new();
        }
        unsafe { CStr::from_ptr(self.ptr(addr) as *const _) }.to_string_lossy().into_owned()
    }

    /// Emscripten's `stringToUTF8`: writes at most `cap - 1` bytes plus a NUL.
    /// Returns the bytes written, without the NUL.
    pub fn write_str(self, addr: u64, s: &str, cap: usize) -> usize {
        if cap == 0 {
            return 0;
        }
        let mut n = s.len().min(cap - 1);
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        self.bytes_mut(addr, n).copy_from_slice(&s.as_bytes()[..n]);
        self.set_u8(addr + n as u64, 0);
        n
    }

    pub fn atomic_u32<'a>(self, addr: u64) -> &'a AtomicU32 {
        unsafe { &*(self.ptr(addr) as *const AtomicU32) }
    }
    pub fn atomic_u64<'a>(self, addr: u64) -> &'a AtomicU64 {
        unsafe { &*(self.ptr(addr) as *const AtomicU64) }
    }

    /// `Atomics.store` + `Atomics.notify` on a 32-bit word.
    pub fn store_notify(self, addr: u64, v: u32) {
        let word = self.atomic_u32(addr);
        word.store(v, Ordering::SeqCst);
        glue5_game::futex::notify(word, u32::MAX);
    }
}

/// The process's memory footprint (what iOS limits) and, on iOS, how much
/// more it may take before the system terminates it, in bytes.
pub fn footprint() -> (Option<u64>, Option<u64>) {
    #[cfg(target_vendor = "apple")]
    let used = unsafe {
        // libproc: in the system on iOS too, though its header is macOS-only.
        unsafe extern "C" {
            fn proc_pid_rusage(pid: libc::c_int, flavor: libc::c_int, buffer: *mut libc::c_void) -> libc::c_int;
        }
        let mut info: libc::rusage_info_v4 = std::mem::zeroed();
        let ok = proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, &mut info as *mut _ as *mut libc::c_void) == 0;
        ok.then_some(info.ri_phys_footprint)
    };
    #[cfg(not(target_vendor = "apple"))]
    let used = None;
    #[cfg(target_os = "ios")]
    let available = {
        unsafe extern "C" {
            fn os_proc_available_memory() -> usize;
        }
        Some(unsafe { os_proc_available_memory() } as u64)
    };
    #[cfg(not(target_os = "ios"))]
    let available = None;
    (used, available)
}

// ---- the engine's resource heap ----------------------------------------------

/// The resource heap's budget, in bytes (0: none).
static RESOURCE_BUDGET: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The streaming engine is asking how much memory is left.
    static ASKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The GPU memory budget, in bytes (0: none): `GLUE5_GPU_BUDGET_MB`, or a
/// phone's default. The renderer keeps its objects under it, and the
/// engine's resource heap is capped at it, so its own eviction holds the
/// GPU's memory down.
pub fn gpu_budget() -> u64 {
    if !crate::rt().config.limit_gpu_memory {
        return 0;
    }
    if let Some(mb) = crate::rt().config.gpu_budget_mb.filter(|&mb| mb > 0) {
        return u64::from(mb) << 20;
    }
    if let Some(mb) = std::env::var("GLUE5_GPU_BUDGET_MB").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&mb| mb > 0) {
        return mb << 20;
    }
    if cfg!(any(target_os = "ios", target_os = "android")) {
        return 850 << 20;
    }
    0
}

/// Budgets the resource heap at `mb` MiB (the engine's `gameResourceMB`).
pub(crate) fn budget_resource_heap(mb: u32) {
    RESOURCE_BUDGET.store((mb as u64) << 20, Ordering::Relaxed);
}

/// Hook for `rage::sysMemGrowBuddyAllocator::GrowHeap()`. The PC build's
/// resource heap starts at `gameResourceMB` and grows by up to 256 MiB, for
/// an allocation and also whenever the streaming engine asks how much memory
/// is left (answering grows it), so it holds whatever the player has passed
/// by: on a phone, past what the system allows. With a budget, answering no
/// longer grows it past the budget, nor counts what lies past it as free:
/// the streaming engine sees it full and evicts, as on consoles. Allocations
/// still grow it, though: the streaming engine does not survive one failing
/// ("read request ... using destination addresses, some of which are NULL",
/// a fatal error). -1: no growth.
pub(crate) unsafe extern "C" fn grow_heap(env: *mut Env, this: u64) -> u32 {
    // Heaps come a little short of whole MiB (255, 511, ...): within the
    // smallest growth step of the budget counts as there.
    const STEP: u64 = 16 << 20;
    let budget = RESOURCE_BUDGET.load(Ordering::Relaxed);
    if budget != 0 && ASKING.get() && unsafe { orig::heap_size(env, this) } + STEP > budget {
        return u32::MAX;
    }
    unsafe { orig::grow_heap(env, this) }
}

/// Hook for `rage::sysMemGrowBuddyAllocator::GetMemoryAvailable()`: past
/// the budget (grown by allocations), what lies beyond it does not count as
/// free, so the streaming engine evicts back under it.
pub(crate) unsafe extern "C" fn memory_available(env: *mut Env, this: u64) -> u64 {
    // The two ask each other: inner calls are answered as they are.
    if ASKING.get() {
        return unsafe { orig::memory_available(env, this) };
    }
    let available = asking(|| unsafe { orig::memory_available(env, this) });
    available.saturating_sub(unsafe { over_budget(env, this) })
}

/// Hook for `rage::sysMemGrowBuddyAllocator::GetLargestAvailableBlock()`
/// (no larger than what [`memory_available`] says is free).
pub(crate) unsafe extern "C" fn largest_block(env: *mut Env, this: u64) -> u64 {
    if ASKING.get() {
        return unsafe { orig::largest_block(env, this) };
    }
    let largest = asking(|| unsafe { orig::largest_block(env, this) });
    match unsafe { over_budget(env, this) } {
        0 => largest,
        over => largest.min(asking(|| unsafe { orig::memory_available(env, this) }).saturating_sub(over)),
    }
}

/// How far the heap has grown past the budget, in bytes.
unsafe fn over_budget(env: *mut Env, this: u64) -> u64 {
    match RESOURCE_BUDGET.load(Ordering::Relaxed) {
        0 => 0,
        budget => unsafe { orig::heap_size(env, this) }.saturating_sub(budget),
    }
}

/// The adapter the engine picked (`grcDevice::InitAdapterOrdinal`'s), the
/// number of adapters, and their descriptions, at these addresses in this
/// build. A description's video memory is at offset 48.
const ADAPTER_ORDINAL: u64 = 6639464;
const ADAPTER_COUNT: u64 = 12484032;
const ADAPTER_TABLE: u64 = 12484024;
const ADAPTER_VRAM: u64 = 48;

/// Hook for `rage::grcResourceCache::UpdateTotalAvailableMemory()`: the
/// engine's resource cache, and the video memory its settings menu shows,
/// follow the adapter's video memory, which the browser build's stand-in
/// reports as 2 GiB whatever the device has. Capped at the GPU's budget,
/// the engine's own eviction and recommendations fit the device: the
/// streaming engine holds the line instead of the renderer's backpressure.
pub(crate) unsafe extern "C" fn update_total_available_memory(env: *mut Env) {
    let mem = unsafe { Mem::of(env) };
    let size = crate::rt().memory.size();
    let within = |addr: u64, len: u64| addr != 0 && addr.checked_add(len).is_some_and(|end| end <= size);
    let cap = gpu_budget();
    let ordinal = u64::from(mem.u32(ADAPTER_ORDINAL));
    if cap != 0 && ordinal < u64::from(mem.u16(ADAPTER_COUNT)) && within(ADAPTER_TABLE, 8) {
        let table = mem.u64(ADAPTER_TABLE);
        let desc = within(table + ordinal * 8, 8).then(|| mem.u64(table + ordinal * 8)).filter(|&d| d != 0);
        if let Some(desc) = desc.filter(|&d| within(d + ADAPTER_VRAM, 8)) {
            let vram = mem.u64(desc + ADAPTER_VRAM);
            if vram > cap {
                mem.set_u64(desc + ADAPTER_VRAM, cap);
                log::info!(
                    "game settings: the adapter's video memory {} MiB capped to the GPU's {} MiB budget",
                    vram >> 20,
                    cap >> 20
                );
            }
        }
    }
    unsafe { orig::update_total_available_memory(env) }
}

fn asking<R>(f: impl FnOnce() -> R) -> R {
    ASKING.set(true);
    let r = f();
    ASKING.set(false);
    r
}

/// This module's hooks.
pub(crate) fn hooks(hooks: &mut glue5_game::Hooks) {
    hooks.grow_heap = Some(grow_heap);
    hooks.memory_available = Some(memory_available);
    hooks.largest_block = Some(largest_block);
    hooks.update_total_available_memory = Some(update_total_available_memory);
}

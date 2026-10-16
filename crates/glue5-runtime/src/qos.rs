//! Thread priorities. On Apple platforms they are quality-of-service classes,
//! which also decide between performance and efficiency cores: the threads a
//! frame waits on are interactive, the engine's others just below. On
//! Android the interactive threads form a performance hint session told how
//! long frames take ([`frame`]), from which the system sets the CPU clocks.

#[derive(Clone, Copy, Debug)]
pub enum Qos {
    /// A frame waits on it (the engine's main thread, the renderer).
    Interactive,
    /// Work the game needs soon (engine workers, shaders being prepared).
    Initiated,
    /// Background work (logs, pipelines compiled while playing).
    Utility,
}

/// Sets the calling thread's class.
pub fn set(qos: Qos) {
    #[cfg(target_vendor = "apple")]
    unsafe {
        use libc::qos_class_t::*;
        let class = match qos {
            Qos::Interactive => QOS_CLASS_USER_INTERACTIVE,
            Qos::Initiated => QOS_CLASS_USER_INITIATED,
            Qos::Utility => QOS_CLASS_UTILITY,
        };
        libc::pthread_set_qos_class_self_np(class, 0);
    }
    #[cfg(target_os = "android")]
    if let Qos::Interactive = qos {
        hint::join();
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "android")))]
    let _ = qos;
}

/// A frame was shown `took` after the one before, for `target`.
pub fn frame(took: std::time::Duration, target: std::time::Duration) {
    #[cfg(target_os = "android")]
    hint::frame(took, target);
    #[cfg(not(target_os = "android"))]
    let _ = (took, target);
}

/// Before the process ends: no more hints. On Android `exit` destroys the
/// system library's own state (the hint manager's mutex) while the
/// renderer's threads still report frames, which then abort the process.
pub fn end() {
    #[cfg(target_os = "android")]
    hint::end();
}

/// Android's performance hints (ADPF, Android 13+; looked up at run time).
/// Without them the system's defaults keep the big cores' clocks low for a
/// game that never fully loads one thread, as the engine's pipeline does.
#[cfg(target_os = "android")]
mod hint {
    use std::ffi::{CStr, c_void};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    struct Api {
        manager: *mut c_void,
        create: unsafe extern "C" fn(*mut c_void, *const i32, usize, i64) -> *mut c_void,
        close: unsafe extern "C" fn(*mut c_void),
        update_target: unsafe extern "C" fn(*mut c_void, i64) -> i32,
        report: unsafe extern "C" fn(*mut c_void, i64) -> i32,
        /// Android 14+; before, the session is made again with the new thread.
        set_threads: Option<unsafe extern "C" fn(*mut c_void, *const i32, usize) -> i32>,
    }
    // SAFETY: the manager is a process-wide singleton, usable from any thread.
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    struct Session {
        threads: Vec<i32>,
        session: *mut c_void,
        target: i64,
        /// Closed for good ([`end`]).
        ended: bool,
    }
    // SAFETY: the session is only used under the mutex.
    unsafe impl Send for Session {}

    static SESSION: Mutex<Session> =
        Mutex::new(Session { threads: Vec::new(), session: std::ptr::null_mut(), target: 0, ended: false });

    fn api() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(|| unsafe {
            let lib = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW);
            if lib.is_null() {
                return None;
            }
            let sym = |name: &CStr| {
                let p = libc::dlsym(lib, name.as_ptr());
                (!p.is_null()).then_some(p)
            };
            let get_manager: unsafe extern "C" fn() -> *mut c_void =
                std::mem::transmute(sym(c"APerformanceHint_getManager")?);
            let manager = get_manager();
            if manager.is_null() {
                return None;
            }
            Some(Api {
                manager,
                create: std::mem::transmute(sym(c"APerformanceHint_createSession")?),
                close: std::mem::transmute(sym(c"APerformanceHint_closeSession")?),
                update_target: std::mem::transmute(sym(c"APerformanceHint_updateTargetWorkDuration")?),
                report: std::mem::transmute(sym(c"APerformanceHint_reportActualWorkDuration")?),
                set_threads: sym(c"APerformanceHint_setThreads").map(|p| std::mem::transmute(p)),
            })
        })
        .as_ref()
    }

    /// Adds the calling thread to the session.
    pub fn join() {
        let Some(api) = api() else { return };
        let mut s = SESSION.lock().unwrap();
        if s.ended {
            return;
        }
        s.threads.push(unsafe { libc::gettid() });
        if s.target == 0 {
            s.target = Duration::from_secs(1).as_nanos() as i64 / 60;
        }
        unsafe {
            if !s.session.is_null()
                && let Some(set_threads) = api.set_threads
            {
                set_threads(s.session, s.threads.as_ptr(), s.threads.len());
                return;
            }
            if !s.session.is_null() {
                (api.close)(s.session);
            }
            s.session = (api.create)(api.manager, s.threads.as_ptr(), s.threads.len(), s.target);
        }
        if s.session.is_null() {
            log::warn!("no performance hint session");
        }
    }

    pub fn frame(took: Duration, target: Duration) {
        let Some(api) = api() else { return };
        let mut s = SESSION.lock().unwrap();
        if s.session.is_null() {
            return;
        }
        let target = target.as_nanos() as i64;
        unsafe {
            if target > 0 && target != s.target {
                s.target = target;
                (api.update_target)(s.session, target);
            }
            (api.report)(s.session, took.as_nanos().max(1) as i64);
        }
    }

    pub fn end() {
        let Some(api) = api() else { return };
        let mut s = SESSION.lock().unwrap();
        s.ended = true;
        if !s.session.is_null() {
            unsafe { (api.close)(s.session) };
            s.session = std::ptr::null_mut();
        }
    }
}

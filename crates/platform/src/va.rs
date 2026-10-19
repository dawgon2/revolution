//! In-process VA-API (libva) for hardware decoding on Linux (docs/adr/0002-linux-video-interop.md).
//!
//! libva is opened at run time ([`crate::va_sys`], `libloading`), so FilmCraft builds and runs
//! without it: [`Display::get`] then reports why, and callers fall back. Everything here is a safe
//! wrapper with RAII handles: a [`Display`] on the AMD (else first working) render node, decoder
//! [`Config`]s and [`Context`]s, a [`SurfacePool`] whose [`Surface`]s go back to the pool when the
//! last holder drops them, parameter / slice [`Buffer`]s, and downloading a surface to CPU memory.
//! No panic crosses into libva: every call returns a `Result` with libva's error string.

use std::ffi::CStr;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::va_sys as sys;

/// The loaded library (`libva-drm.so.2`, which brings `libva.so.2`).
fn lib() -> Result<&'static sys::Va, String> {
    static LIB: OnceLock<Result<sys::Va, String>> = OnceLock::new();
    LIB.get_or_init(|| {
        let mut last = String::new();
        for name in ["libva-drm.so.2", "libva-drm.so"] {
            // SAFETY: loading libva-drm runs its (and libva's) initialisers, which only set up
            // static state; `--dynamic-link-require-all` makes `new` fail unless every declared
            // function resolves, so no call can reach a missing symbol.
            match unsafe { sys::Va::new(name) } {
                Ok(l) => return Ok(l),
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!("libva is not installed ({last})"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// libva's description of a status code.
/// libva's error messages for displays opened here: to the log (debug: a node without a working
/// driver is expected while searching for the 4:2:2 display), never to stderr.
unsafe extern "C" fn on_va_error(_user: *mut std::ffi::c_void, message: *const std::ffi::c_char) {
    let _ = std::panic::catch_unwind(|| {
        if message.is_null() {
            return;
        }
        // SAFETY: libva passes a NUL-terminated message that is valid for the duration of the call.
        let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
        log::debug!("libva: {}", text.trim_end());
    });
}

fn status_text(l: &sys::Va, st: sys::VAStatus) -> String {
    // SAFETY: vaErrorStr takes any status value and returns a pointer to a static string (or null).
    let p = unsafe { l.vaErrorStr(st) };
    if p.is_null() {
        return format!("VA status {st}");
    }
    // SAFETY: non-null, points to a NUL-terminated static string owned by libva.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn check(l: &sys::Va, st: sys::VAStatus, what: &str) -> Result<(), String> {
    if st == 0 { Ok(()) } else { Err(format!("{what}: {}", status_text(l, st))) }
}

/// The HEVC Main 4:2:2 10 display ([`Display::for_hevc_422`]), searched once.
/// Fork L2.6: the GPU (PCI vendor, device) Settings ▸ Playback ▸ Graphics device chose.
static PREFERRED_GPU: OnceLock<(u32, u32)> = OnceLock::new();

/// Fork L2.6: try the render node of this GPU first for the process's display ([`Display::get`]),
/// so decoded frames are imported on the GPU that draws them. Call before the first
/// `Display::get`; only the first call counts. `FILMCRAFT_VAAPI_DEVICE` still wins.
pub fn prefer_gpu(vendor: u32, device: u32) {
    let _ = PREFERRED_GPU.set((vendor, device));
}

static HEVC_422: OnceLock<Result<Arc<Display>, String>> = OnceLock::new();

/// An initialised VA display on a DRM render node.
pub struct Display {
    lib: &'static sys::Va,
    dpy: sys::VADisplay,
    /// The render node, open for as long as the display lives.
    _file: std::fs::File,
    pub node: String,
    pub vendor: String,
    profiles: Vec<(sys::VAProfile, Vec<sys::VAEntrypoint>)>,
    /// Held across each CPU copy between a surface and memory (vaGetImage / vaPutImage and the
    /// image around it): Intel's iHD segfaults in libigdgmm's CPU copy when two threads read the
    /// same tiled surface at once (the XPS 16, compositor and frame worker reading one picture back).
    image_copy: Mutex<()>,
}

// SAFETY: a VADisplay may be used from any thread when its driver serialises its entry points:
// libva itself takes no per-display lock, so this rests on the driver. Both drivers FilmCraft is
// built and tested on do: Mesa's VA frontend (radeonsi, AMD) holds a per-display mutex and Intel's
// iHD media driver a per-display context mutex around every entry point (the 4:2:2 display, fork
// 6.1b). Other drivers (nvidia-vaapi-driver, the old i965) are not checked for this, so
// `open_raw` refuses them (`driver_is_thread_safe`) unless `FILMCRAFT_VAAPI_ANY_DRIVER=1`; they
// then decode in software. FilmCraft never shares a context between threads without its own lock.
unsafe impl Send for Display {}
// SAFETY: as for `Send`; `&Display` only issues thread-safe libva calls.
unsafe impl Sync for Display {}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: `dpy` was initialised by vaInitialize and every object made on it holds an
        // `Arc<Display>`, so none outlives this call.
        unsafe { self.lib.vaTerminate(self.dpy) };
    }
}

impl Display {
    /// The process's display: `FILMCRAFT_VAAPI_DEVICE` when set, else the first render node
    /// (the [`prefer_gpu`] one, then AMD, NVIDIA, other, Intel) that initialises. Opened once.
    pub fn get() -> Result<Arc<Display>, String> {
        static D: OnceLock<Result<Arc<Display>, String>> = OnceLock::new();
        D.get_or_init(|| {
            let lib = lib()?;
            let nodes = match std::env::var("FILMCRAFT_VAAPI_DEVICE").ok().filter(|v| !v.is_empty()) {
                Some(n) => vec![n],
                None => {
                    let mut nodes = render_nodes();
                    if let Some(want) = PREFERRED_GPU.get() {
                        // stable: the chosen GPU's node first, the usual order after it
                        nodes.sort_by_key(|node| crate::pci_ids(node) != Some(*want));
                    }
                    nodes
                }
            };
            let mut why = Vec::new();
            for node in nodes {
                match Display::open(lib, &node) {
                    Ok(d) => {
                        log::info!("VA-API: {} on {}", d.vendor, d.node);
                        return Ok(d);
                    }
                    Err(e) => why.push(format!("{node}: {e}")),
                }
            }
            Err(if why.is_empty() { "no DRM render node".to_string() } else { why.join("; ") })
        })
        .clone()
    }

    /// The display HEVC export encodes on: `FILMCRAFT_VAAPI_ENCODE_DEVICE` when set (another
    /// GPU's video engine, so decoding and encoding don't share one, fork Phase 5.2d), else the
    /// decoding display ([`Display::get`]). Opened once.
    pub fn for_encode() -> Result<Arc<Display>, String> {
        static E: OnceLock<Option<Result<Arc<Display>, String>>> = OnceLock::new();
        let chosen = E.get_or_init(|| {
            let node = std::env::var("FILMCRAFT_VAAPI_ENCODE_DEVICE").ok().filter(|v| !v.is_empty())?;
            Some(lib().and_then(|lib| Display::open(lib, &node)).inspect(|d| {
                log::info!("VA-API encode: {} on {}", d.vendor, d.node);
            }))
        });
        match chosen {
            Some(r) => r.clone(),
            None => Display::get(),
        }
    }

    /// The display on render node `node` (fork 6E.3: one media stack per GPU): the process's
    /// decode or encode display when it is on that node, else one opened once per node and kept.
    pub fn for_node(node: &str) -> Result<Arc<Display>, String> {
        static NODES: Mutex<NodeCache<Arc<Display>>> = Mutex::new(NodeCache::new());
        for d in [Display::get(), Display::for_encode()].into_iter().flatten() {
            if d.node == node {
                return Ok(d);
            }
        }
        // failures are kept too (XPS 16): nvidia-vaapi-driver's node is refused after vaInitialize,
        // which loads CUDA (~0.4 s, and it wakes a sleeping NVIDIA GPU); perf.stats and the
        // two-GPU export option ask for it again and again
        NODES.lock().unwrap_or_else(PoisonError::into_inner).get_or_open(node, || {
            let d = Display::open(lib()?, node).map_err(|e| format!("{node}: {e}"))?;
            log::info!("VA-API: {} on {}", d.vendor, d.node);
            Ok(d)
        })
    }

    /// The display that encodes HEVC Main 4:2:2 10 (fork 6E.4): [`Display::for_encode`] when its
    /// driver has the profile, else the 4:2:2 decoding display ([`Display::for_hevc_422`]) when it
    /// encodes it too, else the first render node that does (on this machine Intel's media
    /// engine: AMD's VCN has no 4:2:2 encoder). Chosen once; an error says why none qualifies.
    pub fn for_encode_422() -> Result<Arc<Display>, String> {
        static E: OnceLock<Result<Arc<Display>, String>> = OnceLock::new();
        E.get_or_init(|| {
            let (profile, enc) = (sys::VAProfile_VAProfileHEVCMain422_10, sys::VAEntrypoint_VAEntrypointEncSlice);
            let mut why = Vec::new();
            let mut seen = Vec::new();
            for d in [Display::for_encode(), Display::for_hevc_422()].into_iter().flatten() {
                if d.supports(profile, enc) {
                    log::info!("VA-API HEVC 4:2:2 encode: {} on {}", d.vendor, d.node);
                    return Ok(d);
                }
                why.push(format!("{}: no HEVC Main 4:2:2 10 encoder ({})", d.node, d.vendor));
                seen.push(d.node.clone());
            }
            let lib = lib()?;
            for node in render_nodes() {
                if seen.contains(&node) {
                    continue;
                }
                match Display::open(lib, &node) {
                    Ok(d) if d.supports(profile, enc) => {
                        log::info!("VA-API HEVC 4:2:2 encode: {} on {}", d.vendor, d.node);
                        return Ok(d);
                    }
                    Ok(d) => why.push(format!("{node}: no HEVC Main 4:2:2 10 encoder ({})", d.vendor)),
                    Err(e) => why.push(format!("{node}: {e}")),
                }
            }
            Err(if why.is_empty() { "no DRM render node".to_string() } else { why.join("; ") })
        })
        .clone()
    }

    /// The display that decodes HEVC Main 4:2:2 10 (fork Phase 6.1b):
    /// `FILMCRAFT_VAAPI_422_DEVICE` when set, else [`Display::get`] when its driver has the
    /// profile, else the first render node whose driver advertises `VAProfileHEVCMain422_10`
    /// with the VLD entry point (on this machine Intel's media engine: AMD's VCN has no 4:2:2
    /// decoding). Chosen once; an error says why no node qualifies.
    pub fn for_hevc_422() -> Result<Arc<Display>, String> {
        HEVC_422
            .get_or_init(|| {
                let (profile, vld) = (sys::VAProfile_VAProfileHEVCMain422_10, sys::VAEntrypoint_VAEntrypointVLD);
                let lib = lib()?;
                if let Some(node) = std::env::var("FILMCRAFT_VAAPI_422_DEVICE").ok().filter(|v| !v.is_empty()) {
                    let d = Display::open(lib, &node).map_err(|e| format!("{node}: {e}"))?;
                    if !d.supports(profile, vld) {
                        return Err(format!("{node} ({}) has no HEVC Main 4:2:2 10 decoder", d.vendor));
                    }
                    log::info!("VA-API HEVC 4:2:2: {} on {}", d.vendor, d.node);
                    return Ok(d);
                }
                let mut why = Vec::new();
                if let Ok(d) = Display::get() {
                    if d.supports(profile, vld) {
                        log::info!("VA-API HEVC 4:2:2: {} on {}", d.vendor, d.node);
                        return Ok(d);
                    }
                    why.push(format!("{}: no HEVC Main 4:2:2 10 decoder", d.node));
                }
                let skip = Display::get().ok().map(|d| d.node.clone());
                for node in render_nodes() {
                    if skip.as_deref() == Some(node.as_str()) {
                        continue;
                    }
                    match Display::open(lib, &node) {
                        Ok(d) if d.supports(profile, vld) => {
                            log::info!("VA-API HEVC 4:2:2: {} on {}", d.vendor, d.node);
                            return Ok(d);
                        }
                        Ok(d) => why.push(format!("{node}: no HEVC Main 4:2:2 10 decoder ({})", d.vendor)),
                        Err(e) => why.push(format!("{node}: {e}")),
                    }
                }
                Err(if why.is_empty() { "no DRM render node".to_string() } else { why.join("; ") })
            })
            .clone()
    }

    /// Whether [`Display::for_hevc_422`] found a display, without waiting: `None` until its
    /// search (vaInitialize on each render node) has finished.
    pub fn hevc_422_ready() -> Option<bool> {
        HEVC_422.get().map(Result::is_ok)
    }

    /// The display on `node`, kept open for the life of the process (U0.2.4): libva unloads a
    /// driver when its last display is terminated, and terminating one Intel (iHD) display while
    /// another thread initialised one corrupted the heap (`filmcraft-cli help` aborted in about
    /// one run in four once upstream's always-open display was gone). A handful at most: one per
    /// render node per kind of use.
    fn open(lib: &'static sys::Va, node: &str) -> Result<Arc<Display>, String> {
        const KEPT_MAX: usize = 64;
        static KEPT: Mutex<Vec<Arc<Display>>> = Mutex::new(Vec::new());
        // one at a time: drivers load and initialise their global state in vaInitialize, and the
        // start-up probes open displays on several threads at once
        static OPENING: Mutex<()> = Mutex::new(());
        let busy = Busy::enter().ok_or("the process is exiting")?;
        let opening = OPENING.lock().unwrap_or_else(PoisonError::into_inner);
        let d = Arc::new(Display::open_raw(lib, node)?);
        drop((opening, busy));
        let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
        // a bound against a runaway caller (today's callers open at most 8); a display past it is
        // terminated when its last user drops it, the hazard above, so say so
        if kept.len() < KEPT_MAX {
            kept.push(d.clone());
        } else {
            log::warn!("VA-API: more than {KEPT_MAX} displays opened; {node} is not kept open for the process's life");
        }
        Ok(d)
    }

    fn open_raw(lib: &'static sys::Va, node: &str) -> Result<Display, String> {
        // NVIDIA's node answers VA-API only through nvidia-vaapi-driver, which is refused below
        // anyway; its vaInitialize loads CUDA, which wakes a sleeping laptop dGPU and keeps it
        // awake (XPS 16, Power saving): known from sysfs, without opening it
        let any_driver = std::env::var("FILMCRAFT_VAAPI_ANY_DRIVER").ok().as_deref() == Some("1");
        if skip_unopened(vendor(node), any_driver) {
            return Err(format!("{node}: NVIDIA's GPU (its VA-API driver is not used; FILMCRAFT_VAAPI_ANY_DRIVER=1 tries it)"));
        }
        OPENED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file = std::fs::OpenOptions::new().read(true).write(true).open(node).map_err(|e| e.to_string())?;
        // SAFETY: the fd is a valid open DRM node, kept open by `file` for the display's lifetime.
        let dpy = unsafe { lib.vaGetDisplayDRM(file.as_raw_fd()) };
        if dpy.is_null() {
            return Err("vaGetDisplayDRM failed".into());
        }
        // libva prints driver-loading failures ("iHD_drv_video.so init failed" while probing a node
        // for the 4:2:2 display) to stderr unless callbacks are set: route errors to the log and
        // turn info messages off, as upstream's display layer did before U0.2.4 (CLI stderr stays clean).
        // SAFETY: `dpy` is a display from vaGetDisplayDRM; a null info callback turns libva's info
        // messages off, and `on_va_error` matches `VAMessageCallback` and never unwinds.
        unsafe {
            lib.vaSetInfoCallback(dpy, None, std::ptr::null_mut());
            lib.vaSetErrorCallback(dpy, Some(on_va_error), std::ptr::null_mut());
        }
        let (mut major, mut minor) = (0, 0);
        // SAFETY: `dpy` is a fresh display; the out-pointers are valid locals.
        let st = unsafe { lib.vaInitialize(dpy, &mut major, &mut minor) };
        if st != 0 {
            let e = status_text(lib, st);
            // SAFETY: terminating a display whose initialisation failed is allowed and frees it.
            unsafe { lib.vaTerminate(dpy) };
            return Err(format!("vaInitialize: {e}"));
        }
        // SAFETY: initialised display; returns a static string or null.
        let vp = unsafe { lib.vaQueryVendorString(dpy) };
        // SAFETY: non-null pointers from vaQueryVendorString are NUL-terminated strings owned by libva.
        let vendor = if vp.is_null() { String::new() } else { unsafe { CStr::from_ptr(vp) }.to_string_lossy().into_owned() };
        let mut d = Display { lib, dpy, _file: file, node: node.to_string(), vendor, profiles: Vec::new(), image_copy: Mutex::new(()) };
        // the `Send` / `Sync` impls above rest on the driver's own per-display lock; `d` drops
        // (vaTerminate) while `OPENING` is still held by `open`
        if !driver_is_thread_safe(&d.vendor) && !any_driver {
            log::warn!(
                "VA-API: {node} ({}) is not a driver Revolution has checked for thread safety; using software decoding \
                 (FILMCRAFT_VAAPI_ANY_DRIVER=1 uses it anyway)",
                d.vendor
            );
            return Err(format!("{node}: VA-API driver not supported ({})", d.vendor));
        }
        d.profiles = d.query_profiles()?;
        Ok(d)
    }

    fn query_profiles(&self) -> Result<Vec<(sys::VAProfile, Vec<sys::VAEntrypoint>)>, String> {
        let l = self.lib;
        // SAFETY: initialised display.
        let max = cap(unsafe { l.vaMaxNumProfiles(self.dpy) }, 256, "profiles")?;
        let mut profiles = vec![0 as sys::VAProfile; max];
        let mut n = 0;
        // SAFETY: the array holds vaMaxNumProfiles entries, as libva requires.
        check(l, unsafe { l.vaQueryConfigProfiles(self.dpy, profiles.as_mut_ptr(), &mut n) }, "vaQueryConfigProfiles")?;
        profiles.truncate((n.max(0) as usize).min(max));
        // SAFETY: initialised display.
        let max_ep = cap(unsafe { l.vaMaxNumEntrypoints(self.dpy) }, 64, "entry points")?;
        let mut out = Vec::new();
        for p in profiles {
            let mut eps = vec![0 as sys::VAEntrypoint; max_ep];
            let mut m = 0;
            // SAFETY: the array holds vaMaxNumEntrypoints entries.
            if unsafe { l.vaQueryConfigEntrypoints(self.dpy, p, eps.as_mut_ptr(), &mut m) } == 0 {
                eps.truncate((m.max(0) as usize).min(max_ep));
                out.push((p, eps));
            }
        }
        Ok(out)
    }

    /// Whether the driver has `profile` with `entrypoint`.
    pub fn supports(&self, profile: sys::VAProfile, entrypoint: sys::VAEntrypoint) -> bool {
        self.profiles.iter().any(|(p, eps)| *p == profile && eps.contains(&entrypoint))
    }

    fn check(&self, st: sys::VAStatus, what: &str) -> Result<(), String> {
        check(self.lib, st, what)
    }

    /// The driver's values of the configuration attributes `types` for `profile` / `entrypoint`
    /// (`None`: not supported).
    pub fn config_attribs(
        &self,
        profile: sys::VAProfile,
        entrypoint: sys::VAEntrypoint,
        types: &[sys::VAConfigAttribType],
    ) -> Result<Vec<Option<u32>>, String> {
        let mut attribs: Vec<sys::VAConfigAttrib> = types.iter().map(|&t| sys::VAConfigAttrib { type_: t, value: 0 }).collect();
        let n = i32::try_from(attribs.len()).map_err(|_| "too many attributes")?;
        // SAFETY: `attribs` holds `n` entries, which libva fills in place.
        self.check(unsafe { self.lib.vaGetConfigAttributes(self.dpy, profile, entrypoint, attribs.as_mut_ptr(), n) }, "vaGetConfigAttributes")?;
        Ok(attribs.iter().map(|a| (a.value != sys::VA_ATTRIB_NOT_SUPPORTED).then_some(a.value)).collect())
    }
}

/// A decoder configuration (profile + entrypoint + render-target format).
pub struct Config {
    display: Arc<Display>,
    pub id: sys::VAConfigID,
    /// The render-target format (`VA_RT_FORMAT_*`) asked for; 0 when not given.
    pub rt_format: u32,
}

impl Drop for Config {
    fn drop(&mut self) {
        // SAFETY: `id` was created on this display and is destroyed once.
        unsafe { self.display.lib.vaDestroyConfig(self.display.dpy, self.id) };
    }
}

impl Config {
    pub fn new(display: &Arc<Display>, profile: sys::VAProfile, entrypoint: sys::VAEntrypoint, rt_format: u32) -> Result<Config, String> {
        let mut attrib = sys::VAConfigAttrib { type_: sys::VAConfigAttribType_VAConfigAttribRTFormat, value: rt_format };
        let mut id = 0;
        // SAFETY: one valid attribute and a valid out-pointer.
        display.check(unsafe { display.lib.vaCreateConfig(display.dpy, profile, entrypoint, &mut attrib, 1, &mut id) }, "vaCreateConfig")?;
        Ok(Config { display: display.clone(), id, rt_format })
    }

    /// A configuration with the attributes `(type, value)` (encoders: render-target format, rate
    /// control, packed headers).
    pub fn with_attribs(
        display: &Arc<Display>,
        profile: sys::VAProfile,
        entrypoint: sys::VAEntrypoint,
        attribs: &[(sys::VAConfigAttribType, u32)],
    ) -> Result<Config, String> {
        let mut list: Vec<sys::VAConfigAttrib> = attribs.iter().map(|&(t, v)| sys::VAConfigAttrib { type_: t, value: v }).collect();
        let n = i32::try_from(list.len()).map_err(|_| "too many attributes")?;
        let mut id = 0;
        // SAFETY: `list` holds `n` valid attributes; valid out-pointer.
        display.check(unsafe { display.lib.vaCreateConfig(display.dpy, profile, entrypoint, list.as_mut_ptr(), n, &mut id) }, "vaCreateConfig")?;
        let rt_format = attribs.iter().find(|(t, _)| *t == sys::VAConfigAttribType_VAConfigAttribRTFormat).map_or(0, |&(_, v)| v);
        Ok(Config { display: display.clone(), id, rt_format })
    }
}

/// A decoding context of a configuration and picture size. `Send` but not `Sync`: a picture's
/// begin / render / end sequence must not interleave with another thread's on the same context.
pub struct Context {
    display: Arc<Display>,
    _config: Arc<Config>,
    pub id: sys::VAContextID,
    pub size: (u32, u32),
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: `id` was created on this display and is destroyed once; every `Buffer` of the
        // context borrows it, so none is left to be destroyed after it.
        unsafe { self.display.lib.vaDestroyContext(self.display.dpy, self.id) };
    }
}

/// A VA parameter struct and the buffer type it fills: the driver reads exactly one whole struct
/// of the type from a buffer of that kind, so only these types can be passed
/// ([`Context::param_buffer`]); sealed.
pub trait ParamBuffer: Copy + sealed::Sealed {
    const KIND: sys::VABufferType;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! param_buffer {
    ($($t:ty => $kind:expr),* $(,)?) => {$(
        impl sealed::Sealed for $t {}
        impl ParamBuffer for $t {
            const KIND: sys::VABufferType = $kind;
        }
    )*};
}

param_buffer! {
    sys::VAPictureParameterBufferHEVC => sys::VABufferType_VAPictureParameterBufferType,
    sys::VAIQMatrixBufferHEVC => sys::VABufferType_VAIQMatrixBufferType,
    sys::VASliceParameterBufferHEVC => sys::VABufferType_VASliceParameterBufferType,
    // range-extension profiles (Main 4:2:2 10): base + rext (+ scc) in one buffer (fork 6.1b)
    sys::VAPictureParameterBufferHEVCExtension => sys::VABufferType_VAPictureParameterBufferType,
    sys::VASliceParameterBufferHEVCExtension => sys::VABufferType_VASliceParameterBufferType,
    sys::VAEncPackedHeaderParameterBuffer => sys::VABufferType_VAEncPackedHeaderParameterBufferType,
    MiscRateControl => sys::VABufferType_VAEncMiscParameterBufferType,
    MiscHrd => sys::VABufferType_VAEncMiscParameterBufferType,
    MiscFrameRate => sys::VABufferType_VAEncMiscParameterBufferType,
    MiscQualityLevel => sys::VABufferType_VAEncMiscParameterBufferType,
    // upstream's hand-written decode structs (`vaapi/ffi.rs`, layouts checked against the header
    // in `vaapi/abi_tests.rs`), filled by its front ends: the fork's zero-copy accelerator
    // (`va_accel.rs`, Y1) hands them to this context
    crate::vaapi::ffi::VAPictureParameterBufferH264 => sys::VABufferType_VAPictureParameterBufferType,
    crate::vaapi::ffi::VAIQMatrixBufferH264 => sys::VABufferType_VAIQMatrixBufferType,
    crate::vaapi::ffi::VASliceParameterBufferH264 => sys::VABufferType_VASliceParameterBufferType,
    crate::vaapi::ffi::VAPictureParameterBufferHEVC => sys::VABufferType_VAPictureParameterBufferType,
    crate::vaapi::ffi::VAIQMatrixBufferHEVC => sys::VABufferType_VAIQMatrixBufferType,
    crate::vaapi::ffi::VASliceParameterBufferHEVC => sys::VABufferType_VASliceParameterBufferType,
    // range-extension profiles through upstream's front end (fork U0.2.3)
    crate::vaapi::ffi::VAPictureParameterBufferHEVCExtension => sys::VABufferType_VAPictureParameterBufferType,
    crate::vaapi::ffi::VASliceParameterBufferHEVCExtension => sys::VABufferType_VASliceParameterBufferType,
}

// the two bindings agree on the HEVC decode structs they both have (field offsets of the
// range-extension ones: `vaapi/abi_tests.rs`) (the H.264 ones exist only in
// `vaapi/ffi.rs`, header-checked there)
const _: () = {
    assert!(std::mem::size_of::<crate::vaapi::ffi::VAPictureParameterBufferHEVC>() == std::mem::size_of::<sys::VAPictureParameterBufferHEVC>());
    assert!(std::mem::size_of::<crate::vaapi::ffi::VAIQMatrixBufferHEVC>() == std::mem::size_of::<sys::VAIQMatrixBufferHEVC>());
    assert!(std::mem::size_of::<crate::vaapi::ffi::VASliceParameterBufferHEVC>() == std::mem::size_of::<sys::VASliceParameterBufferHEVC>());
    assert!(
        std::mem::size_of::<crate::vaapi::ffi::VAPictureParameterBufferHEVCExtension>() == std::mem::size_of::<sys::VAPictureParameterBufferHEVCExtension>()
    );
    assert!(std::mem::size_of::<crate::vaapi::ffi::VASliceParameterBufferHEVCExtension>() == std::mem::size_of::<sys::VASliceParameterBufferHEVCExtension>());
};

/// `VAEncMiscParameterBuffer` followed by its payload, as libva lays it out (a `type` word, then
/// the payload struct; both 4-byte aligned, so `repr(C)` adds no padding between them).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Misc<T: Copy> {
    kind: sys::VAEncMiscParameterType,
    pub payload: T,
}

// libva reads a `type` word directly followed by the payload: no padding in between
const _: () = {
    assert!(std::mem::size_of::<Misc<sys::VAEncMiscParameterRateControl>>() == 4 + std::mem::size_of::<sys::VAEncMiscParameterRateControl>());
    assert!(std::mem::size_of::<Misc<sys::VAEncMiscParameterHRD>>() == 4 + std::mem::size_of::<sys::VAEncMiscParameterHRD>());
    assert!(std::mem::size_of::<Misc<sys::VAEncMiscParameterFrameRate>>() == 4 + std::mem::size_of::<sys::VAEncMiscParameterFrameRate>());
    assert!(std::mem::size_of::<Misc<QualityLevel>>() == 4 + 20);
};

/// `VAEncMiscParameterBufferQualityLevel` (va.h; not in the generated bindings): the level, then
/// `VA_PADDING_LOW` (4) reserved words that must be zero.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct QualityLevel {
    pub quality_level: u32,
    pub va_reserved: [u32; 4],
}

pub type MiscRateControl = Misc<sys::VAEncMiscParameterRateControl>;
pub type MiscHrd = Misc<sys::VAEncMiscParameterHRD>;
pub type MiscFrameRate = Misc<sys::VAEncMiscParameterFrameRate>;
pub type MiscQualityLevel = Misc<QualityLevel>;

impl MiscRateControl {
    pub fn new(payload: sys::VAEncMiscParameterRateControl) -> Self {
        Misc { kind: sys::VAEncMiscParameterType_VAEncMiscParameterTypeRateControl, payload }
    }
}
impl MiscHrd {
    pub fn new(payload: sys::VAEncMiscParameterHRD) -> Self {
        Misc { kind: sys::VAEncMiscParameterType_VAEncMiscParameterTypeHRD, payload }
    }
}
impl MiscQualityLevel {
    pub fn new(level: u32) -> Self {
        Misc { kind: sys::VAEncMiscParameterType_VAEncMiscParameterTypeQualityLevel, payload: QualityLevel { quality_level: level, va_reserved: [0; 4] } }
    }
}
impl MiscFrameRate {
    pub fn new(payload: sys::VAEncMiscParameterFrameRate) -> Self {
        Misc { kind: sys::VAEncMiscParameterType_VAEncMiscParameterTypeFrameRate, payload }
    }
}

impl Context {
    /// A context without fixed render targets (surfaces from any pool of the same format).
    pub fn new(config: &Arc<Config>, width: u32, height: u32) -> Result<Context, String> {
        let d = &config.display;
        let mut id = 0;
        let (w, h) = (i32::try_from(width).map_err(|_| "width")?, i32::try_from(height).map_err(|_| "height")?);
        // SAFETY: a valid config; no render-target list (null, 0); valid out-pointer.
        d.check(unsafe { d.lib.vaCreateContext(d.dpy, config.id, w, h, sys::VA_PROGRESSIVE as i32, std::ptr::null_mut(), 0, &mut id) }, "vaCreateContext")?;
        Ok(Context { display: d.clone(), _config: config.clone(), id, size: (width, height), _not_sync: std::marker::PhantomData })
    }

    /// A parameter buffer holding a copy of `v`, of the kind its type fills.
    pub fn param_buffer<T: ParamBuffer>(&self, v: &T) -> Result<Buffer<'_>, String> {
        // SAFETY: `v` is a live `T`, readable for `size_of::<T>()` bytes; libva copies them with
        // memcpy (padding included, which C may copy) and does not keep the pointer.
        unsafe { self.create_buffer(T::KIND, (v as *const T).cast(), std::mem::size_of::<T>()) }
    }

    /// An HEVC encoder's sequence parameters, checked: the picture size they declare fits this
    /// context (and so every surface [`Context::encode_hevc`] accepts).
    pub fn sequence_buffer(&self, seq: &sys::VAEncSequenceParameterBufferHEVC) -> Result<Buffer<'_>, String> {
        let (w, h) = (u32::from(seq.pic_width_in_luma_samples), u32::from(seq.pic_height_in_luma_samples));
        if w == 0 || h == 0 || w > self.size.0 || h > self.size.1 {
            return Err(format!("a {w}x{h} sequence for a {}x{} context", self.size.0, self.size.1));
        }
        // SAFETY: `seq` is a live VAEncSequenceParameterBufferHEVC, copied by libva.
        unsafe {
            self.create_buffer(
                sys::VABufferType_VAEncSequenceParameterBufferType,
                (seq as *const sys::VAEncSequenceParameterBufferHEVC).cast(),
                std::mem::size_of_val(seq),
            )
        }
    }

    /// A slice-data buffer holding a copy of `data` (the slice NAL unit). The slice parameters
    /// that go with it must lie within it (checked by the caller against `data.len()`).
    pub fn slice_data(&self, data: &[u8]) -> Result<Buffer<'_>, String> {
        // SAFETY: `data` is readable for `data.len()` bytes; libva copies them.
        unsafe { self.create_buffer(sys::VABufferType_VASliceDataBufferType, data.as_ptr().cast(), data.len()) }
    }

    /// # Safety
    /// `data` must be readable for `len` bytes, and hold what the driver expects of `kind`.
    unsafe fn create_buffer(&self, kind: sys::VABufferType, data: *const std::ffi::c_void, len: usize) -> Result<Buffer<'_>, String> {
        let d = &self.display;
        let size = u32::try_from(len).map_err(|_| "buffer too large")?;
        let mut id = 0;
        // SAFETY: (the caller's contract) libva copies `size` bytes from `data` into a new buffer;
        // it does not write through the pointer despite the `*mut` in its signature.
        d.check(unsafe { d.lib.vaCreateBuffer(d.dpy, self.id, kind, size, 1, data.cast_mut(), &mut id) }, "vaCreateBuffer")?;
        Ok(Buffer { context: self, id })
    }

    /// Decode one picture into `target`: begin, render every buffer, end.
    pub fn decode(&self, target: &Surface, buffers: &[Buffer<'_>]) -> Result<(), String> {
        let d = &self.display;
        if buffers.iter().any(|b| !std::ptr::eq(b.context, self)) {
            return Err("a buffer of another context".into());
        }
        // the driver writes a whole context-sized picture into the target: a surface of another
        // display, or a smaller one, would be written out of bounds
        let (tw, th) = target.pool.size;
        if !Arc::ptr_eq(&target.pool.display, d) || tw < self.size.0 || th < self.size.1 {
            return Err(format!("a {tw}x{th} target surface for a {}x{} context", self.size.0, self.size.1));
        }
        // the process doesn't exit (drivers' destructors) in the middle of a picture (review 2026-10-15)
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        // SAFETY: a valid context and surface of this display.
        d.check(unsafe { d.lib.vaBeginPicture(d.dpy, self.id, target.id) }, "vaBeginPicture")?;
        let mut ids: Vec<sys::VABufferID> = buffers.iter().map(|b| b.id).collect();
        let n = i32::try_from(ids.len()).map_err(|_| "too many buffers")?;
        // SAFETY: `ids` holds `n` buffers created on this context; libva reads them during the call.
        let r = d.check(unsafe { d.lib.vaRenderPicture(d.dpy, self.id, ids.as_mut_ptr(), n) }, "vaRenderPicture");
        // SAFETY: ends the picture begun above (also after a render error, so the context stays usable).
        let e = d.check(unsafe { d.lib.vaEndPicture(d.dpy, self.id) }, "vaEndPicture");
        r.and(e)
    }

    /// Copy the rectangle `region` (x, y, w, h) of `src` to the top-left corner of `target` with
    /// the video processor (this context must be of a `VAEntrypointVideoProc` config): both
    /// surfaces of this display and of one format, no scaling, no colour conversion (the same
    /// colour standard on both sides). Fork 6.1b: a decoded (tiled) 4:2:2 picture into a linear
    /// surface another GPU can read. Not waited for: sync `target` before reading it.
    pub fn process_copy(&self, src: &Surface, region: (u32, u32, u32, u32), target: &Surface) -> Result<(), String> {
        self.process_scaled(src, region, target, (region.2, region.3))
    }

    /// [`Context::process_copy`] that scales the rectangle `region` of `src` to `out` (w, h)
    /// at the top-left of `target` (U0.2.3b d: a hand-off copy at the export size).
    pub fn process_scaled(&self, src: &Surface, region: (u32, u32, u32, u32), target: &Surface, out: (u32, u32)) -> Result<(), String> {
        let d = &self.display;
        if !Arc::ptr_eq(&src.pool.display, d) || !Arc::ptr_eq(&target.pool.display, d) {
            return Err("a surface of another display".into());
        }
        if src.pool.fourcc != target.pool.fourcc {
            return Err(format!("copying fourcc {:#x} into {:#x}", src.pool.fourcc, target.pool.fourcc));
        }
        let (x, y, w, h) = region;
        let (tw, th) = target.pool.size;
        let (ow, oh) = out;
        if !rect_fits(region, src.pool.size) || ow == 0 || oh == 0 || ow > tw || oh > th {
            return Err(format!("a {w}x{h} copy at ({x}, {y}) from {:?} to {ow}x{oh} into {tw}x{th}", src.pool.size));
        }
        let small = |v: u32| i16::try_from(v).map_err(|_| format!("{v} does not fit a VA rectangle"));
        let size = |v: u32| u16::try_from(v).map_err(|_| format!("{v} does not fit a VA rectangle"));
        let from = sys::VARectangle { x: small(x)?, y: small(y)?, width: size(w)?, height: size(h)? };
        let to = sys::VARectangle { x: 0, y: 0, width: size(ow)?, height: size(oh)? };
        let bt709 = sys::_VAProcColorStandardType_VAProcColorStandardBT709;
        let pp = sys::VAProcPipelineParameterBuffer {
            surface: src.id,
            surface_region: &from,
            output_region: &to,
            surface_color_standard: bt709,
            output_color_standard: bt709,
            filter_flags: if (ow, oh) == (w, h) { sys::VA_FILTER_SCALING_FAST } else { scaling_filter() },
            ..Default::default()
        };
        // SAFETY: `pp` is a live VAProcPipelineParameterBuffer that libva copies; its region
        // pointers (`from`, `to`) stay valid on this stack frame until vaEndPicture below, the
        // last call that may read them; every other pointer field is null with a zero count.
        let buffer = unsafe {
            self.create_buffer(
                sys::VABufferType_VAProcPipelineParameterBufferType,
                (&pp as *const sys::VAProcPipelineParameterBuffer).cast(),
                std::mem::size_of::<sys::VAProcPipelineParameterBuffer>(),
            )
        }?;
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        // SAFETY: a valid context and target surface of this display.
        d.check(unsafe { d.lib.vaBeginPicture(d.dpy, self.id, target.id) }, "vaBeginPicture (video processing)")?;
        let mut ids = [buffer.id];
        // SAFETY: one buffer created on this context; libva reads it (and the regions it points
        // to, alive here) during the call.
        let r = d.check(unsafe { d.lib.vaRenderPicture(d.dpy, self.id, ids.as_mut_ptr(), 1) }, "vaRenderPicture (video processing)");
        // SAFETY: ends the picture begun above (also after a render error).
        let e = d.check(unsafe { d.lib.vaEndPicture(d.dpy, self.id) }, "vaEndPicture (video processing)");
        drop(buffer);
        r.and(e)
    }

    /// A packed header for an encoder: the parameter buffer saying what it is (`kind`, a
    /// `VAEncPackedHeaderType`) and the data buffer holding `data` (Annex B NAL units, escaped).
    pub fn packed_header(&self, kind: u32, data: &[u8]) -> Result<[Buffer<'_>; 2], String> {
        let bits = u32::try_from(data.len()).ok().and_then(|n| n.checked_mul(8)).ok_or("packed header too large")?;
        let param = sys::VAEncPackedHeaderParameterBuffer { type_: kind, bit_length: bits, has_emulation_bytes: 1, ..Default::default() };
        let p = self.param_buffer(&param)?;
        // SAFETY: `data` is readable for `data.len()` bytes; libva copies them (the bit length
        // above covers exactly these bytes).
        let d = unsafe { self.create_buffer(sys::VABufferType_VAEncPackedHeaderDataBufferType, data.as_ptr().cast(), data.len()) }?;
        Ok([p, d])
    }

    /// A buffer the encoder writes a picture's bitstream into (`bytes` large).
    pub fn coded_buffer(&self, bytes: usize) -> Result<CodedBuffer, String> {
        let d = &self.display;
        let size = u32::try_from(bytes).map_err(|_| "coded buffer too large")?;
        let mut id = 0;
        let kind = sys::VABufferType_VAEncCodedBufferType;
        // SAFETY: no initial data (null): libva allocates `size` bytes the encoder writes.
        let st = unsafe { d.lib.vaCreateBuffer(d.dpy, self.id, kind, size, 1, std::ptr::null_mut(), &mut id) };
        d.check(st, "vaCreateBuffer (coded)")?;
        Ok(CodedBuffer { display: d.clone(), context: self.id, id, size: bytes })
    }

    /// Encode `source` as one HEVC picture into `coded`, reconstructing it into `recon`, predicted
    /// from `reference` (P) or intra (`None`). The surfaces and the coded buffer are filled into
    /// `pic` / `slice` here, so the driver only ever sees surfaces of this display large enough
    /// for the picture and this context's coded buffer. Buffers go to the driver in libva's
    /// order: `before` (sequence, misc, packed headers), the picture, the slice.
    pub fn encode_hevc(
        &self,
        source: &Surface,
        recon: &Surface,
        reference: Option<(&Surface, i32)>,
        coded: &CodedBuffer,
        mut pic: sys::VAEncPictureParameterBufferHEVC,
        mut slice: sys::VAEncSliceParameterBufferHEVC,
        before: &[Buffer<'_>],
    ) -> Result<(), String> {
        let d = &self.display;
        if coded.context != self.id || !Arc::ptr_eq(&coded.display, d) {
            return Err("a coded buffer of another context".into());
        }
        if before.iter().any(|b| !std::ptr::eq(b.context, self)) {
            return Err("a buffer of another context".into());
        }
        for s in [Some(source), Some(recon), reference.map(|r| r.0)].into_iter().flatten() {
            let (w, h) = s.pool.size;
            if !Arc::ptr_eq(&s.pool.display, d) || w < self.size.0 || h < self.size.1 {
                return Err(format!("a {w}x{h} surface for a {}x{} encoder", self.size.0, self.size.1));
            }
            if self._config.rt_format != 0 && s.pool.rt_format != self._config.rt_format {
                return Err(format!("a surface of format {:#x} for a {:#x} encoder", s.pool.rt_format, self._config.rt_format));
            }
        }
        let invalid = sys::VAPictureHEVC { picture_id: sys::VA_INVALID_SURFACE, flags: sys::VA_PICTURE_HEVC_INVALID, ..Default::default() };
        pic.decoded_curr_pic = sys::VAPictureHEVC { picture_id: recon.id, pic_order_cnt: pic.decoded_curr_pic.pic_order_cnt, flags: 0, ..Default::default() };
        pic.reference_frames = [invalid; 15];
        slice.ref_pic_list0 = [invalid; 15];
        slice.ref_pic_list1 = [invalid; 15];
        // one reference at most (list entry 0, filled below): the driver reads no further entries
        pic.num_ref_idx_l0_default_active_minus1 = 0;
        pic.num_ref_idx_l1_default_active_minus1 = 0;
        slice.num_ref_idx_l0_active_minus1 = 0;
        slice.num_ref_idx_l1_active_minus1 = 0;
        if let Some((r, poc)) = reference {
            let p = sys::VAPictureHEVC { picture_id: r.id, pic_order_cnt: poc, flags: 0, ..Default::default() };
            pic.reference_frames[0] = p;
            slice.ref_pic_list0[0] = p;
            if slice.slice_type == 0 {
                // low-delay B (fork 6E.3 S12): list 1 is list 0
                slice.ref_pic_list1[0] = p;
            }
        }
        pic.coded_buf = coded.id;
        // not `ParamBuffer`s: only made here, with the ids checked above
        // SAFETY: `pic` is a live VAEncPictureParameterBufferHEVC, copied by libva.
        let pb = unsafe {
            self.create_buffer(
                sys::VABufferType_VAEncPictureParameterBufferType,
                (&pic as *const sys::VAEncPictureParameterBufferHEVC).cast(),
                std::mem::size_of_val(&pic),
            )
        }?;
        // SAFETY: `slice` is a live VAEncSliceParameterBufferHEVC, copied by libva.
        let sb = unsafe {
            self.create_buffer(
                sys::VABufferType_VAEncSliceParameterBufferType,
                (&slice as *const sys::VAEncSliceParameterBufferHEVC).cast(),
                std::mem::size_of_val(&slice),
            )
        }?;
        let mut ids: Vec<sys::VABufferID> = before.iter().map(|b| b.id).collect();
        ids.push(pb.id);
        ids.push(sb.id);
        let n = i32::try_from(ids.len()).map_err(|_| "too many buffers")?;
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        // SAFETY: a valid context and source surface of this display.
        d.check(unsafe { d.lib.vaBeginPicture(d.dpy, self.id, source.id) }, "vaBeginPicture")?;
        // SAFETY: `ids` holds `n` buffers created on this context; libva reads them during the call.
        let r = d.check(unsafe { d.lib.vaRenderPicture(d.dpy, self.id, ids.as_mut_ptr(), n) }, "vaRenderPicture");
        // SAFETY: ends the picture begun above (also after a render error).
        let e = d.check(unsafe { d.lib.vaEndPicture(d.dpy, self.id) }, "vaEndPicture");
        r.and(e)
    }
}

/// The encoder's output buffer of a context (not borrowed from it, so an encoder can keep several
/// in flight); destroyed on drop. libva buffers do not depend on their context once made.
pub struct CodedBuffer {
    display: Arc<Display>,
    context: sys::VAContextID,
    id: sys::VABufferID,
    size: usize,
}

impl Drop for CodedBuffer {
    fn drop(&mut self) {
        // SAFETY: `id` was created on this display and is destroyed once (encoders never destroy
        // coded buffers themselves).
        unsafe { self.display.lib.vaDestroyBuffer(self.display.dpy, self.id) };
    }
}

/// Segments of one coded buffer followed at most (a picture is one or a few).
const MAX_SEGMENTS: usize = 64;

impl CodedBuffer {
    /// The bitstream the encoder wrote (waits for the picture first: call after syncing its
    /// source surface). Error when it overflowed the buffer. `&mut`: one mapping at a time (the
    /// driver keeps one per buffer).
    pub fn read(&mut self) -> Result<Vec<u8>, String> {
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        let d = &self.display;
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: a coded buffer of this display, mapped into a valid out-pointer.
        d.check(unsafe { d.lib.vaMapBuffer(d.dpy, self.id, &mut ptr) }, "vaMapBuffer (coded)")?;
        let mut out = Vec::new();
        let mut result = Ok(());
        let mut seg = ptr as *const sys::VACodedBufferSegment;
        for _ in 0..MAX_SEGMENTS {
            if seg.is_null() {
                break;
            }
            // SAFETY: a mapped coded buffer is a linked list of VACodedBufferSegment structs owned
            // by the driver, valid until vaUnmapBuffer; `seg` is the head or a `next` link (read
            // unaligned: the pointer comes from the driver).
            let s = unsafe { std::ptr::read_unaligned(seg) };
            if s.status & sys::VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK != 0 {
                result = Err(format!("the picture overflowed the {} byte coded buffer", self.size));
                break;
            }
            let n = s.size as usize;
            if n > self.size || out.len().saturating_add(n) > self.size {
                result = Err("coded segment larger than its buffer".to_string());
                break;
            }
            if n > 0 {
                if s.buf.is_null() || s.bit_offset != 0 {
                    result = Err("coded segment without data or not byte aligned".to_string());
                    break;
                }
                // SAFETY: the segment's `buf` holds `size` bytes while mapped (bounded by the
                // buffer's size above).
                out.extend_from_slice(unsafe { std::slice::from_raw_parts(s.buf as *const u8, n) });
            }
            seg = s.next as *const sys::VACodedBufferSegment;
        }
        if result.is_ok() && !seg.is_null() {
            result = Err(format!("more than {MAX_SEGMENTS} coded segments"));
        }
        // SAFETY: unmaps the mapping made above; nothing read from it is used after this.
        unsafe { d.lib.vaUnmapBuffer(d.dpy, self.id) };
        result.map(|()| out)
    }
}

/// Allocates linear DMA-BUF memory of at least the given number of bytes (another device's, for
/// [`LinearAlloc::Imported`]); the caller owns the descriptor.
pub type DmaBufAlloc = Arc<dyn Fn(u64) -> Result<std::os::fd::OwnedFd, String> + Send + Sync>;

/// Ways of getting untiled VA surfaces, tried in this order until the export says LINEAR
/// ([`LinearCopy::new`]).
#[derive(Clone)]
pub enum LinearAlloc {
    /// Asked of the driver: `VASurfaceAttribDRMFormatModifiers` [LINEAR] with the VPP-write +
    /// export usage hint (iHD 26.3 ignores it and Y-tiles Y210 surfaces whatever is asked).
    Modifier,
    /// Memory from another allocator (the compositing GPU's), imported as a DRM PRIME 2 surface
    /// with the LINEAR modifier and 256-byte-aligned rows.
    Imported(DmaBufAlloc),
}

impl std::fmt::Debug for LinearAlloc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LinearAlloc::Modifier => "Modifier",
            LinearAlloc::Imported(_) => "Imported",
        })
    }
}

impl LinearAlloc {
    fn is_imported(&self) -> bool {
        matches!(self, LinearAlloc::Imported(_))
    }
}

/// A DMA-BUF mapped for CPU reads: `DMA_BUF_IOCTL_SYNC` (start, read) on creation, (end, read)
/// and unmap on drop.
struct Mapping<'a> {
    fd: &'a std::os::fd::OwnedFd,
    ptr: *mut std::ffi::c_void,
    len: usize,
}

/// `struct dma_buf_sync` and its ioctl (`linux/dma-buf.h`: `_IOW('b', 0, struct dma_buf_sync)`).
#[repr(C)]
struct DmaBufSync {
    flags: u64,
}
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_END: u64 = 4;

/// Largest DMA-BUF mapped (a 16K 4:2:2 16-bit picture is 1 GiB).
const MAX_MAPPING: u64 = 1 << 30;

impl<'a> Mapping<'a> {
    fn new(fd: &'a std::os::fd::OwnedFd) -> Result<Mapping<'a>, String> {
        let raw = fd.as_raw_fd();
        // SAFETY: seeking a valid open descriptor (DMA-BUFs report their size at the end).
        let end = unsafe { libc::lseek(raw, 0, libc::SEEK_END) };
        let size = u64::try_from(end).map_err(|_| format!("the DMA-BUF's size is unknown ({})", std::io::Error::last_os_error()))?;
        if size == 0 || size > MAX_MAPPING {
            return Err(format!("a DMA-BUF of {size} bytes"));
        }
        let len = usize::try_from(size).map_err(|_| "DMA-BUF size")?;
        // SAFETY: a new read-only shared mapping of `len` bytes of a valid descriptor at offset 0;
        // the kernel chooses the address. Checked for MAP_FAILED below.
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, raw, 0) };
        if ptr == libc::MAP_FAILED || ptr.is_null() {
            return Err(format!("mmap of the DMA-BUF: {}", std::io::Error::last_os_error()));
        }
        let m = Mapping { fd, ptr, len };
        m.sync(DMA_BUF_SYNC_READ)?;
        Ok(m)
    }

    fn sync(&self, flags: u64) -> Result<(), String> {
        let mut s = DmaBufSync { flags };
        // SAFETY: DMA_BUF_IOCTL_SYNC on a valid DMA-BUF descriptor with a valid `dma_buf_sync`.
        let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &mut s as *mut DmaBufSync) };
        if r != 0 { Err(format!("DMA_BUF_IOCTL_SYNC: {}", std::io::Error::last_os_error())) } else { Ok(()) }
    }

    /// Copy `out.len()` bytes at byte `start` of the mapping into `out`.
    fn copy_out(&self, start: u64, out: &mut [u8]) -> Result<(), String> {
        let start = usize::try_from(start).map_err(|_| "offset")?;
        if start.checked_add(out.len()).is_none_or(|e| e > self.len) {
            return Err(format!("{} bytes at {start} outside the {}-byte DMA-BUF", out.len(), self.len));
        }
        // SAFETY: `start..start + out.len()` lies in the mapping (checked above), which stays mapped
        // while `self` lives; the bytes are copied (no reference into memory another device could
        // write is formed) into `out`, which does not overlap the mapping.
        unsafe { std::ptr::copy_nonoverlapping((self.ptr as *const u8).add(start), out.as_mut_ptr(), out.len()) };
        Ok(())
    }
}

impl Drop for Mapping<'_> {
    fn drop(&mut self) {
        let _ = self.sync(DMA_BUF_SYNC_READ | DMA_BUF_SYNC_END);
        // SAFETY: unmaps the mapping made in `new`, once; no copy of the pointer outlives `self`.
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

/// Bytes per pixel of the first plane of a fourcc linear surfaces can be imported in: Y210
/// (packed 16-bit Y Cb Y Cr per two pixels: 4), P010 (2) and NV12 (1), whose interleaved chroma
/// plane at half height has rows of the same length (U0.2.3b d: 4:2:0 hand-off).
fn linear_bytes_per_pixel(fourcc: u32) -> Option<u32> {
    match fourcc {
        sys::VA_FOURCC_Y210 => Some(4),
        sys::VA_FOURCC_P010 => Some(2),
        sys::VA_FOURCC_NV12 => Some(1),
        _ => None,
    }
}

/// Rows of linear surfaces are a multiple of this many bytes, the row alignment AMD's GPUs
/// require of linear images they import.
const LINEAR_ROW_ALIGN: u32 = 256;

/// The width linear surfaces of `fourcc` are made at for `width`-pixel pictures: rows of a
/// [`LINEAR_ROW_ALIGN`] multiple (Y210: 64 pixels, P010: 128, NV12: 256).
fn linear_width(fourcc: u32, width: u32) -> Option<u32> {
    let align = LINEAR_ROW_ALIGN / linear_bytes_per_pixel(fourcc)?;
    width.div_ceil(align).checked_mul(align)
}

/// (bytes per row, planes, plane offsets, total bytes) of an imported linear `w`×`h` surface of
/// `fourcc` in one buffer object: Y210 one plane; NV12 / P010 the luma plane, then the
/// interleaved chroma plane at half height (rows of the same length).
fn linear_layout(fourcc: u32, w: u32, h: u32) -> Option<(u32, u32, [u32; 4], u32)> {
    let pitch = w.checked_mul(linear_bytes_per_pixel(fourcc)?)?;
    let luma = pitch.checked_mul(h)?;
    if fourcc == sys::VA_FOURCC_Y210 {
        return Some((pitch, 1, [0; 4], luma));
    }
    let chroma = pitch.checked_mul(h.div_ceil(2))?;
    Some((pitch, 2, [0, luma, 0, 0], luma.checked_add(chroma)?))
}

/// The video-processing context of `display`, made once and kept for the process: iHD keeps a
/// pointer to the last context that rendered into a surface, and syncing a pooled surface after
/// the video-processing context that wrote it was destroyed crashed inside vaSyncSurface (6.1b
/// hand-off, iHD 26.3), so these contexts are never destroyed. One copy at a time per display.
fn vpp_context(display: &Arc<Display>) -> Result<&'static Mutex<Context>, String> {
    static CONTEXTS: Mutex<Vec<(usize, &'static Mutex<Context>)>> = Mutex::new(Vec::new());
    let key = Arc::as_ptr(display) as usize;
    let mut g = CONTEXTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, c)) = g.iter().find(|(k, _)| *k == key) {
        return Ok(c);
    }
    if g.len() >= 8 {
        return Err("too many displays with video processing".into());
    }
    // no render-target format attribute: iHD's video processor refuses YUV422_10 there, and the
    // surfaces carry their formats; the context holds its config and display (kept alive with it)
    let config = Arc::new(Config::with_attribs(display, sys::VAProfile_VAProfileNone, sys::VAEntrypoint_VAEntrypointVideoProc, &[])?);
    let context: &'static Mutex<Context> = Box::leak(Box::new(Mutex::new(Context::new(&config, 0, 0)?)));
    g.push((key, context));
    Ok(context)
}

/// Copies decoded pictures into linear (untiled), exportable surfaces with the video processor of
/// their display (fork 6.1b, owner §1a.30): Intel's media engine decodes 4:2:2 into tiled
/// memory AMD's GPU cannot read, so each picture is copied once on Intel's side, synced and handed
/// over as a DMA-BUF instead of being read back to memory.
pub struct LinearCopy {
    context: &'static Mutex<Context>,
    pool: Arc<SurfacePool>,
    /// The visible size copied (the surfaces may be wider, [`linear_width`]).
    pub size: (u32, u32),
}

impl LinearCopy {
    /// A copier for `width`×`height` pictures of `fourcc` (render-target format `rt_format`) on
    /// `display`, or why it cannot be made (no video processor, surfaces refused).
    /// `import`: where linear memory comes from when the driver does not make linear surfaces
    /// itself ([`LinearAlloc::Imported`]).
    pub fn new(display: &Arc<Display>, rt_format: u32, fourcc: u32, width: u32, height: u32, import: Option<DmaBufAlloc>) -> Result<LinearCopy, String> {
        if !display.supports(sys::VAProfile_VAProfileNone, sys::VAEntrypoint_VAEntrypointVideoProc) {
            return Err(format!("the VA-API driver on {} has no video processor", display.node));
        }
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(format!("picture size {width}x{height}"));
        }
        let context = vpp_context(display)?;
        let padded = linear_width(fourcc, width).ok_or_else(|| format!("linear surfaces of fourcc {fourcc:#x}"))?;
        // the first way of asking that gives an exportable linear surface (probed on a surface of
        // a private pool, freed after; remembered per display, format and size)
        static FOUND: Mutex<Vec<(String, u32, (u32, u32), bool)>> = Mutex::new(Vec::new());
        let key = (display.node.clone(), fourcc, (padded, height));
        let ways: Vec<LinearAlloc> = std::iter::once(LinearAlloc::Modifier).chain(import.map(LinearAlloc::Imported)).collect();
        let known = FOUND.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|f| (&f.0, f.1, f.2) == (&key.0, key.1, key.2)).map(|f| f.3);
        if let Some(imported) = known
            && let Some(how) = ways.iter().find(|w| w.is_imported() == imported)
        {
            let pool = SurfacePool::private_linear(display, rt_format, fourcc, padded, height, how.clone());
            return Ok(LinearCopy { context, pool, size: (width, height) });
        }
        let mut tried = Vec::new();
        for how in ways {
            let probe = Arc::new(SurfacePool {
                display: display.clone(),
                rt_format,
                fourcc,
                size: (padded, height),
                linear: Some(how.clone()),
                free: Mutex::new(Vec::new()),
                count: Default::default(),
                upload: Mutex::new(None),
            });
            match probe.get().and_then(|s| s.export().map(|e| e.dmabuf.modifier)) {
                Ok(0) => {
                    log::info!("VA-API: linear {padded}x{height} surfaces of fourcc {fourcc:#x} on {} ({how:?})", display.node);
                    let mut found = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
                    if found.len() < 16 {
                        found.push((key.0, key.1, key.2, how.is_imported()));
                    }
                    let pool = SurfacePool::private_linear(display, rt_format, fourcc, padded, height, how);
                    return Ok(LinearCopy { context, pool, size: (width, height) });
                }
                Ok(m) => tried.push(format!("{how:?}: modifier {m:#x}")),
                Err(e) => tried.push(format!("{how:?}: {e}")),
            }
        }
        Err(format!("no linear surfaces on {} ({})", display.node, tried.join("; ")))
    }

    /// Whether the copies land in imported memory ([`LinearAlloc::Imported`]) rather than the
    /// driver's own linear surfaces.
    pub fn imported(&self) -> bool {
        self.pool.linear.as_ref().is_some_and(LinearAlloc::is_imported)
    }

    /// The rectangle `region` (x, y, w, h; at most [`LinearCopy::size`]) of `src` copied to the
    /// top-left of a linear surface, finished (synced) and exported: the result is ready for
    /// another device to read. An error leaves nothing pending (the surface goes back synced).
    pub fn copy(&self, src: &Surface, region: (u32, u32, u32, u32)) -> Result<Surface, String> {
        self.copy_scaled(src, region, (region.2, region.3))
    }

    /// [`LinearCopy::copy`] scaled to `out` (w, h; at most [`LinearCopy::size`]): the copier is
    /// made at the output size, `region` may be larger (U0.2.3b d: fewer bytes to hand off when
    /// the export frame is smaller than the source).
    pub fn copy_scaled(&self, src: &Surface, region: (u32, u32, u32, u32), out: (u32, u32)) -> Result<Surface, String> {
        if out.0 > self.size.0 || out.1 > self.size.1 {
            return Err(format!("a {}x{} copy for a {}x{} copier", out.0, out.1, self.size.0, self.size.1));
        }
        let target = self.pool.get()?;
        let clock = filmcraft_export::trace::start();
        let context = self.context.lock().unwrap_or_else(PoisonError::into_inner);
        filmcraft_export::trace::add(filmcraft_export::trace::DEC_COPY_LOCK, clock);
        let done = context.process_scaled(src, region, &target, out);
        drop(context);
        // whatever happened, nothing may still write into the surface once it is handed out or
        // returns to the pool
        let clock = filmcraft_export::trace::start();
        let synced = target.sync();
        filmcraft_export::trace::add(filmcraft_export::trace::DEC_COPY_SYNC, clock);
        done?;
        synced?;
        let e = target.export()?;
        if e.dmabuf.modifier != 0 {
            return Err(format!("the driver made a tiled surface (modifier {:#x}) where a linear one was asked for", e.dmabuf.modifier));
        }
        Ok(target)
    }
}

/// A libva buffer of a context, destroyed on drop (before the context: it borrows it, so a
/// destroyed buffer id can never hit a later buffer that reused it).
pub struct Buffer<'a> {
    context: &'a Context,
    id: sys::VABufferID,
}

impl Drop for Buffer<'_> {
    fn drop(&mut self) {
        let d = &self.context.display;
        // SAFETY: `id` was created on this display and is destroyed once (libva does not destroy
        // decode buffers in vaRenderPicture / vaEndPicture).
        unsafe { d.lib.vaDestroyBuffer(d.dpy, self.id) };
    }
}

/// Spare surfaces a pool keeps for the next pictures; surfaces returned beyond this are destroyed
/// (their memory goes back to the driver).
const SPARE_SURFACES: usize = 8;

/// Surfaces of one format and size, recycled: a [`Surface`] returns to the pool when its last
/// holder drops it; the pool keeps a few spare and destroys the rest, and destroys all of them
/// when the last surface and the pool are gone.
pub struct SurfacePool {
    display: Arc<Display>,
    pub rt_format: u32,
    pub fourcc: u32,
    pub size: (u32, u32),
    /// Surfaces are asked for untiled (`DRM_FORMAT_MOD_LINEAR`) this way, for export to another
    /// GPU, which cannot read the driver's tiling (fork 6.1b: Intel → AMD hand-off of 4:2:2
    /// pictures). Whether the driver honoured it shows in the export's modifier.
    pub linear: Option<LinearAlloc>,
    free: Mutex<Vec<(sys::VASurfaceID, Arc<SurfaceMem>)>>,
    count: std::sync::atomic::AtomicUsize,
    /// The image [`Surface::write`] fills and copies from, kept for the next frame (fork X422: a
    /// fresh 104 MB image per 6.2K 4:2:2 frame cost more than filling it). Destroyed with the pool.
    upload: Mutex<Option<sys::VAImage>>,
}

/// One surface's memory: a process-unique id (libva reuses surface ids after a surface is
/// destroyed, so ids alone cannot key caches), a token that dies with the memory, and its
/// DMA-BUF export, made once.
pub struct SurfaceMem {
    pub memory_id: u64,
    alive: Arc<()>,
    export: Mutex<Option<Arc<Export>>>,
}

impl SurfaceMem {
    /// Dead once the surface is destroyed (import caches drop their imports of it).
    pub fn liveness(&self) -> std::sync::Weak<()> {
        Arc::downgrade(&self.alive)
    }
}

/// A surface's memory exported as DMA-BUF (the description owns the fds).
pub struct Export {
    pub dmabuf: filmcraft_frame::DmaBuf,
    /// Exported for writing too (encoder input written by the GPU, fork Phase 5.2c).
    pub writable: bool,
}

/// A driver's `vaMaxNum*` count as an array length: libva writes up to that many entries, so a
/// count above `max` is refused rather than truncated (a smaller array would overflow).
fn cap(n: i32, max: usize, what: &str) -> Result<usize, String> {
    usize::try_from(n.max(0)).ok().filter(|&n| n <= max).ok_or_else(|| format!("the driver reports {n} {what} (at most {max} handled)"))
}

/// (rows, bytes per row) of each plane of a `w`×`h` image of `fourcc`: NV12 / P010 (luma, then
/// interleaved chroma at half height), Y210 (one packed plane: Y0 Cb Y1 Cr, 16 bits each, per
/// two pixels).
fn plane_rows(fourcc: u32, w: u32, h: u32) -> Result<Vec<(usize, usize)>, String> {
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    Ok(match fourcc {
        sys::VA_FOURCC_NV12 => vec![(h, w), (ch, cw * 2)],
        sys::VA_FOURCC_P010 => vec![(h, w * 2), (ch, cw * 4)],
        sys::VA_FOURCC_Y210 => vec![(h, cw * 8)],
        _ => return Err(format!("reading surfaces of fourcc {fourcc:#x}")),
    })
}

/// Whether the rectangle (x, y, w, h) is non-empty and lies inside a surface of `size`.
/// The VPP filter scaled copies use (U0.2.3b d): high quality unless the diagnostic
/// `FILMCRAFT_HANDOFF_FILTER=fast|default` says otherwise.
fn scaling_filter() -> u32 {
    static F: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *F.get_or_init(|| match std::env::var("FILMCRAFT_HANDOFF_FILTER").as_deref() {
        Ok("fast") => sys::VA_FILTER_SCALING_FAST,
        Ok("default") => sys::VA_FILTER_SCALING_DEFAULT,
        _ => sys::VA_FILTER_SCALING_HQ,
    })
}

/// Threads inside VA-API set-up work the process must not exit in the middle of ([`Busy`]).
static BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Set by [`wait_for_va_work`] when the process exits: no new display opens after it.
static EXITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// VA-API work the process must not exit in the middle of (U0.2.4): opening a display, the
/// start-up probes, each picture's submit (decode, video processing, encode), and the surface work
/// outside a submit (sync, image reads / writes, DMA-BUF export, coded-buffer maps). Exiting while a thread was inside Intel's driver initialisation ran the
/// driver's destructors under it and corrupted the heap (`filmcraft-cli help` aborted in a few
/// percent of runs); exit now waits for such work (up to 3 s, [`wait_for_va_work`]).
pub(crate) struct Busy(());

impl Busy {
    /// `None` once the process is exiting: start no new VA-API work then.
    pub(crate) fn enter() -> Option<Busy> {
        install_exit_wait();
        BUSY.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if EXITING.load(std::sync::atomic::Ordering::SeqCst) {
            BUSY.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        }
        Some(Busy(()))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// VA-API work refused because the process was exiting ([`Busy::enter`]).
static REFUSED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many VA-API operations (display opens, probes, picture submits) were refused because the
/// process was exiting (diagnostics and tests).
pub fn work_refused_while_exiting() -> usize {
    REFUSED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Run by `exit()` before the shared libraries' destructors: no new VA-API work, and the work in
/// flight finishes first (bounded: a hung driver can't keep the process alive).
extern "C" fn wait_for_va_work() {
    EXITING.store(true, std::sync::atomic::Ordering::SeqCst);
    let start = std::time::Instant::now();
    while BUSY.load(std::sync::atomic::Ordering::SeqCst) > 0 && start.elapsed() < std::time::Duration::from_secs(3) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Before the process exits: no new VA-API work, and the work in flight finishes first (up to
/// 3 s). Callers that exit early (a short CLI command) call it themselves, since libraries loaded
/// after the `atexit` hook was installed run their exit handlers before it.
pub fn quiesce() {
    wait_for_va_work();
}

fn install_exit_wait() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: registers a function without arguments that touches only atomics and sleeps;
        // `exit()` runs it once on the exiting thread.
        if unsafe { libc::atexit(wait_for_va_work) } != 0 {
            log::warn!("VA-API: no exit hook; exiting during driver set-up may crash");
        }
    });
}

/// Displays opened so far (diagnostics and tests): one per render node used, as `Display::get`,
/// `Display::for_node` and friends keep them for the life of the process.
static OPENED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The number of VA displays this process has opened.
pub fn displays_opened() -> usize {
    OPENED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a VA-API driver, by its vendor string, serialises its entry points per display (the
/// `Send` / `Sync` impls of [`Display`] rest on that): Mesa's Gallium frontend and Intel's iHD
/// media driver. Not the old i965 driver or nvidia-vaapi-driver (unchecked).
fn driver_is_thread_safe(vendor: &str) -> bool {
    vendor.starts_with("Mesa Gallium driver") || vendor.starts_with("Intel iHD driver")
}

/// Whether a render node is refused before it is opened: NVIDIA's (see [`Display::open_raw`]),
/// unless every driver is asked for.
fn skip_unopened(vendor: Option<u32>, any_driver: bool) -> bool {
    vendor == Some(VENDOR_NVIDIA) && !any_driver
}

/// PCI vendor ids of DRM render nodes.
const VENDOR_AMD: u32 = 0x1002;
const VENDOR_NVIDIA: u32 = 0x10de;
const VENDOR_INTEL: u32 = 0x8086;

/// The PCI vendor of a render node (`/sys/class/drm/renderD128/device/vendor`).
fn vendor(node: &str) -> Option<u32> {
    let name = std::path::Path::new(node).file_name()?.to_str()?;
    let s = std::fs::read_to_string(format!("/sys/class/drm/{name}/device/vendor")).ok()?;
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// What opening a display on each render node gave, failures included, so a node whose driver
/// is refused is not initialised again on every ask. A handful of GPUs at most: a hostile list of
/// names can't grow it without bound.
struct NodeCache<T> {
    entries: Vec<(String, Result<T, String>)>,
}

impl<T: Clone> NodeCache<T> {
    const MAX: usize = 16;

    const fn new() -> Self {
        NodeCache { entries: Vec::new() }
    }

    fn get_or_open(&mut self, node: &str, open: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if let Some((_, r)) = self.entries.iter().find(|(n, _)| n == node) {
            return r.clone();
        }
        if self.entries.len() >= Self::MAX {
            return Err("too many VA-API displays".into());
        }
        let r = open();
        self.entries.push((node.to_string(), r.clone()));
        r
    }
}

/// Render nodes in preference order (U0.2.4: here, not in the fork-only `ffmpeg.rs`): discrete GPUs first (AMD, NVIDIA, others), Intel last.
pub(crate) fn render_nodes() -> Vec<String> {
    let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("renderD"))
                .map(|n| format!("/dev/dri/{n}"))
                .collect()
        })
        .unwrap_or_default();
    nodes.sort();
    nodes.sort_by_key(|n| node_rank(vendor(n)));
    nodes
}

/// The order render nodes are tried in, by vendor (never by node number, which differs between
/// boots and laptops): AMD (Mesa radeonsi), Intel (iHD), other, NVIDIA last (its VA-API driver is
/// a wrapper around NVDEC, refused unless `FILMCRAFT_VAAPI_ANY_DRIVER=1`; on a hybrid laptop such
/// as the XPS 16 the Intel iGPU decodes, fork XPS a / c).
fn node_rank(vendor: Option<u32>) -> u8 {
    match vendor {
        Some(VENDOR_AMD) => 0,
        Some(VENDOR_INTEL) => 1,
        Some(VENDOR_NVIDIA) => 3,
        _ => 2,
    }
}

fn rect_fits((x, y, w, h): (u32, u32, u32, u32), size: (u32, u32)) -> bool {
    w > 0 && h > 0 && x.checked_add(w).is_some_and(|r| r <= size.0) && y.checked_add(h).is_some_and(|b| b <= size.1)
}

impl SurfacePool {
    /// The display the surfaces belong to.
    pub fn display(&self) -> &Arc<Display> {
        &self.display
    }
}

impl Drop for SurfacePool {
    fn drop(&mut self) {
        if let Some(image) = self.upload.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let d = &self.display;
            // SAFETY: the pool's cached upload image (made by vaCreateImage on this display, not
            // in use: it is only taken out of the slot while a write runs), destroyed once.
            unsafe { d.lib.vaDestroyImage(d.dpy, image.image_id) };
        }
        let free = std::mem::take(&mut *self.free.lock().unwrap_or_else(PoisonError::into_inner));
        for (id, _) in free {
            self.destroy(id);
        }
    }
}

/// Live surfaces in all pools (`perf.stats`).
static LIVE_SURFACES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn live_surfaces() -> usize {
    LIVE_SURFACES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pools kept for reuse across decoders (most recently used last): a decoder restarted for a clip
/// (or the next clip of the same format) takes surfaces from the spares instead of allocating.
const SHARED_POOLS: usize = 4;

impl SurfacePool {
    /// The process's pool for this format and size on `display` (made on first use; the
    /// [`SHARED_POOLS`] most recently used ones are kept alive with their spare surfaces).
    pub fn shared(display: &Arc<Display>, rt_format: u32, fourcc: u32, width: u32, height: u32) -> Arc<SurfacePool> {
        SurfacePool::shared_with(display, rt_format, fourcc, (width, height), None)
    }

    /// [`SurfacePool::shared`] for untiled surfaces, asked for as `how` ([`SurfacePool::linear`]).
    /// Untiled surfaces for one copier alone ([`LinearCopy`]): never handed to another copier.
    /// XPS 16 (iHD 26.3.5): a linear copy surface reused by the next stream's copier (same size)
    /// was not written by the video processor, so that stream's pictures read back as the
    /// previous stream's (`va_422_handoff`, `consecutive_streams_…`); a private pool, freed with
    /// its copier, never crosses streams.
    pub fn private_linear(display: &Arc<Display>, rt_format: u32, fourcc: u32, width: u32, height: u32, how: LinearAlloc) -> Arc<SurfacePool> {
        Arc::new(SurfacePool {
            display: display.clone(),
            rt_format,
            fourcc,
            size: (width, height),
            linear: Some(how),
            free: Mutex::new(Vec::new()),
            count: Default::default(),
            upload: Mutex::new(None),
        })
    }

    fn shared_with(display: &Arc<Display>, rt_format: u32, fourcc: u32, size: (u32, u32), linear: Option<LinearAlloc>) -> Arc<SurfacePool> {
        static POOLS: Mutex<Vec<Arc<SurfacePool>>> = Mutex::new(Vec::new());
        let mut g = POOLS.lock().unwrap_or_else(PoisonError::into_inner);
        let kind = |l: &Option<LinearAlloc>| l.as_ref().map(LinearAlloc::is_imported);
        let found = g.iter().position(|p| {
            Arc::ptr_eq(&p.display, display) && p.rt_format == rt_format && p.fourcc == fourcc && p.size == size && kind(&p.linear) == kind(&linear)
        });
        let pool = match found {
            Some(i) => g.remove(i),
            None => Arc::new(SurfacePool {
                display: display.clone(),
                rt_format,
                fourcc,
                size,
                linear,
                free: Mutex::new(Vec::new()),
                count: Default::default(),
                upload: Mutex::new(None),
            }),
        };
        g.push(pool.clone());
        if g.len() > SHARED_POOLS {
            g.remove(0);
        }
        pool
    }

    pub fn new(display: &Arc<Display>, rt_format: u32, fourcc: u32, width: u32, height: u32) -> Arc<SurfacePool> {
        Arc::new(SurfacePool {
            display: display.clone(),
            rt_format,
            fourcc,
            size: (width, height),
            linear: None,
            free: Mutex::new(Vec::new()),
            count: Default::default(),
            upload: Mutex::new(None),
        })
    }

    /// The spare surface returned longest ago, or a new one. Oldest first: a surface's last frame
    /// may still be sampled by GPU work already submitted when it comes back, and the decoder
    /// overwrites it on reuse; the longer it rests, the more surely that work has finished.
    pub fn get(self: &Arc<Self>) -> Result<Surface, String> {
        // (syncs a spare or creates a surface: driver work exit must wait for, U0.2 owed 4)
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        let spare = {
            let mut free = self.free.lock().unwrap_or_else(PoisonError::into_inner);
            if free.is_empty() { None } else { Some(free.remove(0)) }
        };
        if let Some((id, mem)) = spare {
            // A spare can come back with work pending (a picture dropped after a failed decode, a
            // DPB picture evicted before output): wait for it, so the next decode or write into
            // it never races that work (Intel has two video engines, fork 6.1b). Usually a no-op.
            let d = &self.display;
            // SAFETY: a valid surface of this display, not in use by any `Surface`.
            let st = unsafe { d.lib.vaSyncSurface(d.dpy, id) };
            if st == 0 {
                return Ok(Surface { id, mem, pool: self.clone() });
            }
            log::warn!("VA-API: a spare surface did not sync ({}); replaced", status_text(d.lib, st));
            self.destroy(id);
        }
        static MEMORY_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let d = &self.display;
        let mut id = sys::VA_INVALID_SURFACE;
        let int = |type_: sys::VASurfaceAttribType, i: i32| sys::VASurfaceAttrib {
            type_,
            flags: sys::VA_SURFACE_ATTRIB_SETTABLE,
            value: sys::VAGenericValue { type_: sys::VAGenericValueType_VAGenericValueTypeInteger, value: sys::_VAGenericValue__bindgen_ty_1 { i } },
        };
        // untiled: the one modifier allowed is LINEAR (0), for a surface written by the video
        // processor and exported
        let mut linear_modifier: u64 = 0;
        let mut modifiers = sys::VADRMFormatModifierList { num_modifiers: 1, modifiers: &mut linear_modifier };
        let mut attrs = vec![int(sys::VASurfaceAttribType_VASurfaceAttribPixelFormat, self.fourcc as i32)];
        // imported linear memory: one object, one layer (Y210 packed; NV12 / P010 luma then
        // chroma), rows of the (256-byte multiple) width; our descriptor is closed once the driver
        // has imported it
        let mut external = sys::VADRMPRIMESurfaceDescriptor::default();
        let mut _memory: Option<std::os::fd::OwnedFd> = None;
        if let Some(LinearAlloc::Imported(alloc)) = &self.linear {
            let (pitch, num_planes, offset, bytes) = linear_layout(self.fourcc, self.size.0, self.size.1)
                .ok_or_else(|| format!("importing {}x{} surfaces of fourcc {:#x}", self.size.0, self.size.1, self.fourcc))?;
            let fd = alloc(u64::from(bytes))?;
            let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
            _memory = Some(fd);
            external.fourcc = self.fourcc;
            external.width = self.size.0;
            external.height = self.size.1;
            external.num_objects = 1;
            external.objects[0] = sys::_VADRMPRIMESurfaceDescriptor__bindgen_ty_1 { fd: raw, size: bytes, drm_format_modifier: 0 };
            external.num_layers = 1;
            // the DRM fourccs of Y210, P010 and NV12 are the VA ones
            let pitches = if num_planes == 2 { [pitch, pitch, 0, 0] } else { [pitch, 0, 0, 0] };
            external.layers[0] =
                sys::_VADRMPRIMESurfaceDescriptor__bindgen_ty_2 { drm_format: self.fourcc, num_planes, object_index: [0; 4], offset, pitch: pitches };
            attrs.push(int(sys::VASurfaceAttribType_VASurfaceAttribMemoryType, sys::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 as i32));
            attrs.push(sys::VASurfaceAttrib {
                type_: sys::VASurfaceAttribType_VASurfaceAttribExternalBufferDescriptor,
                flags: sys::VA_SURFACE_ATTRIB_SETTABLE,
                value: sys::VAGenericValue {
                    type_: sys::VAGenericValueType_VAGenericValueTypePointer,
                    value: sys::_VAGenericValue__bindgen_ty_1 { p: (&mut external as *mut sys::VADRMPRIMESurfaceDescriptor).cast() },
                },
            });
        }
        if let Some(LinearAlloc::Modifier) = &self.linear {
            let hint = (sys::VA_SURFACE_ATTRIB_USAGE_HINT_VPP_WRITE | sys::VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT) as i32;
            attrs.push(int(sys::VASurfaceAttribType_VASurfaceAttribUsageHint, hint));
            attrs.push(sys::VASurfaceAttrib {
                type_: sys::VASurfaceAttribType_VASurfaceAttribDRMFormatModifiers,
                flags: sys::VA_SURFACE_ATTRIB_SETTABLE,
                value: sys::VAGenericValue {
                    type_: sys::VAGenericValueType_VAGenericValueTypePointer,
                    value: sys::_VAGenericValue__bindgen_ty_1 { p: (&mut modifiers as *mut sys::VADRMFormatModifierList).cast() },
                },
            });
        }
        let n = attrs.len() as u32;
        // SAFETY: one surface into a valid out-pointer; `n` valid attributes; the modifier list
        // (and the modifier it points to), the external descriptor and the memory's fd (owned by
        // `_memory`) live on this stack frame until the call returns; libva reads them only
        // during it (the driver imports the memory into its own handle and keeps no pointer).
        let st = unsafe { d.lib.vaCreateSurfaces(d.dpy, self.rt_format, self.size.0, self.size.1, &mut id, 1, attrs.as_mut_ptr(), n) };
        d.check(st, "vaCreateSurfaces")?;
        self.count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        LIVE_SURFACES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mem =
            Arc::new(SurfaceMem { memory_id: MEMORY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed), alive: Arc::new(()), export: Mutex::new(None) });
        Ok(Surface { id, mem, pool: self.clone() })
    }

    /// Surfaces alive (in use or spare).
    pub fn count(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn destroy(&self, mut id: sys::VASurfaceID) {
        // SAFETY: a surface of this display that no `Surface` refers to any more; destroyed once.
        unsafe { self.display.lib.vaDestroySurfaces(self.display.dpy, &mut id, 1) };
        self.count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        LIVE_SURFACES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// One surface of a pool; back to the pool on drop.
pub struct Surface {
    pub id: sys::VASurfaceID,
    mem: Arc<SurfaceMem>,
    pool: Arc<SurfacePool>,
}

impl Drop for Surface {
    fn drop(&mut self) {
        let mut free = self.pool.free.lock().unwrap_or_else(PoisonError::into_inner);
        if free.len() < SPARE_SURFACES {
            free.push((self.id, self.mem.clone()));
        } else {
            drop(free);
            // the memory (its liveness token and export fds) goes with the surface
            self.pool.destroy(self.id);
        }
    }
}

impl Surface {
    pub fn pool(&self) -> &Arc<SurfacePool> {
        &self.pool
    }

    /// The surface's memory: unique id, liveness.
    pub fn mem(&self) -> &Arc<SurfaceMem> {
        &self.mem
    }

    /// The surface's memory as DMA-BUF layers (luma, chroma; Y210: the one packed layer) for
    /// reading, exported once per surface.
    pub fn export(&self) -> Result<Arc<Export>, String> {
        self.export_with(false)
    }

    /// The surface's memory as DMA-BUF layers for reading and writing (an encoder's input that
    /// the GPU renders into), exported once per surface.
    pub fn export_writable(&self) -> Result<Arc<Export>, String> {
        self.export_with(true)
    }

    fn export_with(&self, writable: bool) -> Result<Arc<Export>, String> {
        if let Some(e) = self.mem.export.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            && (e.writable || !writable)
        {
            return Ok(e.clone());
        }
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        let d = &self.pool.display;
        let mut desc = sys::VADRMPRIMESurfaceDescriptor::default();
        let access = if writable { sys::VA_EXPORT_SURFACE_READ_WRITE } else { sys::VA_EXPORT_SURFACE_READ_ONLY };
        let (mem, flags) = (sys::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2, access | sys::VA_EXPORT_SURFACE_SEPARATE_LAYERS);
        let out = &mut desc as *mut sys::VADRMPRIMESurfaceDescriptor as *mut std::ffi::c_void;
        // SAFETY: a valid surface; `out` points to the VADRMPRIMESurfaceDescriptor that
        // VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 fills.
        let st = unsafe { d.lib.vaExportSurfaceHandle(d.dpy, self.id, mem, flags, out) };
        d.check(st, "vaExportSurfaceHandle")?;
        // take ownership of every object fd right away, so none leaks on an error below
        let nobj = (desc.num_objects as usize).min(desc.objects.len());
        let fds: Vec<std::os::fd::OwnedFd> = desc.objects[..nobj]
            .iter()
            .filter(|o| o.fd >= 0)
            // SAFETY: vaExportSurfaceHandle returns new fds owned by the caller.
            .map(|o| unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(o.fd) })
            .collect();
        if fds.len() != nobj || nobj == 0 {
            return Err("the export has no buffer objects".into());
        }
        // packed 4:2:2 (Y210) is one layer; the planar formats are luma and chroma
        let packed = self.pool.fourcc == sys::VA_FOURCC_Y210;
        let want = if packed { 1 } else { 2 };
        if desc.num_layers != want {
            return Err(format!("{} layers (expected {want})", desc.num_layers));
        }
        let modifier = desc.objects[0].drm_format_modifier;
        if desc.objects[..nobj].iter().any(|o| o.drm_format_modifier != modifier) {
            return Err("objects with different format modifiers".into());
        }
        let layer = |l: &sys::_VADRMPRIMESurfaceDescriptor__bindgen_ty_2| -> Result<Vec<filmcraft_frame::DmaBufPlane>, String> {
            let n = (l.num_planes as usize).min(4);
            (0..n)
                .map(|p| {
                    let obj = *l.object_index.get(p).ok_or("plane index")? as usize;
                    if obj >= nobj {
                        return Err("object index out of range".into());
                    }
                    Ok(filmcraft_frame::DmaBufPlane { object: obj, offset: l.offset[p], pitch: l.pitch[p] })
                })
                .collect()
        };
        let layers = [layer(&desc.layers[0])?, if packed { Vec::new() } else { layer(&desc.layers[1])? }];
        let dmabuf = filmcraft_frame::DmaBuf {
            layer_formats: [desc.layers[0].drm_format, if packed { 0 } else { desc.layers[1].drm_format }],
            modifier,
            layers,
            width: desc.width,
            height: desc.height,
            // the description owns the fds, so they stay open while any copy lives
            objects: fds.into(),
        };
        let e = Arc::new(Export { dmabuf, writable });
        *self.mem.export.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.clone());
        Ok(e)
    }

    /// Wait until the decode into this surface has finished.
    pub fn sync(&self) -> Result<(), String> {
        // a sync outside a picture submit is driver work too: exit waits for it (U0.2 owed 4)
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        let d = &self.pool.display;
        // SAFETY: a valid surface of this display.
        d.check(unsafe { d.lib.vaSyncSurface(d.dpy, self.id) }, "vaSyncSurface")
    }

    /// Fill the whole surface from memory: `f(image bytes, plane offsets, plane pitches)` writes
    /// the planes of an image of the pool's format and size (`P010` / `NV12`: luma, then
    /// interleaved chroma; `Y210`: one packed plane), which is then copied into the surface. The surface must not be in use
    /// by the GPU.
    pub fn write(&self, f: impl FnOnce(&mut [u8], [usize; 2], [usize; 2]) -> Result<(), String>) -> Result<(), String> {
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        let d = &self.pool.display;
        let l = d.lib;
        let (w, h) = self.pool.size;
        // (before the image exists: an early return must not leak it)
        let planes = plane_rows(self.pool.fourcc, w, h)?.len();
        // the pool's image from the last write, else a new one (another write holds it)
        let cached = self.pool.upload.lock().unwrap_or_else(PoisonError::into_inner).take();
        let image = match cached {
            Some(image) => image,
            None => {
                let mut format = self.image_format()?;
                let int = |v: u32| i32::try_from(v).map_err(|_| format!("{v} does not fit libva's int"));
                let (wi, hi) = (int(w)?, int(h)?);
                let mut image: sys::VAImage = Default::default();
                // SAFETY: a valid format struct and out-pointer.
                d.check(unsafe { l.vaCreateImage(d.dpy, &mut format, wi, hi, &mut image) }, "vaCreateImage")?;
                image
            }
        };
        let result = (|| {
            if (image.num_planes as usize) < planes || (image.width as u32) < w || (image.height as u32) < h {
                return Err("the driver made an image smaller than the surface".to_string());
            }
            let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
            // SAFETY: the image's buffer, mapped for writing into a valid out-pointer.
            d.check(unsafe { l.vaMapBuffer(d.dpy, image.buf, &mut ptr) }, "vaMapBuffer")?;
            if ptr.is_null() {
                return Err("vaMapBuffer returned no memory".to_string());
            }
            // SAFETY: the mapping covers `data_size` writable bytes until vaUnmapBuffer below, and
            // nothing else refers to it meanwhile.
            let bytes = unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, image.data_size as usize) };
            let filled = f(bytes, [image.offsets[0] as usize, image.offsets[1] as usize], [image.pitches[0] as usize, image.pitches[1] as usize]);
            // SAFETY: unmaps the mapping made above; `bytes` is not used after this.
            unsafe { l.vaUnmapBuffer(d.dpy, image.buf) };
            filled?;
            let _copy = d.image_copy.lock().unwrap_or_else(PoisonError::into_inner);
            // SAFETY: the image (as large as the surface, checked above) is copied into this
            // surface, which is of this display.
            d.check(unsafe { l.vaPutImage(d.dpy, self.id, image.image_id, 0, 0, w, h, 0, 0, w, h) }, "vaPutImage")
        })();
        // kept for the next write when it worked and the slot is free; else destroyed
        let mut slot = self.pool.upload.lock().unwrap_or_else(PoisonError::into_inner);
        if result.is_ok() && slot.is_none() {
            *slot = Some(image);
        } else {
            drop(slot);
            // SAFETY: destroys the image created or taken above, once (it is in no slot).
            unsafe { l.vaDestroyImage(d.dpy, image.image_id) };
        }
        result
    }

    /// The driver's image format for the pool's fourcc.
    fn image_format(&self) -> Result<sys::VAImageFormat, String> {
        let d = &self.pool.display;
        let l = d.lib;
        // SAFETY: initialised display.
        let max = cap(unsafe { l.vaMaxNumImageFormats(d.dpy) }, 256, "image formats")?;
        let mut formats: Vec<sys::VAImageFormat> = vec![Default::default(); max];
        let mut n = 0;
        // SAFETY: the array holds vaMaxNumImageFormats entries.
        d.check(unsafe { l.vaQueryImageFormats(d.dpy, formats.as_mut_ptr(), &mut n) }, "vaQueryImageFormats")?;
        formats.truncate((n.max(0) as usize).min(max));
        formats.into_iter().find(|f| f.fourcc == self.pool.fourcc).ok_or_else(|| "the driver has no image format for the surface".to_string())
    }

    /// [`Surface::read`] of a linear surface over imported memory: its DMA-BUF mapped for reading
    /// between `DMA_BUF_IOCTL_SYNC` start / end, each row of the rectangle copied out of the
    /// mapping (bounds-checked against the buffer's real size), plane by plane as
    /// [`Surface::read`] (Y210 one packed plane; NV12 / P010 luma, then chroma at half height).
    fn read_mapped(&self, (x, y, w, h): (u32, u32, u32, u32), mut f: impl FnMut(usize, usize, &[u8])) -> Result<(), String> {
        let fourcc = self.pool.fourcc;
        let bpp = u64::from(linear_bytes_per_pixel(fourcc).ok_or("mapped reads of this format")?);
        let rows = plane_rows(fourcc, w, h)?;
        // 4:2:0 chroma rows and pairs start on even luma coordinates
        if rows.len() > 1 && (x % 2 != 0 || y % 2 != 0) {
            return Err(format!("a 4:2:0 rectangle at odd ({x}, {y})"));
        }
        let e = self.export()?;
        let d = &e.dmabuf;
        if d.modifier != 0 {
            return Err(format!("mapping tiled memory (modifier {:#x})", d.modifier));
        }
        let planes: Vec<_> = d.layers.iter().flatten().collect();
        if planes.len() != rows.len() {
            return Err(format!("an export of {} planes ({} expected)", planes.len(), rows.len()));
        }
        let object = planes.first().and_then(|p| d.objects.get(p.object)).ok_or("plane of a missing buffer object")?;
        if planes.iter().any(|p| d.objects.get(p.object).is_none_or(|o| !std::ptr::eq(o, object))) {
            return Err("planes in several buffer objects".into());
        }
        let map = Mapping::new(object)?;
        for (i, (plane, &(count, len))) in planes.iter().zip(&rows).enumerate() {
            let (pitch, offset) = (u64::from(plane.pitch), u64::from(plane.offset));
            // chroma: half the rows, the same byte column (interleaved pairs at half width)
            let first = if i == 0 { u64::from(y) } else { u64::from(y / 2) };
            let mut row = vec![0u8; len];
            for r in 0..count {
                let start = first
                    .checked_add(r as u64)
                    .and_then(|o| o.checked_mul(pitch))
                    .and_then(|o| o.checked_add(offset))
                    .and_then(|o| o.checked_add(u64::from(x).checked_mul(bpp)?))
                    .ok_or("row offset overflows")?;
                map.copy_out(start, &mut row)?;
                f(i, r, &row);
            }
        }
        Ok(())
    }

    /// Copy the rectangle (x, y, w, h) of this surface to memory, as the image format `fourcc`
    /// (`P010` / `NV12` / `Y210`): `f(plane index, row index, row bytes)` for every row of the luma
    /// plane then the chroma plane (Y210: the one packed plane, x even). Syncs first.
    pub fn read(&self, rect: (u32, u32, u32, u32), mut f: impl FnMut(usize, usize, &[u8])) -> Result<(), String> {
        let (x, y, w, h) = rect;
        if !rect_fits(rect, self.pool.size) {
            return Err(format!("rectangle {w}x{h} at ({x}, {y}) outside the {}x{} surface", self.pool.size.0, self.pool.size.1));
        }
        // packed 4:2:2 holds a chroma pair per two pixels: the rectangle starts on a pair
        if self.pool.fourcc == sys::VA_FOURCC_Y210 && x % 2 != 0 {
            return Err(format!("a Y210 rectangle at odd x {x}"));
        }
        // held across the sync and the image copy / map (U0.2 owed 4)
        let _busy = Busy::enter().ok_or("the process is exiting")?;
        self.sync()?;
        // over imported (foreign) memory iHD's vaGetImage reads zeros: map the memory instead
        if matches!(self.pool.linear, Some(LinearAlloc::Imported(_))) {
            return self.read_mapped(rect, f);
        }
        let d = &self.pool.display;
        let l = d.lib;
        let fourcc = self.pool.fourcc;
        let rows = plane_rows(fourcc, w, h)?;
        let mut format = self.image_format()?;
        let int = |v: u32| i32::try_from(v).map_err(|_| format!("{v} does not fit libva's int"));
        let (xi, yi, wi, hi) = (int(x)?, int(y)?, int(w)?, int(h)?);
        // one CPU copy of this display at a time, through vaDestroyImage below
        let _copy = d.image_copy.lock().unwrap_or_else(PoisonError::into_inner);
        let mut image: sys::VAImage = Default::default();
        // SAFETY: a valid format struct and out-pointer.
        d.check(unsafe { l.vaCreateImage(d.dpy, &mut format, wi, hi, &mut image) }, "vaCreateImage")?;
        // a driver could hand back an image smaller than asked: the rows below are bounds-checked
        let result = (|| {
            // SAFETY: the image was created above with the requested size; the rectangle lies in the
            // surface (`rect_fits`).
            d.check(unsafe { l.vaGetImage(d.dpy, self.id, xi, yi, w, h, image.image_id) }, "vaGetImage")?;
            // checked before mapping: every return after vaMapBuffer goes through vaUnmapBuffer
            if (image.num_planes as usize) < rows.len() {
                return Err(format!("an image of {} planes ({} expected)", image.num_planes, rows.len()));
            }
            let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
            // SAFETY: the image's buffer, mapped for reading into a valid out-pointer.
            d.check(unsafe { l.vaMapBuffer(d.dpy, image.buf, &mut ptr) }, "vaMapBuffer")?;
            if ptr.is_null() {
                // SAFETY: the buffer was mapped (successfully, if without memory): unmap it.
                unsafe { l.vaUnmapBuffer(d.dpy, image.buf) };
                return Err("vaMapBuffer returned no memory".to_string());
            }
            // SAFETY: the mapping covers `data_size` bytes until vaUnmapBuffer below.
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, image.data_size as usize) };
            let mut ok = true;
            for (plane, &(nrows, row_bytes)) in rows.iter().enumerate() {
                let (off, pitch) = (image.offsets[plane] as usize, image.pitches[plane] as usize);
                for r in 0..nrows {
                    let start = r.checked_mul(pitch).and_then(|o| o.checked_add(off));
                    match start.and_then(|s| bytes.get(s..s.checked_add(row_bytes)?)) {
                        Some(row) => f(plane, r, row),
                        None => ok = false,
                    }
                }
            }
            // SAFETY: unmaps the mapping made above; `bytes` is not used after this.
            unsafe { l.vaUnmapBuffer(d.dpy, image.buf) };
            if ok { Ok(()) } else { Err("image smaller than its planes".to_string()) }
        })();
        // SAFETY: destroys the image created above, once.
        unsafe { l.vaDestroyImage(d.dpy, image.image_id) };
        result
    }
}

#[cfg(test)]
mod tests {
    /// XPS 16: a node whose display fails to open (nvidia-vaapi-driver, refused after a CUDA-loading
    /// vaInitialize) is opened once; later asks get the same error without opening it again, and
    /// a run of hostile names stays bounded.
    /// XPS 11: the profiles and entry points Revolution reads from the driver (`vaQueryConfigProfiles` /
    /// `vaQueryConfigEntrypoints` through our FFI) are exactly what `vainfo` lists for the same node:
    /// every capability is read from the driver, none assumed. `vainfo` is an external oracle, like
    /// ffmpeg; skips without it or without VA-API.
    #[test]
    fn driver_capabilities_match_vainfo() {
        let Ok(d) = Display::get() else {
            eprintln!("no VA-API; skipped");
            return;
        };
        let Ok(out) = std::process::Command::new("vainfo").args(["--display", "drm", "--device", &d.node]).output() else {
            eprintln!("no vainfo; skipped");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let profiles = [
            ("VAProfileNone", sys::VAProfile_VAProfileNone),
            ("VAProfileMPEG2Simple", sys::VAProfile_VAProfileMPEG2Simple),
            ("VAProfileMPEG2Main", sys::VAProfile_VAProfileMPEG2Main),
            ("VAProfileMPEG4Simple", sys::VAProfile_VAProfileMPEG4Simple),
            ("VAProfileMPEG4AdvancedSimple", sys::VAProfile_VAProfileMPEG4AdvancedSimple),
            ("VAProfileMPEG4Main", sys::VAProfile_VAProfileMPEG4Main),
            ("VAProfileH264Baseline", sys::VAProfile_VAProfileH264Baseline),
            ("VAProfileH264Main", sys::VAProfile_VAProfileH264Main),
            ("VAProfileH264High", sys::VAProfile_VAProfileH264High),
            ("VAProfileVC1Simple", sys::VAProfile_VAProfileVC1Simple),
            ("VAProfileVC1Main", sys::VAProfile_VAProfileVC1Main),
            ("VAProfileVC1Advanced", sys::VAProfile_VAProfileVC1Advanced),
            ("VAProfileH263Baseline", sys::VAProfile_VAProfileH263Baseline),
            ("VAProfileJPEGBaseline", sys::VAProfile_VAProfileJPEGBaseline),
            ("VAProfileH264ConstrainedBaseline", sys::VAProfile_VAProfileH264ConstrainedBaseline),
            ("VAProfileVP8Version0_3", sys::VAProfile_VAProfileVP8Version0_3),
            ("VAProfileH264MultiviewHigh", sys::VAProfile_VAProfileH264MultiviewHigh),
            ("VAProfileH264StereoHigh", sys::VAProfile_VAProfileH264StereoHigh),
            ("VAProfileHEVCMain", sys::VAProfile_VAProfileHEVCMain),
            ("VAProfileHEVCMain10", sys::VAProfile_VAProfileHEVCMain10),
            ("VAProfileVP9Profile0", sys::VAProfile_VAProfileVP9Profile0),
            ("VAProfileVP9Profile1", sys::VAProfile_VAProfileVP9Profile1),
            ("VAProfileVP9Profile2", sys::VAProfile_VAProfileVP9Profile2),
            ("VAProfileVP9Profile3", sys::VAProfile_VAProfileVP9Profile3),
            ("VAProfileHEVCMain12", sys::VAProfile_VAProfileHEVCMain12),
            ("VAProfileHEVCMain422_10", sys::VAProfile_VAProfileHEVCMain422_10),
            ("VAProfileHEVCMain422_12", sys::VAProfile_VAProfileHEVCMain422_12),
            ("VAProfileHEVCMain444", sys::VAProfile_VAProfileHEVCMain444),
            ("VAProfileHEVCMain444_10", sys::VAProfile_VAProfileHEVCMain444_10),
            ("VAProfileHEVCMain444_12", sys::VAProfile_VAProfileHEVCMain444_12),
            ("VAProfileHEVCSccMain", sys::VAProfile_VAProfileHEVCSccMain),
            ("VAProfileHEVCSccMain10", sys::VAProfile_VAProfileHEVCSccMain10),
            ("VAProfileHEVCSccMain444", sys::VAProfile_VAProfileHEVCSccMain444),
            ("VAProfileAV1Profile0", sys::VAProfile_VAProfileAV1Profile0),
            ("VAProfileAV1Profile1", sys::VAProfile_VAProfileAV1Profile1),
            ("VAProfileHEVCSccMain444_10", sys::VAProfile_VAProfileHEVCSccMain444_10),
            ("VAProfileProtected", sys::VAProfile_VAProfileProtected),
            ("VAProfileH264High10", sys::VAProfile_VAProfileH264High10),
            ("VAProfileVVCMain10", sys::VAProfile_VAProfileVVCMain10),
            ("VAProfileVVCMultilayerMain10", sys::VAProfile_VAProfileVVCMultilayerMain10),
            ("VAProfileAV1Profile2", sys::VAProfile_VAProfileAV1Profile2),
            ("VAProfileH264High422", sys::VAProfile_VAProfileH264High422),
        ];
        let entrypoints = [
            ("VAEntrypointVLD", sys::VAEntrypoint_VAEntrypointVLD),
            ("VAEntrypointIZZ", sys::VAEntrypoint_VAEntrypointIZZ),
            ("VAEntrypointIDCT", sys::VAEntrypoint_VAEntrypointIDCT),
            ("VAEntrypointMoComp", sys::VAEntrypoint_VAEntrypointMoComp),
            ("VAEntrypointDeblocking", sys::VAEntrypoint_VAEntrypointDeblocking),
            ("VAEntrypointEncSlice", sys::VAEntrypoint_VAEntrypointEncSlice),
            ("VAEntrypointEncPicture", sys::VAEntrypoint_VAEntrypointEncPicture),
            ("VAEntrypointEncSliceLP", sys::VAEntrypoint_VAEntrypointEncSliceLP),
            ("VAEntrypointVideoProc", sys::VAEntrypoint_VAEntrypointVideoProc),
            ("VAEntrypointFEI", sys::VAEntrypoint_VAEntrypointFEI),
            ("VAEntrypointStats", sys::VAEntrypoint_VAEntrypointStats),
            ("VAEntrypointProtectedTEEComm", sys::VAEntrypoint_VAEntrypointProtectedTEEComm),
            ("VAEntrypointProtectedContent", sys::VAEntrypoint_VAEntrypointProtectedContent),
        ];
        let mut theirs: Vec<(sys::VAProfile, sys::VAEntrypoint)> = text
            .lines()
            .filter_map(|l| {
                let (p, e) = l.split_once(':')?;
                let p = profiles.iter().find(|(n, _)| *n == p.trim())?.1;
                let e = entrypoints.iter().find(|(n, _)| *n == e.trim())?.1;
                Some((p, e))
            })
            .collect();
        let mut ours: Vec<(sys::VAProfile, sys::VAEntrypoint)> = d.profiles.iter().flat_map(|(p, es)| es.iter().map(move |e| (*p, *e))).collect();
        theirs.sort_unstable();
        ours.sort_unstable();
        assert!(!theirs.is_empty(), "vainfo listed nothing for {}:\n{text}", d.node);
        assert_eq!(ours, theirs, "{} ({})", d.node, d.vendor);
        eprintln!("{} ({}): {} profile / entry point pairs, as vainfo lists them", d.node, d.vendor, ours.len());
    }

    /// XPS 16: NVIDIA's render node is refused from sysfs, never opened (opening it loads CUDA and
    /// wakes the dGPU), unless FILMCRAFT_VAAPI_ANY_DRIVER=1; other makers' nodes are opened.
    #[test]
    fn nvidia_nodes_are_refused_without_opening_them() {
        assert!(super::skip_unopened(Some(super::VENDOR_NVIDIA), false));
        assert!(!super::skip_unopened(Some(super::VENDOR_NVIDIA), true));
        assert!(!super::skip_unopened(Some(super::VENDOR_INTEL), false));
        assert!(!super::skip_unopened(Some(super::VENDOR_AMD), false));
        assert!(!super::skip_unopened(None, false));
    }

    #[test]
    fn failed_nodes_are_not_opened_again() {
        let mut cache = super::NodeCache::<u32>::new();
        let mut opens = 0;
        for _ in 0..5 {
            let r = cache.get_or_open("/dev/dri/renderD129", || {
                opens += 1;
                Err("/dev/dri/renderD129: VA-API driver not supported".into())
            });
            assert!(r.is_err());
        }
        assert_eq!(opens, 1, "the refused node is opened once");
        assert_eq!(cache.get_or_open("/dev/dri/renderD128", || Ok(7)), Ok(7));
        assert_eq!(cache.get_or_open("/dev/dri/renderD128", || panic!("opened twice")), Ok(7));
        for i in 0..100 {
            let _ = cache.get_or_open(&format!("/dev/dri/hostile{i}"), || Err("no".into()));
        }
        assert!(cache.entries.len() <= 16, "bounded");
        assert_eq!(cache.get_or_open("/dev/dri/renderD128", || panic!("opened again")), Ok(7), "known nodes stay");
    }

    /// Fork XPS a: the node order is by vendor, so the XPS 16 (Intel iGPU renderD128, RTX 4050
    /// renderD129, or the other way round after a reboot) tries Intel first, and this desktop
    /// (AMD renderD129, Intel renderD128) tries AMD first.
    #[test]
    fn render_nodes_are_ranked_by_vendor_not_node_number() {
        let order = |mut v: Vec<(&'static str, Option<u32>)>| {
            v.sort_by_key(|(n, _)| *n);
            v.sort_by_key(|(_, vendor)| super::node_rank(*vendor));
            v.into_iter().map(|(n, _)| n).collect::<Vec<_>>()
        };
        let (amd, intel, nvidia) = (Some(super::VENDOR_AMD), Some(super::VENDOR_INTEL), Some(super::VENDOR_NVIDIA));
        assert_eq!(order(vec![("renderD128", nvidia), ("renderD129", intel)]), ["renderD129", "renderD128"]);
        assert_eq!(order(vec![("renderD128", intel), ("renderD129", nvidia)]), ["renderD128", "renderD129"]);
        assert_eq!(order(vec![("renderD128", intel), ("renderD129", amd)]), ["renderD129", "renderD128"]);
        assert_eq!(order(vec![("renderD130", None), ("renderD128", nvidia), ("renderD129", intel)]), ["renderD129", "renderD130", "renderD128"]);
    }

    #[test]
    fn only_drivers_checked_for_thread_safety_are_shared() {
        use super::driver_is_thread_safe as ok;
        assert!(ok("Mesa Gallium driver 26.2.4-arch3.1 for AMD Radeon RX 6700 XT (radeonsi, navi22, ACO)"));
        assert!(ok("Intel iHD driver for Intel(R) Gen Graphics - 26.3.5 ()"));
        assert!(!ok("Intel i965 driver for Intel(R) Haswell Desktop - 2.4.1"));
        assert!(!ok("VA-API NVDEC driver [direct backend]"));
        assert!(!ok(""));
    }

    use super::{Config, Context, Display, SurfacePool, rect_fits, sys};
    use std::sync::Arc;

    /// Unsafe-code review round 3: a context decodes only into surfaces of its display that hold a
    /// whole picture (the driver would write past a smaller one). Skipped without VA-API.
    #[test]
    fn a_context_refuses_a_smaller_target_surface() {
        let Ok(d) = Display::get() else {
            eprintln!("no VA-API; skipped");
            return;
        };
        let (profile, vld) = (sys::VAProfile_VAProfileHEVCMain, sys::VAEntrypoint_VAEntrypointVLD);
        if !d.supports(profile, vld) {
            eprintln!("no HEVC decode; skipped");
            return;
        }
        let config = Arc::new(Config::new(&d, profile, vld, sys::VA_RT_FORMAT_YUV420).expect("config"));
        let ctx = Context::new(&config, 256, 256).expect("context");
        let small = SurfacePool::new(&d, sys::VA_RT_FORMAT_YUV420, sys::VA_FOURCC_NV12, 128, 256).get().expect("surface");
        let err = ctx.decode(&small, &[]).expect_err("a smaller target is refused");
        assert!(err.contains("128x256"), "{err}");
        let fits = SurfacePool::new(&d, sys::VA_RT_FORMAT_YUV420, sys::VA_FOURCC_NV12, 256, 256).get().expect("surface");
        // passes the check (the driver may still refuse an empty picture)
        if let Err(e) = ctx.decode(&fits, &[]) {
            assert!(!e.contains("target surface"), "{e}");
        }
    }

    /// XPS 16 crash: with the window on the NVIDIA GPU (which can't import Intel's tiled pictures),
    /// the compositor and a frame worker read the same decoded picture back at once, and iHD's
    /// vaGetImage segfaulted in libigdgmm's CPU copy. Reads of one display are serialised: many
    /// threads reading one surface at once all get its pixels, no crash.
    #[test]
    fn concurrent_reads_of_one_surface_never_crash() {
        let Ok(d) = Display::get() else {
            eprintln!("no VA-API; skipped");
            return;
        };
        let Ok(surface) = SurfacePool::new(&d, sys::VA_RT_FORMAT_YUV420, sys::VA_FOURCC_NV12, 1920, 1088).get() else {
            eprintln!("no NV12 surface; skipped");
            return;
        };
        let surface = Arc::new(surface);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let s = surface.clone();
                std::thread::spawn(move || {
                    for _ in 0..40 {
                        let mut rows = 0;
                        s.read((0, 0, 1920, 1088), |_, _, _| rows += 1).expect("read");
                        assert_eq!(rows, 1088 + 544);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a reading thread");
        }
    }

    #[test]
    fn read_rectangles_must_lie_in_the_surface() {
        assert!(rect_fits((0, 0, 64, 32), (64, 32)));
        assert!(rect_fits((8, 4, 56, 28), (64, 32)));
        assert!(!rect_fits((0, 0, 0, 32), (64, 32)));
        assert!(!rect_fits((1, 0, 64, 32), (64, 32)));
        assert!(!rect_fits((0, 0, 64, 33), (64, 32)));
        assert!(!rect_fits((u32::MAX, 0, 2, 1), (64, 32)));
        assert!(!rect_fits((0, 0, u32::MAX, 1), (64, 32)));
    }
}

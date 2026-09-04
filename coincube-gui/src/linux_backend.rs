//! Windowing-backend selection on Linux.
//!
//! winit 0.30 has no libdecor support: on Wayland it asks the compositor for
//! server-side decorations and, failing that (GNOME/mutter never grants them),
//! falls back to its own `sctk-adwaita` client-side title bar, which has known
//! drag/move bugs on GNOME — the unmovable window on Ubuntu. Under XWayland the
//! window manager draws the frame itself, the same path every other app on the
//! desktop already uses, so we prefer it when we can.
//!
//! winit dropped `WINIT_UNIX_BACKEND` in 0.29 "in favor of standard
//! `WAYLAND_DISPLAY` and `DISPLAY` variables", so clearing the Wayland
//! variables is the only supported way left to ask for X11.
//!
//! The trade-off is real and not right for everyone: XWayland has no fractional
//! scaling, so on a display scaled to 125%/150% (common on Ubuntu) the window is
//! upscaled and looks soft, and per-monitor DPI changes are not picked up.
//! `COINCUBE_LINUX_BACKEND=wayland` opts back out for those users, and
//! `COINCUBE_LINUX_BACKEND=x11` forces X11 past the pre-flight check below.
//!
//! That pre-flight check has to be conclusive, because there is no second
//! chance: iced 0.14 builds the event loop with `expect("Create event loop")`,
//! so a winit backend that cannot start is a panic, and `panic = "abort"` in
//! the release profiles rules out catching it. Once the Wayland variables are
//! gone we are committed, so we only remove them after confirming that
//! everything winit's X11 backend needs is present *and* that the display
//! actually answers.

use std::{
    ffi::{CString, OsString},
    sync::OnceLock,
};

/// The variables winit consults to decide Wayland is present, in its own order:
/// `WAYLAND_DISPLAY` first, then `WAYLAND_SOCKET`, which a compositor sets when
/// it hands the client a pre-opened socket instead of a name. Clearing only the
/// first leaves winit on Wayland, so both have to go.
const WAYLAND_VARS: [&str; 2] = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"];

/// `libX11`, under the sonames x11-dl asks for. Split out from the rest because
/// the display probe needs a handle to it.
const XLIB_SONAMES: &[&str] = &["libX11.so.6", "libX11.so"];

/// Everything else winit's X11 backend loads at runtime, under the sonames its
/// loaders ask for. `libX11-xcb`, `libXcursor` and `libXi` are opened by x11-dl
/// in `XConnection::new`; `libxkbcommon-x11` is opened by xkbcommon-dl and
/// `.expect()`ed during window creation. `libxcb` is not listed: both `libX11`
/// and `libxkbcommon-x11` link it, so a successful `dlopen` of either already
/// proves it resolved.
const OTHER_X11_SONAMES: &[&[&str]] = &[
    &["libX11-xcb.so.1", "libX11-xcb.so"],
    &["libXcursor.so.1", "libXcursor.so"],
    &["libXi.so.6", "libXi.so"],
    &["libxkbcommon-x11.so.0", "libxkbcommon-x11.so"],
];

/// What we took out of the environment, so children can be given it back.
/// Empty when we left Wayland alone, which is also what non-Linux-desktop and
/// already-X11 sessions get.
static REMOVED_WAYLAND_VARS: OnceLock<Vec<(&'static str, OsString)>> = OnceLock::new();

/// Steers winit onto XWayland when that is both possible and wanted. Call once,
/// at the very top of `main`: this mutates the environment, which is only sound
/// while the process is still single-threaded.
pub fn prefer_x11_over_wayland() {
    let forced = std::env::var("COINCUBE_LINUX_BACKEND").unwrap_or_default();

    // Only worth switching if there is an X server to switch to, we are
    // actually on Wayland, and winit's X11 backend would come up.
    let auto = || {
        is_set("DISPLAY") && WAYLAND_VARS.iter().any(|name| is_set(name)) && x11_backend_usable()
    };

    let use_x11 = match forced.to_ascii_lowercase().as_str() {
        // Explicit opt-out: keep native Wayland, undecorated title bar and all,
        // in exchange for sharp rendering on a fractionally-scaled display.
        "wayland" => false,
        // Forcing X11 skips the library and display probes, not the `DISPLAY`
        // check — without a display set there is no X11 backend to force.
        "x11" => is_set("DISPLAY"),
        "" | "auto" => auto(),
        other => {
            eprintln!(
                "COINCUBE_LINUX_BACKEND: ignoring unknown value '{}' \
                 (expected 'auto', 'x11' or 'wayland')",
                other
            );
            auto()
        }
    };

    let removed = if use_x11 {
        WAYLAND_VARS
            .iter()
            .filter_map(|name| {
                let value = std::env::var_os(name)?;
                std::env::remove_var(name);
                Some((*name, value))
            })
            .collect()
    } else {
        Vec::new()
    };

    let _ = REMOVED_WAYLAND_VARS.set(removed);
}

/// The Wayland variables a child process should see. Clearing them from our own
/// environment is inherited by everything we spawn, which would drag a browser
/// launched from the app onto XWayland too — blurry on a scaled display, and
/// not a choice we get to make on its behalf. Empty once nothing needs putting
/// back, so callers can skip the whole dance.
pub fn wayland_env_for_children() -> Vec<(&'static str, OsString)> {
    REMOVED_WAYLAND_VARS
        .get()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|(name, _)| std::env::var_os(name).is_none())
        .cloned()
        .collect()
}

/// winit treats a variable that is set but empty as not set at all. Mirror that
/// exactly: reading `DISPLAY=""` as an available X server would have us clear
/// the Wayland variables and leave winit with no backend to pick.
fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Whether winit's X11 backend would actually come up: every library it loads
/// at runtime resolves, and `DISPLAY` names a server that answers.
fn x11_backend_usable() -> bool {
    let Some(xlib) = dlopen_any(XLIB_SONAMES) else {
        return false;
    };

    OTHER_X11_SONAMES
        .iter()
        .all(|sonames| dlopen_any(sonames).is_some())
        && x_display_reachable(xlib)
}

/// Opens and closes an X display the way winit's `XConnection::new` does. A
/// `DISPLAY` that is set but unreachable — a stale value inherited from an old
/// session, or an XWayland that failed to start — passes every library check
/// and then takes winit all the way to its panic.
///
/// `XInitThreads` comes first, in winit's own order. On libX11 before 1.8 it
/// installs the global locking hooks and is documented as having to be the
/// first Xlib call in the process, so probing without it would leave winit's
/// later call too late to take effect.
fn x_display_reachable(xlib: *mut libc::c_void) -> bool {
    type XInitThreads = unsafe extern "C" fn() -> libc::c_int;
    type XOpenDisplay = unsafe extern "C" fn(*const libc::c_char) -> *mut libc::c_void;
    type XCloseDisplay = unsafe extern "C" fn(*mut libc::c_void) -> libc::c_int;

    // SAFETY: `xlib` is a live handle from `dlopen_any`, which we never close,
    // so the symbols outlive their use. Each is transmuted to the signature
    // libX11 declares for it, and `XOpenDisplay(NULL)` reads `DISPLAY` itself,
    // exactly as winit's call does.
    unsafe {
        let init_threads = libc::dlsym(xlib, b"XInitThreads\0".as_ptr().cast());
        let open = libc::dlsym(xlib, b"XOpenDisplay\0".as_ptr().cast());
        let close = libc::dlsym(xlib, b"XCloseDisplay\0".as_ptr().cast());
        if init_threads.is_null() || open.is_null() || close.is_null() {
            return false;
        }

        let init_threads: XInitThreads = std::mem::transmute(init_threads);
        let open: XOpenDisplay = std::mem::transmute(open);
        let close: XCloseDisplay = std::mem::transmute(close);

        if init_threads() == 0 {
            return false;
        }

        let display = open(std::ptr::null());
        if display.is_null() {
            return false;
        }
        let _ = close(display);
        true
    }
}

/// `dlopen`s the first soname that resolves, returning the handle.
///
/// The handle is deliberately never closed. Unloading a library that has
/// registered TLS destructors or `atexit` handlers is the classic `dlclose`
/// hazard, and there is nothing to gain here: when this succeeds we take the
/// X11 path and winit loads the very same libraries moments later, and when it
/// fails nothing was mapped in the first place.
fn dlopen_any(sonames: &[&str]) -> Option<*mut libc::c_void> {
    sonames.iter().find_map(|soname| {
        let name = CString::new(*soname).ok()?;
        // SAFETY: `name` is a valid nul-terminated C string that outlives the
        // call, and the flags are the pair x11-dl itself opens these with.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if handle.is_null() {
            None
        } else {
            Some(handle)
        }
    })
}

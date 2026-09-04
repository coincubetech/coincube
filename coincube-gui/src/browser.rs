//! Opening a URL in the user's browser.
//!
//! A thin wrapper over `open::that_detached` that exists for one reason: on
//! Linux we may have cleared the Wayland variables to put ourselves on XWayland
//! (see [`crate::linux_backend`]), and a child process inherits that. A browser
//! that is not already running would then start under XWayland as well — blurry
//! on a scaled display, and not our call to make. Hand the variables back to
//! the child so it picks its own backend.

use std::io;

/// Opens `url` in the user's browser, detached from this process.
pub fn open_url(url: &str) -> io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        open::that_detached(url)
    }

    #[cfg(target_os = "linux")]
    {
        let wayland_env = crate::linux_backend::wayland_env_for_children();
        if wayland_env.is_empty() {
            // Nothing to restore — the child would inherit the environment
            // `open` expects anyway, so let it do the whole job.
            return open::that_detached(url);
        }

        let mut last_err = None;
        for mut command in open::commands(url) {
            command.envs(wayland_env.iter().map(|(name, value)| (*name, value)));
            match spawn_detached(&mut command) {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no URL launcher available")
        }))
    }
}

/// `open::that_detached`'s own double-fork, reimplemented because `open` only
/// applies it to commands it builds and spawns itself, with no seam for adding
/// environment first.
#[cfg(target_os = "linux")]
fn spawn_detached(command: &mut std::process::Command) -> io::Result<()> {
    use std::{os::unix::process::CommandExt, process::Stdio};

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: `fork`, `_exit` and `setsid` are all async-signal-safe, which is
    // what `pre_exec` requires of this closure. The sequence is the one `open`
    // uses: fork so the browser is reparented away from us, and `setsid` so it
    // outlives the app and does not receive our terminal's signals.
    unsafe {
        command.pre_exec(|| {
            match libc::fork() {
                -1 => return Err(io::Error::last_os_error()),
                0 => (),
                _ => libc::_exit(0),
            }

            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }

            Ok(())
        });
    }

    // The intermediate child `_exit`s the moment it has forked, so this reaps
    // immediately rather than waiting on the browser.
    command.spawn()?.wait().map(|_| ())
}

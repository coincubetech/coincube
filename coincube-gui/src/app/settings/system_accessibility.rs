//! Read-only platform input for resolving device motion preferences.

/// `None` means this host cannot provide a reduced-motion observation.
#[cfg(all(target_os = "macos", not(test)))]
pub fn reduces_motion() -> Option<bool> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    // NSWorkspace exposes the current Accessibility → Display choice. Called
    // from the iced GUI thread at startup and on its existing one-second tick.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        (!workspace.is_null()).then(|| msg_send![workspace, accessibilityDisplayShouldReduceMotion])
    }
}

#[cfg(any(not(target_os = "macos"), test))]
pub fn reduces_motion() -> Option<bool> {
    None
}

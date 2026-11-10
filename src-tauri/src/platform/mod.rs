//! Platform-specific focus verification and text-only delivery.
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "linux")]
pub mod portal_parent;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "linux")]
pub mod x11_type;

#[cfg(target_os = "linux")]
pub use linux::Destination;
#[cfg(target_os = "macos")]
pub use macos::Destination;
#[cfg(target_os = "windows")]
pub use windows::Destination;

#[derive(serde::Serialize)]
pub struct DesktopStatus {
    pub supported: bool,
    pub keyboard_ready: bool,
    pub permission_required: bool,
    pub platform: &'static str,
    pub message: String,
}

pub fn status() -> DesktopStatus {
    #[cfg(target_os = "linux")]
    {
        let wayland = portal::wayland();
        if wayland {
            let ready = portal::keyboard_ready();
            DesktopStatus {
                supported: true,
                keyboard_ready: ready,
                permission_required: !ready,
                platform: "Linux / Wayland",
                message: if ready {
                    "Accessibility ready. Electron/browser editors and terminals use authorized keyboard input; no Enter is sent.".into()
                } else {
                    "Keyboard access is required for Electron/browser editors and terminals. Choose Grant access and approve the system dialog.".into()
                },
            }
        } else {
            // X11 cannot show a system permission dialog; Arxell's own opt-in
            // is the only consent that exists there and must be explicit.
            let consented = x11_type::consented();
            let ready = consented && x11_type::available();
            DesktopStatus {
                supported: true,
                keyboard_ready: ready,
                permission_required: !consented,
                platform: "Linux / X11",
                message: if ready {
                    "X11 keyboard delivery enabled via XTest. Focus is verified before each chunk; no Enter is sent.".into()
                } else if !consented {
                    "X11 sessions have no system keyboard permission. Enable 'X11 keyboard delivery' in Settings → Shortcuts & delivery to type into other applications.".into()
                } else {
                    "This display does not provide the XTest keyboard extension; external typing is unavailable here.".into()
                },
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        macos::status()
    }
    #[cfg(target_os = "windows")]
    {
        windows::status()
    }
}

pub fn set_x11_keyboard(enabled: bool) {
    #[cfg(target_os = "linux")]
    x11_type::set_consent(enabled);
    #[cfg(not(target_os = "linux"))]
    let _ = enabled;
}

pub fn initialize_desktop() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::enable_accessibility()?;
        linux::start_focus_monitor()?;
    }
    Ok(())
}

pub fn prepare() -> Result<(), String> {
    prepare_with_parent("")
}

pub fn prepare_with_parent(_parent: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        initialize_desktop()?;
        if portal::wayland() {
            portal::prepare_keyboard_with_parent(_parent)
        } else if x11_type::consented() && !x11_type::available() {
            Err("This display does not provide the XTest keyboard extension; external typing is unavailable here.".into())
        } else {
            // X11 has no dialog to show; readiness is reported through status.
            Ok(())
        }
    }
    #[cfg(target_os = "macos")]
    {
        macos::prepare()
    }
    #[cfg(target_os = "windows")]
    {
        windows::prepare()
    }
}

pub fn modifiers_down() -> bool {
    #[cfg(target_os = "linux")]
    {
        // Wayland intentionally does not expose other applications' key state.
        // The activation shortcut itself is tracked separately by the host.
        if portal::wayland() {
            return false;
        }
        let Ok(x) = x11_dl::xlib::Xlib::open() else {
            return true;
        };
        // SAFETY: each query owns its display connection; all output pointers
        // refer to initialized local storage. No Xlib pointer escapes.
        unsafe {
            let display = (x.XOpenDisplay)(std::ptr::null());
            if display.is_null() {
                return true;
            }
            let root = (x.XDefaultRootWindow)(display);
            let (mut rr, mut child, mut rx, mut ry, mut wx, mut wy, mut mask) =
                (0, 0, 0, 0, 0, 0, 0);
            let ok = (x.XQueryPointer)(
                display, root, &mut rr, &mut child, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask,
            );
            (x.XCloseDisplay)(display);
            ok == 0
                || mask
                    & (x11_dl::xlib::ShiftMask
                        | x11_dl::xlib::ControlMask
                        | x11_dl::xlib::Mod1Mask
                        | x11_dl::xlib::Mod4Mask
                        | x11_dl::xlib::Mod5Mask)
                    != 0
        }
    }
    #[cfg(target_os = "macos")]
    {
        macos::modifiers_down()
    }
    #[cfg(target_os = "windows")]
    {
        windows::modifiers_down()
    }
}

//! Explicitly consented X11 keyboard delivery via the server XTest extension.
//!
//! X11 has no operating-system permission mechanism: any client may inject
//! input. Arxell therefore refuses to synthesize keys until the user enables
//! "X11 keyboard delivery" in Settings; that opt-in is the only consent X11
//! can express. Before each chunk, the focused X window is verified to belong
//! to the captured destination process, so typed events cannot silently land
//! in another application. This replaces the former AT-SPI
//! DeviceEventController path, which required no consent of any kind and
//! demonstrably delivered nothing in native testing.
use std::sync::{
    LockResult, MutexGuard, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::{ffi::c_void, sync::Mutex};
use x11_dl::xlib::{self, Xlib};
use x11_dl::xtest::Xf86vmode as Xtest;

static CONSENT: AtomicBool = AtomicBool::new(false);
static AVAILABLE: OnceLock<bool> = OnceLock::new();

const XK_SHIFT_L: xlib::XID = 0xffe1;
const XA_CARDINAL: xlib::Atom = 6;
const ANY_PROPERTY_TYPE: xlib::Atom = 0;
const POINTER_ROOT: xlib::Window = 1;

pub fn set_consent(enabled: bool) {
    CONSENT.store(enabled, Ordering::Release);
}
pub fn consented() -> bool {
    CONSENT.load(Ordering::Acquire)
}

/// The gate every X11 capture must pass before any key is synthesized.
pub fn keyboard_allowed() -> Result<(), String> {
    if !consented() {
        return Err(
            "X11 sessions have no system keyboard permission. Enable 'X11 keyboard delivery' in Settings → Shortcuts & delivery before typing into other applications."
                .into(),
        );
    }
    if !available() {
        return Err(
            "This display does not provide the XTest keyboard extension; external typing is unavailable here."
                .into(),
        );
    }
    Ok(())
}

/// One process-wide probe; a failed open is remembered, not retried per key.
pub fn available() -> bool {
    *AVAILABLE.get_or_init(|| with_session(|_| Ok(())).is_ok())
}

fn open() -> Result<Session, String> {
    let xlib = Xlib::open().map_err(|e| e.to_string())?;
    let xtest = Xtest::open().map_err(|e| e.to_string())?;
    // SAFETY: XOpenDisplay returns an owned connection for this thread; it is
    // stored and closed exactly once by the owning Session's Drop.
    let display = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
    if display.is_null() {
        return Err("No X display is available.".into());
    }
    let mut base = 0;
    let mut err_base = 0;
    let mut ev1 = 0;
    let mut ev2 = 0;
    // SAFETY: all outputs point at initialized local variables.
    let ok = unsafe {
        (xtest.XTestQueryExtension)(display, &mut ev1, &mut ev2, &mut base, &mut err_base)
    };
    if ok == 0 {
        // SAFETY: closing the display we opened above on this same thread.
        unsafe { (xlib.XCloseDisplay)(display) };
        return Err("The XTest extension is not available on this display.".into());
    }
    Ok(Session {
        display,
        xlib,
        xtest,
    })
}

struct Session {
    display: *mut xlib::Display,
    xlib: Xlib,
    xtest: Xtest,
}
impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the connection was opened by `open` on this thread and is
        // closed exactly once here.
        unsafe { (self.xlib.XCloseDisplay)(self.display) };
    }
}
unsafe impl Send for Session {}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn with_session<T>(work: impl FnOnce(&Session) -> Result<T, String>) -> Result<T, String> {
    fn borrow() -> LockResult<MutexGuard<'static, Option<Session>>> {
        SESSION.lock()
    }
    let mut guard = borrow().map_err(|_| "Keyboard connection state failed")?;
    if guard.is_none() {
        *guard = Some(open()?);
    }
    work(guard.as_ref().expect("session was just opened"))
}

/// Unicode keysym encoding: Latin-1 keeps its code point; everything above
/// uses the 0x0100_0000 | codepoint plane reserved for Unicode.
pub fn keysym_for(c: char) -> xlib::XID {
    let code = c as u32;
    if code <= 0xff {
        code as xlib::XID
    } else {
        0x0100_0000 | code as xlib::XID
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// The keysym already exists on the keyboard: press this keycode.
    Key { keycode: u8, shift: bool },
    /// Not on the keyboard: temporarily bind it to a spare keycode.
    Remap { keysym: xlib::XID },
}

/// Pure lookup over `(keycode, level-0 keysym, level-1/shift keysym)` rows.
/// Only base and shifted levels are used; higher levels are not synthesized.
pub fn plan_typing(keysym: xlib::XID, keymap: &[(u8, xlib::XID, xlib::XID)]) -> Plan {
    for &(keycode, base, shifted) in keymap {
        if base == keysym {
            return Plan::Key {
                keycode,
                shift: false,
            };
        }
        if shifted == keysym {
            return Plan::Key {
                keycode,
                shift: true,
            };
        }
    }
    Plan::Remap { keysym }
}

fn read_keymap(session: &Session) -> Result<Vec<(u8, xlib::XID, xlib::XID)>, String> {
    let mut min = 0;
    let mut max = 0;
    // SAFETY: outputs point at initialized locals; the display is owned.
    unsafe { (session.xlib.XDisplayKeycodes)(session.display, &mut min, &mut max) };
    if !(8..=255).contains(&min) || !(min..=255).contains(&max) {
        return Err("The keyboard map is unavailable.".into());
    }
    let count = max - min + 1;
    let mut per_code = 0;
    // SAFETY: the returned keysym list is owned by us and freed below; the
    // display is owned and the arguments are initialized locals.
    let symbols = unsafe {
        (session.xlib.XGetKeyboardMapping)(
            session.display,
            min as xlib::KeyCode,
            count,
            &mut per_code,
        )
    };
    if symbols.is_null() || per_code < 2 {
        if !symbols.is_null() {
            // SAFETY: freeing the list returned above on this same connection.
            unsafe { (session.xlib.XFree)(symbols.cast()) };
        }
        return Err("The keyboard map is unreadable.".into());
    }
    let mut rows = Vec::with_capacity(count as usize);
    for index in 0..count {
        // SAFETY: index is within the returned array of count*per_code items.
        let (base, shifted) = unsafe {
            (
                *symbols.add((index * per_code) as usize),
                *symbols.add((index * per_code + 1) as usize),
            )
        };
        rows.push(((min + index) as u8, base, shifted));
    }
    // SAFETY: freeing the list returned above on this same connection.
    unsafe { (session.xlib.XFree)(symbols.cast()) };
    Ok(rows)
}

fn sync(session: &Session) {
    // SAFETY: flushing the owned connection; no arguments to validate.
    unsafe { (session.xlib.XSync)(session.display, 0) };
}

fn press(session: &Session, keycode: u8, pressed: bool) -> Result<(), String> {
    // SAFETY: XTestFakeKeyEvent takes the owned display, a keycode within the
    // server range, a boolean press state and a zero (no) delay.
    let ok = unsafe {
        (session.xtest.XTestFakeKeyEvent)(
            session.display,
            keycode as std::ffi::c_uint,
            i32::from(pressed),
            0,
        )
    };
    sync(session);
    if ok == 0 {
        return Err("The X server refused a synthetic key event; typing stopped.".into());
    }
    Ok(())
}

fn window_pid(session: &Session, window: xlib::Window) -> Option<i64> {
    // SAFETY: XInternAtom with only_if_exists=true returns 0 for absent atoms;
    // the name is a static NUL-terminated literal.
    let atom = unsafe { (session.xlib.XInternAtom)(session.display, c"_NET_WM_PID".as_ptr(), 1) };
    if atom == 0 {
        return None;
    }
    let mut kind = 0;
    let mut format = 0;
    let mut items: std::ffi::c_ulong = 0;
    let mut remaining: std::ffi::c_ulong = 0;
    let mut data: *mut u8 = std::ptr::null_mut();
    // SAFETY: all outputs point at initialized locals; XGetWindowProperty
    // allocates data that we free below on the same connection.
    let ok = unsafe {
        (session.xlib.XGetWindowProperty)(
            session.display,
            window,
            atom,
            0,
            1,
            0,
            ANY_PROPERTY_TYPE,
            &mut kind,
            &mut format,
            &mut items,
            &mut remaining,
            &mut data,
        )
    };
    if ok != 0 || data.is_null() {
        return None;
    }
    let mut pid = None;
    // SAFETY: the server wrote a 32-bit cardinal at data when kind/format
    // match; reading that one word is within the allocation it returned.
    if kind == XA_CARDINAL && format == 32 && items >= 1 && !data.is_null() {
        pid = Some(unsafe { *(data.cast::<i64>()) });
    }
    // SAFETY: freeing the property data returned above.
    unsafe { (session.xlib.XFree)(data.cast()) };
    pid
}

fn parent_of(session: &Session, window: xlib::Window) -> Option<xlib::Window> {
    let mut root = 0;
    let mut parent = 0;
    let mut children: *mut xlib::Window = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: outputs are initialized locals; the children array is freed
    // immediately on the same connection.
    let ok = unsafe {
        (session.xlib.XQueryTree)(
            session.display,
            window,
            &mut root,
            &mut parent,
            &mut children,
            &mut count,
        )
    };
    if ok == 0 {
        return None;
    }
    if !children.is_null() {
        // SAFETY: freeing the array returned above.
        unsafe { (session.xlib.XFree)(children.cast()) };
    }
    (parent != 0 && parent != root).then_some(parent)
}

/// Fail closed unless the X server's current focus belongs to the captured
/// destination process. XTest delivers to whatever window the server has
/// focused, so this check runs before every chunk of typed text.
pub fn verify_focus(pid: u32) -> Result<(), String> {
    with_session(|session| {
        let mut focus = 0;
        let mut revert = 0;
        // SAFETY: outputs are initialized locals on the owned connection.
        unsafe { (session.xlib.XGetInputFocus)(session.display, &mut focus, &mut revert) };
        if focus == 0 || focus == POINTER_ROOT {
            return Err("No window is focused for keyboard delivery.".into());
        }
        let mut window = focus;
        for _ in 0..24 {
            if window_pid(session, window) == Some(pid as i64) {
                return Ok(());
            }
            match parent_of(session, window) {
                Some(parent) => window = parent,
                None => break,
            }
        }
        Err(
            "The focused X11 window does not belong to the dictation target; nothing was typed."
                .into(),
        )
    })
}

pub fn type_character(c: char) -> Result<(), String> {
    keyboard_allowed()?;
    if c.is_control() {
        return Err("Control character refused.".into());
    }
    let keysym = keysym_for(c);
    with_session(|session| {
        let plan = plan_typing(keysym, &read_keymap(session)?);
        match plan {
            Plan::Key {
                keycode,
                shift: false,
            } => {
                press(session, keycode, true)?;
                press(session, keycode, false)
            }
            Plan::Key {
                keycode,
                shift: true,
            } => {
                // SAFETY: XKeysymToKeycode only reads the owned connection.
                let shift_key =
                    unsafe { (session.xlib.XKeysymToKeycode)(session.display, XK_SHIFT_L) };
                if shift_key == 0 {
                    return type_by_remap(session, keysym);
                }
                press(session, shift_key, true)?;
                let typed = (|| {
                    press(session, keycode, true)?;
                    press(session, keycode, false)
                })();
                press(session, shift_key, false)?;
                typed
            }
            Plan::Remap { keysym } => type_by_remap(session, keysym),
        }
    })
}

/// Bind the keysym to the highest keycode for the press, then restore the
/// original mapping: the standard xdotool-style technique for characters not
/// present on the physical keyboard.
fn type_by_remap(session: &Session, keysym: xlib::XID) -> Result<(), String> {
    let mut min = 0;
    let mut max = 0;
    // SAFETY: outputs are initialized locals on the owned connection.
    unsafe { (session.xlib.XDisplayKeycodes)(session.display, &mut min, &mut max) };
    if !(8..=255).contains(&min) || !(min..=255).contains(&max) {
        return Err("The keyboard map is unavailable.".into());
    }
    let spare = max as u8;
    let mut per_code = 0;
    // SAFETY: the returned list is owned and restored below; no stale Xlib
    // cache is relied on because the spare keycode is pressed directly.
    let saved =
        unsafe { (session.xlib.XGetKeyboardMapping)(session.display, spare, 1, &mut per_code) };
    if saved.is_null() {
        return Err("The keyboard map is unreadable.".into());
    }
    let mut bind = [keysym];
    // SAFETY: bind/saved are owned arrays valid for the call; the server makes
    // its own copies of the mapping data.
    unsafe {
        (session.xlib.XChangeKeyboardMapping)(
            session.display,
            spare as i32,
            1,
            bind.as_mut_ptr(),
            1,
        )
    };
    sync(session);
    let typed = press(session, spare, true).and_then(|()| press(session, spare, false));
    unsafe {
        (session.xlib.XChangeKeyboardMapping)(
            session.display,
            spare as i32,
            per_code.max(1),
            saved,
            1,
        )
    };
    sync(session);
    // SAFETY: freeing the saved mapping list after the restore above.
    unsafe { (session.xlib.XFree)(saved.cast::<c_void>().cast()) };
    typed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_keysyms_follow_the_x11_encoding() {
        assert_eq!(keysym_for('a'), 0x61);
        // Latin-1 keeps its code point as the keysym.
        assert_eq!(keysym_for('É'), 0xc9);
        // Above Latin-1 the Unicode keysym plane applies.
        assert_eq!(keysym_for('€'), 0x0100_20ac);
        assert_eq!(keysym_for('世'), 0x0100_4e16);
    }

    #[test]
    fn planning_prefers_existing_keys_and_reports_shift() {
        let keymap = vec![
            (38u8, 0x0061, 0x0041), // a / A
            (11, 0x0031, 0x0021),   // 1 / !
        ];
        assert_eq!(
            plan_typing(0x0061, &keymap),
            Plan::Key {
                keycode: 38,
                shift: false
            }
        );
        assert_eq!(
            plan_typing(0x0041, &keymap),
            Plan::Key {
                keycode: 38,
                shift: true
            }
        );
        assert_eq!(
            plan_typing(0x0100_00e9, &keymap),
            Plan::Remap {
                keysym: 0x0100_00e9
            }
        );
    }

    #[test]
    fn typing_requires_explicit_consent_and_refuses_control_characters() {
        let before = consented();
        set_consent(false);
        let refused = type_character('a').unwrap_err();
        assert!(refused.contains("X11 keyboard delivery"), "{refused}");
        set_consent(true);
        // Control characters are refused before any connection is opened, so
        // this stays safe on machines without an X display.
        let control = type_character('\n').unwrap_err();
        assert!(control.contains("Control character refused"));
        set_consent(before);
    }

    #[test]
    fn capture_gate_names_the_setting_and_missing_extension() {
        let before = consented();
        set_consent(false);
        assert!(keyboard_allowed().unwrap_err().contains("Settings"));
        set_consent(true);
        // On a machine with XTest the gate passes; without one it must name
        // the extension rather than attempting to type.
        match keyboard_allowed() {
            Ok(()) => {}
            Err(error) => assert!(error.contains("XTest"), "{error}"),
        }
        set_consent(before);
    }
}

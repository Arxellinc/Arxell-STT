//! Native shortcuts on Windows/macOS/X11; the consent-based GlobalShortcuts
//! portal on Wayland. Shortcuts never show or focus the Arxell window.
use std::str::FromStr;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub fn validate(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    if !value.contains('+') {
        return Err("Use a modified shortcut, for example Ctrl+Shift+Space.".into());
    }
    Shortcut::from_str(value)
        .map(|_| ())
        .map_err(|e| format!("Invalid shortcut: {e}"))
}
pub fn begin_capture(state: &crate::app::AppState) -> Result<(), String> {
    // Share the session lock with start_dictation_from: capture and microphone
    // start must not race, including activations arriving from another thread.
    let session = state.session.lock().map_err(|_| "Session lock failed")?;
    if state.setup_busy.load(Ordering::Acquire) {
        return Err("Finish permission setup before recording a shortcut.".into());
    }
    if session
        .as_ref()
        .is_some_and(|s| s.running.load(Ordering::Acquire))
    {
        return Err("Stop dictation before recording a shortcut.".into());
    }
    state.shortcut_capture.store(true, Ordering::Release);
    Ok(())
}

pub fn end_capture(state: &crate::app::AppState) {
    if state.shortcut_capture.swap(false, Ordering::AcqRel) {
        state.shortcut_held.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use crate::app::{AppState, SessionHandle};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn capture_is_transient_and_does_not_change_the_saved_binding() {
        let state = AppState::default();
        state.settings.lock().unwrap().shortcut = "Ctrl+Shift+Space".into();
        begin_capture(&state).unwrap();
        assert!(state.shortcut_capture.load(Ordering::Acquire));
        state.shortcut_held.store(true, Ordering::Release);
        end_capture(&state);
        assert!(!state.shortcut_capture.load(Ordering::Acquire));
        assert!(!state.shortcut_held.load(Ordering::Acquire));
        assert_eq!(state.settings.lock().unwrap().shortcut, "Ctrl+Shift+Space");
        // Reload/focus changes when not capturing must not reset a held toggle.
        state.shortcut_held.store(true, Ordering::Release);
        end_capture(&state);
        assert!(state.shortcut_held.load(Ordering::Acquire));
    }

    #[test]
    fn capture_refuses_an_active_session() {
        let state = AppState::default();
        *state.session.lock().unwrap() = Some(SessionHandle {
            stop: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            abort: transcribe_cpp::CancelToken::new(),
            running: Arc::new(AtomicBool::new(true)),
            thread: std::thread::spawn(|| {}),
        });
        assert!(begin_capture(&state).is_err());
        assert!(!state.shortcut_capture.load(Ordering::Acquire));
        state
            .session
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .thread
            .join()
            .unwrap();
    }

    #[test]
    fn recorded_native_key_names_are_accepted() {
        for value in [
            "Ctrl+Shift+Space",
            "Super+K",
            "Alt+1",
            "Ctrl+Shift+Equal",
            "Ctrl+NumpadAdd",
            "Alt+F24",
            "Ctrl+ArrowUp",
            "Ctrl+Escape",
        ] {
            assert!(validate(value).is_ok(), "{value}");
        }
        assert!(validate("Space").is_err());
        assert!(validate("Ctrl+F25").is_err());
    }
}

fn toggle(app: &AppHandle) {
    let state = app.state::<crate::app::AppState>();
    if state.shortcut_capture.load(Ordering::Acquire) {
        // The OS may consume our existing registered accelerator before the
        // WebView sees it. Capture that chord too, without starting the mic or
        // tearing down/re-authorizing the existing Wayland portal session.
        if let Ok(settings) = state.settings.try_lock() {
            let _ = app.emit("shortcut-captured", &settings.shortcut);
        }
        return;
    }
    let running = state.session.lock().is_ok_and(|s| {
        s.as_ref()
            .is_some_and(|s| s.running.load(Ordering::Acquire))
    });
    crate::app::trace(format_args!("shortcut toggle was_running={running}"));
    let result = if running {
        crate::ui::stop_dictation(state)
    } else {
        crate::ui::start_dictation_from(app.clone(), state, false).map(|_| ())
    };
    if let Err(message) = result {
        crate::app::trace(format_args!("shortcut toggle outcome=error"));
        let _ = app.emit("shortcut-error", message);
    } else {
        crate::app::trace(format_args!("shortcut toggle outcome=accepted"));
    }
}
fn register(app: &AppHandle, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    app.global_shortcut()
        .on_shortcut(value, |app, _, event| {
            let pressed = event.state == ShortcutState::Pressed;
            let was_held = app
                .state::<crate::app::AppState>()
                .shortcut_held
                .swap(pressed, Ordering::AcqRel);
            if pressed && !was_held {
                toggle(app);
            }
        })
        .map_err(|e| format!("Shortcut unavailable or already in use: {e}"))
}
pub fn apply(app: &AppHandle, old: &str, new: &str) -> Result<(), String> {
    validate(new)?;
    if old == new {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if crate::platform::portal::wayland() {
        return wayland::apply(app, new);
    }
    if !old.is_empty() {
        app.global_shortcut()
            .unregister(old)
            .map_err(|e| e.to_string())?;
    }
    if let Err(error) = register(app, new) {
        let restored = register(app, old).is_ok() && !old.is_empty();
        app.state::<crate::app::AppState>()
            .shortcut_ready
            .store(restored, Ordering::Release);
        return Err(error);
    }
    app.state::<crate::app::AppState>()
        .shortcut_ready
        .store(!new.is_empty(), Ordering::Release);
    Ok(())
}
pub fn reauthorize(app: &AppHandle, value: &str) -> Result<(), String> {
    validate(value)?;
    if value.is_empty() {
        return Err("Set a start / stop shortcut first.".into());
    }
    #[cfg(target_os = "linux")]
    if crate::platform::portal::wayland() {
        return wayland::apply(app, value);
    }
    if !app.global_shortcut().is_registered(value) {
        register(app, value)?;
    }
    app.state::<crate::app::AppState>()
        .shortcut_ready
        .store(true, Ordering::Release);
    Ok(())
}

pub fn register_cancel(app: &AppHandle, key: &str) -> bool {
    #[cfg(target_os = "linux")]
    if crate::platform::portal::wayland() {
        return false;
    }
    let result = app.global_shortcut().on_shortcut(key, |app, _, event| {
        if event.state == ShortcutState::Pressed {
            let _ = crate::ui::cancel_dictation(app.state::<crate::app::AppState>());
        }
    });
    if let Err(error) = result {
        let _ = app.emit(
            "shortcut-error",
            format!("Global cancel unavailable; cancel still works inside Arxell: {error}"),
        );
        false
    } else {
        true
    }
}
pub fn unregister_cancel(app: &AppHandle, key: &str) {
    let _ = app.global_shortcut().unregister(key);
}

#[cfg(target_os = "linux")]
mod wayland {
    use super::*;
    use crate::platform::portal;
    use futures_util::StreamExt;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use zbus::{
        blocking::Proxy,
        zvariant::{OwnedObjectPath, OwnedValue, Value},
    };
    static SESSION: Mutex<Option<Arc<portal::Session>>> = Mutex::new(None);

    // Mutter can lose the accelerator release when Ctrl is released before
    // Space: release lookup uses the modifiers then held. Recover activation
    // deduplication, but NEVER infer physical key release from a quiet timeout.
    #[derive(Default)]
    struct PressGate {
        held: bool,
        last_press: Option<std::time::Instant>,
    }
    impl PressGate {
        fn expire(&mut self, now: std::time::Instant) -> bool {
            if self.held
                && self.last_press.is_some_and(|last| {
                    now.saturating_duration_since(last) >= Duration::from_secs(1)
                })
            {
                self.held = false;
                return true;
            }
            false
        }
        fn signal(&mut self, pressed: bool, now: std::time::Instant) -> bool {
            self.expire(now);
            let activate = pressed && !self.held;
            self.held = pressed;
            self.last_press = pressed.then_some(now);
            activate
        }
    }

    fn trigger(value: &str) -> String {
        use tauri_plugin_global_shortcut::Modifiers;
        let Ok(shortcut) = Shortcut::from_str(value) else {
            return value.to_owned();
        };
        let mut parts: Vec<String> = Vec::new();
        for (flag, name) in [
            (Modifiers::CONTROL, "CTRL"),
            (Modifiers::ALT, "ALT"),
            (Modifiers::SHIFT, "SHIFT"),
            (Modifiers::SUPER, "LOGO"),
        ] {
            if shortcut.mods.contains(flag) {
                parts.push(name.into());
            }
        }
        let code = shortcut.key.to_string();
        // Portal preferred_trigger uses XKB keysyms, not DOM Code names.
        let key = match code.as_str() {
            "Space" => "space",
            "Enter" => "Return",
            "Escape" => "Escape",
            "Backspace" => "BackSpace",
            "ArrowUp" => "Up",
            "ArrowDown" => "Down",
            "ArrowLeft" => "Left",
            "ArrowRight" => "Right",
            "PageUp" => "Prior",
            "PageDown" => "Next",
            "CapsLock" => "Caps_Lock",
            "NumLock" => "Num_Lock",
            "ScrollLock" => "Scroll_Lock",
            "PrintScreen" => "Print",
            "Backquote" => "grave",
            "Backslash" => "backslash",
            "BracketLeft" => "bracketleft",
            "BracketRight" => "bracketright",
            "Comma" => "comma",
            "Equal" => "equal",
            "Minus" => "minus",
            "Period" => "period",
            "Quote" => "apostrophe",
            "Semicolon" => "semicolon",
            "Slash" => "slash",
            "NumpadAdd" => "KP_Add",
            "NumpadSubtract" => "KP_Subtract",
            "NumpadMultiply" => "KP_Multiply",
            "NumpadDivide" => "KP_Divide",
            "NumpadDecimal" => "KP_Decimal",
            "NumpadEnter" => "KP_Enter",
            "NumpadEqual" => "KP_Equal",
            "AudioVolumeDown" => "XF86AudioLowerVolume",
            "AudioVolumeUp" => "XF86AudioRaiseVolume",
            "AudioVolumeMute" => "XF86AudioMute",
            "MediaPlay" | "MediaPlayPause" => "XF86AudioPlay",
            "MediaPause" => "XF86AudioPause",
            "MediaStop" => "XF86AudioStop",
            "MediaTrackNext" => "XF86AudioNext",
            "MediaTrackPrevious" => "XF86AudioPrev",
            other => other,
        };
        parts.push(if let Some(letter) = key.strip_prefix("Key") {
            letter.to_ascii_lowercase()
        } else if let Some(digit) = key.strip_prefix("Digit") {
            digit.into()
        } else if let Some(digit) = key.strip_prefix("Numpad") {
            format!("KP_{digit}")
        } else {
            key.into()
        });
        parts.join("+")
    }
    pub fn apply(app: &AppHandle, value: &str) -> Result<(), String> {
        app.state::<crate::app::AppState>()
            .shortcut_ready
            .store(false, Ordering::Release);
        let generation = app
            .state::<crate::app::AppState>()
            .shortcut_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        app.state::<crate::app::AppState>()
            .shortcut_held
            .store(false, Ordering::Release);
        if let Some(old) = SESSION.lock().map_err(|_| "Shortcut state failed")?.take() {
            old.close();
        }
        if value.is_empty() {
            return Ok(());
        }
        let app = app.clone();
        let value = value.to_owned();
        std::thread::Builder::new().name("wayland-shortcut".into()).spawn(move || {
            let result = listen(&app, &value, generation);
            if let Err(error) = result
                && app.state::<crate::app::AppState>().shortcut_generation.load(Ordering::Acquire) == generation {
                app.state::<crate::app::AppState>().shortcut_held.store(false, Ordering::Release);
                app.state::<crate::app::AppState>().shortcut_ready.store(false, Ordering::Release);
                if let Ok(mut slot) = SESSION.lock() && app.state::<crate::app::AppState>().shortcut_generation.load(Ordering::Acquire) == generation
                    && let Some(session) = slot.take() { session.close(); }
                let _ = app.emit("shortcut-error", format!("Wayland shortcut unavailable: {error} Use the microphone button, or choose Authorize / retry in Shortcut settings."));
            }
        }).map_err(|e| e.to_string())?;
        Ok(())
    }
    fn listen(app: &AppHandle, value: &str, generation: u64) -> Result<(), String> {
        // Async zbus streams must be destroyed in a runtime context even when
        // consent is denied or this generation is superseded before binding.
        let runtime = tauri::async_runtime::handle();
        let _context = runtime.inner().enter();
        let current = || {
            app.state::<crate::app::AppState>()
                .shortcut_generation
                .load(Ordering::Acquire)
                == generation
        };
        let session = Arc::new(portal::create_session(portal::SHORTCUTS)?);
        if !current() {
            return Ok(());
        }
        let proxy = Proxy::new(
            &session.connection,
            portal::SERVICE,
            portal::PATH,
            portal::SHORTCUTS,
        )
        .map_err(|e| e.to_string())?;
        // Subscribe before BindShortcuts to avoid losing a fast first activation.
        let mut signals = tauri::async_runtime::block_on(proxy.inner().receive_all_signals())
            .map_err(|e| e.to_string())?;
        let session_proxy = Proxy::new(
            &session.connection,
            portal::SERVICE,
            session.path.as_str(),
            "org.freedesktop.portal.Session",
        )
        .map_err(|e| e.to_string())?;
        let mut closed =
            tauri::async_runtime::block_on(session_proxy.inner().receive_signal("Closed"))
                .map_err(|e| e.to_string())?;
        let mut owners = tauri::async_runtime::block_on(proxy.inner().receive_owner_changed())
            .map_err(|e| e.to_string())?;
        let handle = portal::token();
        let preferred = trigger(value);
        let shortcuts = vec![(
            "dictation",
            HashMap::from([
                ("description", Value::from("Start / stop Arxell dictation")),
                ("preferred_trigger", Value::from(preferred.as_str())),
            ]),
        )];
        let options = HashMap::from([("handle_token", Value::from(handle.as_str()))]);
        let _ = app.emit(
            "shortcut-status",
            "Approve the global dictation shortcut in the desktop permission dialog.",
        );
        portal::request(
            &session.connection,
            portal::SHORTCUTS,
            "BindShortcuts",
            &handle,
            &(&session.path, shortcuts, "", options),
        )?;
        if !current() {
            return Ok(());
        }
        *SESSION.lock().map_err(|_| "Shortcut state failed")? = Some(session.clone());
        app.state::<crate::app::AppState>()
            .shortcut_ready
            .store(true, Ordering::Release);
        let _ = app.emit(
            "shortcut-status",
            "Global dictation shortcut authorized by the desktop.",
        );
        let mut press_gate = PressGate::default();
        while current() {
            let next: Result<Option<zbus::Message>, String> = tauri::async_runtime::block_on(
                async {
                    tokio::select! {
                        message = signals.next() => message.map(Some).ok_or_else(|| "Desktop shortcut portal disconnected.".into()),
                        _ = closed.next() => Err("Desktop shortcut permission was revoked or the session closed.".into()),
                        _ = owners.next() => Err("Desktop portal restarted. Reauthorize the shortcut.".into()),
                        _ = tokio::time::sleep(Duration::from_millis(250)) => Ok(None),
                    }
                },
            );
            let Some(message) = next? else {
                // A timeout is not proof that modifiers are up. Keep desktop
                // delivery blocked until an actual Deactivated signal arrives.
                continue;
            };
            let member = message
                .header()
                .member()
                .map(|s| s.as_str().to_owned())
                .unwrap_or_default();
            if !matches!(member.as_str(), "Activated" | "Deactivated") {
                continue;
            }
            let Ok((path, id, _, _)): Result<
                (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>),
                _,
            > = message.body().deserialize() else {
                continue;
            };
            if path != session.path || id != "dictation" {
                continue;
            }
            let activate = press_gate.signal(member == "Activated", std::time::Instant::now());
            crate::app::trace(format_args!(
                "portal shortcut event={member} toggle={activate}"
            ));
            app.state::<crate::app::AppState>()
                .shortcut_held
                .store(press_gate.held, Ordering::Release);
            if activate {
                toggle(app);
            }
        }
        Ok(())
    }
    #[cfg(test)]
    mod tests {
        #[test]
        fn missing_release_recovers_but_repeats_do_not_toggle() {
            let now = std::time::Instant::now();
            let mut gate = super::PressGate::default();
            assert!(gate.signal(true, now));
            for ms in [500, 530, 560, 590, 900, 1200] {
                assert!(!gate.signal(true, now + std::time::Duration::from_millis(ms)));
            }
            assert!(!gate.expire(now + std::time::Duration::from_millis(2000)));
            assert!(gate.expire(now + std::time::Duration::from_millis(2300)));
            assert!(gate.signal(true, now + std::time::Duration::from_millis(3000)));
            assert!(!gate.signal(false, now + std::time::Duration::from_millis(3100)));
            assert!(gate.signal(true, now + std::time::Duration::from_millis(3200)));
        }

        #[test]
        fn translates_portal_accelerators() {
            for (value, expected) in [
                ("Ctrl+Shift+Space", "CTRL+SHIFT+space"),
                ("Ctrl+K", "CTRL+k"),
                ("Alt+F12", "ALT+F12"),
                ("Super+ArrowUp", "LOGO+Up"),
                ("Ctrl+Shift+Equal", "CTRL+SHIFT+equal"),
                ("Ctrl+NumpadAdd", "CTRL+KP_Add"),
                ("Alt+Numpad1", "ALT+KP_1"),
                ("Control+PageDown", "CTRL+Next"),
            ] {
                assert_eq!(super::trigger(value), expected);
            }
        }
    }
}

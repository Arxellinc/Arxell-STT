//! Permission/readiness gate shared by the UI AND native shortcut start path.
//! A loaded model alone never authorizes recording or cross-application input.
use crate::{
    app::{AppState, ModelStatus},
    settings::Settings,
};
use std::sync::atomic::Ordering;

#[derive(Default)]
pub struct Checks {
    pub microphone: Option<String>,
    pub microphone_error: Option<String>,
    pub desktop_verified: bool,
    pub desktop_error: Option<String>,
}
#[derive(Clone, serde::Serialize)]
pub struct Check {
    pub id: &'static str,
    pub label: &'static str,
    pub required: bool,
    pub ready: bool,
    pub message: String,
}
#[derive(serde::Serialize)]
pub struct Snapshot {
    pub ready: bool,
    pub busy: bool,
    pub checks: Vec<Check>,
}
fn complete(checks: &[Check]) -> bool {
    checks.iter().all(|check| !check.required || check.ready)
}

pub fn snapshot(state: &AppState, settings: &Settings) -> Snapshot {
    let checks = state.setup_checks.lock().unwrap_or_else(|e| e.into_inner());
    let microphone = checks.microphone.as_ref() == Some(&settings.microphone);
    let desktop = crate::platform::status();
    let shortcut = settings.shortcut.is_empty() || state.shortcut_ready.load(Ordering::Acquire);
    let model = state
        .model_status
        .lock()
        .is_ok_and(|status| *status == ModelStatus::Ready);
    let checks = vec![
        Check {
            id: "microphone",
            label: "Microphone",
            required: true,
            ready: microphone,
            message: if microphone {
                "Audio input checked; test audio was discarded.".into()
            } else {
                checks
                    .microphone_error
                    .clone()
                    .unwrap_or_else(|| "Microphone access and audio input must be checked.".into())
            },
        },
        Check {
            id: "desktop",
            label: "Typing into other applications",
            required: settings.advanced.desktop_enabled,
            ready: checks.desktop_verified && desktop.supported && desktop.keyboard_ready,
            message: if !settings.advanced.desktop_enabled {
                "Disabled — in-app transcription only.".into()
            } else if checks.desktop_verified && desktop.keyboard_ready {
                "System input access verified for this app session.".into()
            } else if !desktop.supported {
                "Not supported on this desktop. Disable external typing or use a supported desktop."
                    .into()
            } else {
                checks
                    .desktop_error
                    .clone()
                    .unwrap_or_else(|| desktop.message.clone())
            },
        },
        Check {
            id: "shortcut",
            label: "Global shortcut",
            required: !settings.shortcut.is_empty(),
            ready: shortcut,
            message: if settings.shortcut.is_empty() {
                "Not configured; use the microphone button.".into()
            } else if shortcut {
                "Registered with the desktop.".into()
            } else {
                "Shortcut authorization is not complete. Authorize or change the shortcut in Settings.".into()
            },
        },
        Check {
            id: "model",
            label: "Transcription model",
            required: true,
            ready: model,
            message: if model {
                "Loaded.".into()
            } else {
                "Waiting for a loaded model. Check Model settings if loading fails.".into()
            },
        },
    ];
    let busy = state.setup_busy.load(Ordering::Acquire);
    Snapshot {
        ready: !busy && complete(&checks),
        busy,
        checks,
    }
}

pub fn ensure_ready(state: &AppState, settings: &Settings) -> Result<(), String> {
    let report = snapshot(state, settings);
    if report.busy {
        return Err("Setup is still checking required access. No recording was started.".into());
    }
    if let Some(check) = report
        .checks
        .iter()
        .find(|check| check.required && !check.ready)
    {
        return Err(format!(
            "Setup required — {}: {} Open Arxell and complete setup before dictating.",
            check.label, check.message
        ));
    }
    Ok(())
}

pub fn check_microphone(state: &AppState, settings: &Settings) -> Result<(), String> {
    let result = crate::mic::check_input(&settings.microphone);
    let mut checks = state
        .setup_checks
        .lock()
        .map_err(|_| "Setup state lock failed")?;
    match result {
        Ok(()) => {
            checks.microphone = Some(settings.microphone.clone());
            checks.microphone_error = None;
            Ok(())
        }
        Err(error) => {
            checks.microphone = None;
            checks.microphone_error = Some(error.clone());
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(required: bool, ready: bool) -> Check {
        Check {
            id: "test",
            label: "test",
            required,
            ready,
            message: String::new(),
        }
    }
    #[test]
    fn every_enabled_requirement_must_be_verified() {
        assert!(complete(&[check(true, true), check(false, false)]));
        for missing in 0..4 {
            let mut checks = vec![check(true, true); 4];
            checks[missing].ready = false;
            assert!(!complete(&checks));
        }
    }
    #[test]
    fn loaded_model_does_not_bypass_setup_or_the_native_start_gate() {
        let state = AppState::default();
        *state.model_status.lock().unwrap() = ModelStatus::Ready;
        let settings = Settings::default();
        assert!(!snapshot(&state, &settings).ready);
        assert!(
            ensure_ready(&state, &settings)
                .unwrap_err()
                .starts_with("Setup required")
        );
    }
    #[test]
    fn microphone_check_is_specific_to_the_selected_input() {
        let state = AppState::default();
        *state.model_status.lock().unwrap() = ModelStatus::Ready;
        state.setup_checks.lock().unwrap().microphone = Some(String::new());
        let mut settings = Settings::default();
        settings.advanced.desktop_enabled = false;
        settings.shortcut.clear();
        assert!(ensure_ready(&state, &settings).is_ok());
        settings.microphone = "different device".into();
        assert!(ensure_ready(&state, &settings).is_err());
        state.setup_busy.store(true, Ordering::Release);
        assert!(
            ensure_ready(&state, &settings)
                .unwrap_err()
                .contains("still checking")
        );
    }
}

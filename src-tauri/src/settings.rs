//! Advanced settings, defaults and validation.

use serde::{Deserialize, Serialize};

use crate::cue::{CueConfig, normalize};
use crate::engine::{BackendPreference, StreamPreset};
use crate::submit::SubmitKey;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    pub model_path: String,
    pub advanced: AdvancedSettings,
    #[serde(default)]
    pub microphone: String,
    #[serde(default)]
    pub shortcut: String,
    #[serde(default = "default_cancel_key")]
    pub cancel_key: String,
    /// Number of historical transcripts to retain; zero means unlimited.
    #[serde(default = "default_history_limit")]
    pub history_limit: u32,
    #[serde(default = "default_history_visible")]
    pub history_visible: bool,
}

fn default_history_limit() -> u32 {
    10
}

fn default_history_visible() -> bool {
    true
}

fn default_cancel_key() -> String {
    "Escape".into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 4,
            model_path: std::env::var("ARXELL_MODEL")
                .unwrap_or_else(|_| crate::engine::DEFAULT_MODEL_PATH.into()),
            advanced: AdvancedSettings::default(),
            microphone: String::new(),
            shortcut: String::new(),
            cancel_key: default_cancel_key(),
            history_limit: default_history_limit(),
            history_visible: default_history_visible(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 4 {
            return Err("unsupported settings version".into());
        }
        if self.model_path.trim().is_empty() {
            return Err("choose a model file".into());
        }
        if self.model_path.len() > 4096 || self.microphone.len() > 512 || self.shortcut.len() > 128
        {
            return Err("setting is too long".into());
        }
        if !["Escape", "Control+Escape", "Alt+Escape"].contains(&self.cancel_key.as_str()) {
            return Err("unsupported cancel key".into());
        }
        if self.shortcut.eq_ignore_ascii_case(&self.cancel_key) {
            return Err("Start/stop and cancel shortcuts must be different.".into());
        }
        if ![0, 3, 5, 10, 20, 100].contains(&self.history_limit) {
            return Err("history limit must be 3, 5, 10, 20, 100 or unlimited".into());
        }
        self.advanced.validate()
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        Self::load_with_migration(path).map(|(settings, _)| settings)
    }

    /// Also reports whether an older version was upgraded in memory.
    pub fn load_with_migration(path: &std::path::Path) -> Result<(Self, bool), String> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|e| format!("read settings: {e}"))?;
                let mut migrated = false;
                let version = value
                    .get("version")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                if version == 1 {
                    // v1 shipped idle unload on by default; the model now stays
                    // loaded unless the user asks otherwise.
                    if let Some(advanced) = value.get_mut("advanced") {
                        advanced["idle_unload_seconds"] = serde_json::json!(0);
                    }
                    value["version"] = serde_json::json!(2);
                    migrated = true;
                }
                let version = value
                    .get("version")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                if version < 3 {
                    // v2 stored raw low-latency chunk sizes that could not
                    // keep up with real time; replace them with named presets
                    // and enable VAD gating.
                    if let Some(advanced) = value.get_mut("advanced")
                        && let Some(object) = advanced.as_object_mut()
                    {
                        object.remove("stream_ms");
                        object
                            .entry("stream_preset")
                            .or_insert(serde_json::json!("efficient"));
                        object
                            .entry("vad_gating")
                            .or_insert(serde_json::json!(true));
                    }
                    value["version"] = serde_json::json!(3);
                    migrated = true;
                }
                let version = value
                    .get("version")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                if version < 4 {
                    // Licensing moved out of the UI; the model is installed
                    // explicitly from the model selector.
                    if let Some(object) = value.as_object_mut() {
                        object.remove("model_license_accepted");
                    }
                    value["version"] = serde_json::json!(4);
                    migrated = true;
                }
                let settings: Self =
                    serde_json::from_value(value).map_err(|e| format!("read settings: {e}"))?;
                settings.validate()?;
                Ok((settings, migrated))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok((Self::default(), false))
            }
            Err(error) => Err(format!("read settings: {error}")),
        }
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        self.validate()?;
        let parent = path.parent().ok_or("settings directory is missing")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let temporary = path.with_extension("json.tmp");
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        // Keep the previous settings if replacement is not supported or fails.
        std::fs::rename(&temporary, path).map_err(|e| format!("save settings: {e}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdvancedSettings {
    pub cue: CueConfig,
    pub backend: BackendPreference,
    pub submit_key: SubmitKey,
    pub submit_enabled: bool,
    pub allow_unknown_targets: bool,
    pub api_enabled: bool,
    pub api_port: u16,
    pub api_token: String,
    pub api_max_seconds: u64,
    pub vad_threshold: f32,
    pub speech_hold_ms: u64,
    pub max_session_seconds: u64,
    pub idle_unload_seconds: u64,
    pub stream_preset: StreamPreset,
    pub vad_gating: bool,
    pub cues_enabled: bool,
    pub diagnostics: bool,
    pub desktop_enabled: bool,
    pub terminal_enabled: bool,
    #[serde(default)]
    pub x11_keyboard_enabled: bool,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            cue: CueConfig::default(),
            backend: BackendPreference::Auto,
            submit_key: SubmitKey::Enter,
            submit_enabled: true,
            allow_unknown_targets: false,
            api_enabled: false,
            api_port: 14321,
            api_token: String::new(),
            api_max_seconds: 120,
            vad_threshold: 0.5,
            speech_hold_ms: 0,
            max_session_seconds: 600,
            idle_unload_seconds: 0,
            stream_preset: StreamPreset::Efficient,
            vad_gating: true,
            cues_enabled: true,
            diagnostics: false,
            desktop_enabled: true,
            terminal_enabled: true,
            x11_keyboard_enabled: false,
        }
    }
}

impl AdvancedSettings {
    pub fn validate(&self) -> Result<(), String> {
        let cue = &self.cue;
        if self.allow_unknown_targets {
            return Err("Unverified target insertion is not supported by this build.".into());
        }
        if !self.vad_threshold.is_finite() || !(0.05..=0.95).contains(&self.vad_threshold) {
            return Err("speech threshold must be between 0.05 and 0.95".into());
        }
        if self.speech_hold_ms > 200 || self.speech_hold_ms >= cue.pre_gap_ms {
            return Err(
                "speech hold must be at most 200 ms and less than the pre-cue pause".into(),
            );
        }
        if !(10..=3600).contains(&self.max_session_seconds) || self.idle_unload_seconds > 3600 {
            return Err("session limit must be 10–3600 seconds; idle unload 0–3600 seconds".into());
        }
        if self.api_port < 1024 || !(1..=600).contains(&self.api_max_seconds) {
            return Err("API port must be 1024–65535; audio limit 1–600 seconds".into());
        }
        if self.api_enabled
            && (self.api_token.len() < 32
                || self.api_token.len() > 128
                || !self.api_token.is_ascii()
                || self.api_token.bytes().any(|b| b <= 32 || b >= 127))
        {
            return Err("API token must contain 32–128 printable ASCII characters".into());
        }

        if cue.pre_gap_ms == 0 || cue.pre_gap_ms >= cue.confirm_ms {
            return Err("pre-cue gap must be > 0 and less than the confirmation pause".into());
        }
        if cue.confirm_ms >= cue.inactivity_ms {
            return Err("confirmation pause must be less than the inactivity timeout".into());
        }
        if cue.text_grace_ms > 5000
            || cue.confirm_ms.saturating_add(cue.text_grace_ms) >= cue.inactivity_ms
        {
            return Err("cue recognition grace must be at most 5000 ms and finish before inactivity timeout".into());
        }
        if cue.max_cue_ms == 0 {
            return Err("maximum cue duration must be greater than zero".into());
        }
        if cue.inactivity_ms > 120_000 || cue.max_cue_ms > 10_000 {
            return Err(
                "inactivity must be at most 120 seconds and cue duration at most 10 seconds".into(),
            );
        }
        if [&cue.end_phrase, &cue.submit_phrase, &cue.cancel_phrase]
            .iter()
            .any(|s| s.len() > 128)
        {
            return Err("cue phrases must be at most 128 bytes".into());
        }

        let phrases = [&cue.end_phrase, &cue.submit_phrase, &cue.cancel_phrase];
        if phrases.iter().any(|phrase| normalize(phrase).is_empty()) {
            return Err("cue phrases must not be empty".into());
        }

        let normalized: Vec<String> = phrases.iter().map(|phrase| normalize(phrase)).collect();
        for (index, left) in normalized.iter().enumerate() {
            for right in normalized.iter().skip(index + 1) {
                if left == right {
                    return Err("cue phrases must be unique".into());
                }
            }
        }

        Ok(())
    }

    pub fn prefix_warnings(&self) -> Vec<String> {
        let entries = [
            ("end", normalize(&self.cue.end_phrase)),
            ("submit", normalize(&self.cue.submit_phrase)),
            ("cancel", normalize(&self.cue.cancel_phrase)),
        ];
        let mut warnings = Vec::new();
        for (left_name, left) in &entries {
            for (right_name, right) in &entries {
                if left_name == right_name || left == right {
                    continue;
                }
                if right.starts_with(left) {
                    warnings.push(format!(
                        "{left_name} phrase '{left}' is a prefix of {right_name} phrase '{right}'"
                    ));
                }
            }
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_unknown_version_rejected() {
        let settings = Settings::default();
        let json = serde_json::to_string(&settings).unwrap();
        let mut decoded: Settings = serde_json::from_str(&json).unwrap();
        assert!(decoded.validate().is_ok());
        decoded.version = 5;
        assert!(decoded.validate().is_err());
    }

    #[test]
    fn v1_settings_migrate_idle_unload_to_never() {
        let directory = std::env::temp_dir().join(format!(
            "arxell-settings-migrate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("settings.json");
        std::fs::create_dir_all(&directory).unwrap();
        let mut v1 = Settings {
            version: 1,
            ..Default::default()
        };
        v1.advanced.idle_unload_seconds = 300; // the old default
        std::fs::write(&path, serde_json::to_string(&v1).unwrap()).unwrap();

        let migrated = Settings::load(&path).unwrap();
        assert_eq!(migrated.version, 4);
        assert_eq!(migrated.advanced.idle_unload_seconds, 0);

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn default_keeps_the_model_loaded() {
        assert_eq!(AdvancedSettings::default().idle_unload_seconds, 0);
    }

    #[test]
    fn legacy_settings_receive_new_defaults() {
        let mut json = serde_json::to_value(Settings::default()).unwrap();
        for key in [
            "microphone",
            "shortcut",
            "cancel_key",
            "model_license_accepted",
        ] {
            json.as_object_mut().unwrap().remove(key);
        }
        let advanced = json["advanced"].as_object_mut().unwrap();
        advanced.retain(|key, _| {
            [
                "cue",
                "backend",
                "submit_key",
                "submit_enabled",
                "allow_unknown_targets",
                "api_enabled",
            ]
            .contains(&key.as_str())
        });
        let settings: Settings = serde_json::from_value(json).unwrap();
        settings.validate().unwrap();
        assert_eq!(settings.advanced.vad_threshold, 0.5);
        assert_eq!(settings.cancel_key, "Escape");
    }

    #[test]
    fn rejects_invalid_runtime_limits_and_weak_api_tokens() {
        let mut settings = Settings::default();
        settings.advanced.vad_threshold = f32::NAN;
        assert!(settings.validate().is_err());
        settings.advanced.vad_threshold = 0.5;
        settings.advanced.api_enabled = true;
        assert!(settings.validate().is_err());
        settings.advanced.api_token = "x".repeat(64);
        assert!(settings.validate().is_ok());
        settings.advanced.speech_hold_ms = 250;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn saves_private_settings_and_preserves_previous_file_on_invalid_input() {
        let directory = std::env::temp_dir().join(format!(
            "arxell-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("settings.json");
        let mut settings = Settings::default();
        settings.save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        settings.advanced.api_port = 80;
        assert!(settings.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(Settings::load(&path).is_ok());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn v2_settings_migrate_to_named_preset_and_gating() {
        let directory = std::env::temp_dir().join(format!(
            "arxell-settings-v2-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("settings.json");
        std::fs::create_dir_all(&directory).unwrap();
        let mut value = serde_json::to_value(Settings::default()).unwrap();
        value["version"] = serde_json::json!(2);
        let advanced = value["advanced"].as_object_mut().unwrap();
        advanced.remove("stream_preset");
        advanced.remove("vad_gating");
        advanced.insert("stream_ms".into(), serde_json::json!(320));
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        let (migrated, changed) = Settings::load_with_migration(&path).unwrap();
        assert!(changed);
        assert_eq!(migrated.version, 4);
        assert_eq!(migrated.advanced.stream_preset, StreamPreset::Efficient);
        assert!(migrated.advanced.vad_gating);
        assert!(
            !serde_json::to_string(&migrated)
                .unwrap()
                .contains("stream_ms")
        );

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn desktop_and_terminal_typing_default_on_but_explicit_off_is_preserved() {
        let mut settings = Settings::default();
        assert!(settings.advanced.terminal_enabled);
        assert!(settings.advanced.desktop_enabled);
        settings.validate().unwrap();
        settings.advanced.terminal_enabled = true;
        let json = serde_json::to_string(&settings).unwrap();
        let decoded: Settings = serde_json::from_str(&json).unwrap();
        assert!(decoded.advanced.terminal_enabled);
        settings.advanced.desktop_enabled = false;
        settings.advanced.terminal_enabled = false;
        let decoded: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(!decoded.advanced.desktop_enabled);
        assert!(!decoded.advanced.terminal_enabled);
    }

    #[test]
    fn x11_keyboard_delivery_requires_explicit_opt_in_and_older_files_default_off() {
        assert!(!Settings::default().advanced.x11_keyboard_enabled);
        // A pre-XTest settings file has no such field: it must stay off, since
        // X11 offers no system dialog and the toggle is the only consent.
        let legacy: Settings =
            serde_json::from_str(r#"{"version":4,"model_path":"","advanced":{}}"#)
                .expect("decode legacy file");
        assert!(!legacy.advanced.x11_keyboard_enabled);
        let mut enabled = Settings::default();
        enabled.advanced.x11_keyboard_enabled = true;
        let decoded: Settings =
            serde_json::from_str(&serde_json::to_string(&enabled).unwrap()).unwrap();
        assert!(decoded.advanced.x11_keyboard_enabled);
    }

    #[test]
    fn history_defaults_and_limits() {
        let mut value = serde_json::to_value(Settings::default()).unwrap();
        value.as_object_mut().unwrap().remove("history_limit");
        value.as_object_mut().unwrap().remove("history_visible");
        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert_eq!(settings.history_limit, 10);
        assert!(settings.history_visible);
        for limit in [0, 3, 5, 10, 20, 100] {
            settings.history_limit = limit;
            settings.validate().unwrap();
        }
        settings.history_limit = 4;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn default_is_valid() {
        assert!(AdvancedSettings::default().validate().is_ok());
    }

    #[test]
    fn rejects_bad_pre_gap() {
        let mut settings = AdvancedSettings::default();
        settings.cue.pre_gap_ms = 800;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn rejects_confirmation_after_timeout() {
        let mut settings = AdvancedSettings::default();
        settings.cue.confirm_ms = 3_000;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_phrases() {
        let mut settings = AdvancedSettings::default();
        settings.cue.submit_phrase = "over".into();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn rejects_empty_phrase() {
        let mut settings = AdvancedSettings::default();
        settings.cue.cancel_phrase = "  ".into();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn warns_on_prefix_pair() {
        let mut settings = AdvancedSettings::default();
        settings.cue.submit_phrase = "over and out".into();
        assert!(settings.validate().is_ok());
        let warnings = settings.prefix_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("prefix"));
    }
}

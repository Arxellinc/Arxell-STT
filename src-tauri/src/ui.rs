use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::app::{APP_NAME, AppState, SessionHandle, VERSION};
use crate::engine;
use crate::live;

#[derive(Serialize)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub build_id: &'static str,
    pub status: &'static str,
    pub warning: Option<String>,
    pub desktop_available: bool,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        name: APP_NAME,
        version: VERSION,
        build_id: crate::app::BUILD_ID,
        status: if state
            .settings
            .lock()
            .is_ok_and(|settings| crate::setup::snapshot(&state, &settings).ready)
        {
            "Ready"
        } else {
            "Setup required"
        },
        warning: state.setup_error.lock().ok().and_then(|s| s.clone()),
        desktop_available: cfg!(any(
            target_os = "linux",
            target_os = "windows",
            target_os = "macos"
        )),
    }
}

#[tauri::command]
pub fn begin_shortcut_capture(state: State<'_, AppState>) -> Result<(), String> {
    crate::shortcuts::begin_capture(&state)
}

#[tauri::command]
pub fn end_shortcut_capture(state: State<'_, AppState>) {
    crate::shortcuts::end_capture(&state);
}

#[tauri::command]
pub fn reauthorize_shortcut(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let shortcut = state
        .settings
        .lock()
        .map_err(|_| "Settings lock failed")?
        .shortcut
        .clone();
    crate::shortcuts::reauthorize(&app, &shortcut)
}

#[tauri::command]
pub fn desktop_status() -> crate::platform::DesktopStatus {
    crate::platform::status()
}

#[tauri::command]
pub fn setup_status(state: State<'_, AppState>) -> Result<crate::setup::Snapshot, String> {
    let settings = state.settings.lock().map_err(|_| "Settings lock failed")?;
    Ok(crate::setup::snapshot(&state, &settings))
}

struct SetupGuard(AppHandle);
impl Drop for SetupGuard {
    fn drop(&mut self) {
        self.0
            .state::<AppState>()
            .setup_busy
            .store(false, Ordering::Release);
        crate::app::trace(format_args!("setup request ended"));
    }
}

#[tauri::command]
pub async fn setup_permissions(
    app: AppHandle,
    window: tauri::WebviewWindow,
) -> Result<crate::setup::Snapshot, String> {
    crate::app::trace(format_args!("setup command received"));
    {
        let state = app.state::<AppState>();
        let session = state.session.lock().map_err(|_| "Session lock failed")?;
        if state.shortcut_capture.load(Ordering::Acquire) {
            return Err("Finish recording the shortcut before permission setup.".into());
        }
        if session
            .as_ref()
            .is_some_and(|session| session.running.load(Ordering::Acquire))
        {
            return Err("Stop dictation before checking permissions.".into());
        }
        state.setup_busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "A permission check is already running. Complete the system dialog before retrying.")?;
    }
    let guard = SetupGuard(app.clone());
    crate::app::trace(format_args!("setup started"));
    {
        let state = app.state::<AppState>();
        let mut checks = state
            .setup_checks
            .lock()
            .map_err(|_| "Setup state lock failed")?;
        checks.microphone_error = None;
        checks.desktop_error = None;
    }
    let settings = app
        .state::<AppState>()
        .settings
        .lock()
        .map_err(|_| "Settings lock failed")?
        .clone();
    let worker_app = app.clone();
    let microphone_settings = settings.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::setup::check_microphone(&worker_app.state::<AppState>(), &microphone_settings)
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::app::trace(format_args!("setup microphone=verified"));
    if settings.advanced.desktop_enabled {
        let result = prepare_system_input(&app, window).await;
        crate::app::trace(format_args!("setup desktop_success={}", result.is_ok()));
        let state = app.state::<AppState>();
        let mut checks = state
            .setup_checks
            .lock()
            .map_err(|_| "Setup state lock failed")?;
        checks.desktop_verified = result.is_ok() && crate::platform::status().keyboard_ready;
        checks.desktop_error = result.as_ref().err().cloned();
        result?;
    }
    if !settings.shortcut.is_empty()
        && !app
            .state::<AppState>()
            .shortcut_ready
            .load(Ordering::Acquire)
    {
        crate::shortcuts::reauthorize(&app, &settings.shortcut)?;
    }
    drop(guard);
    crate::app::trace(format_args!("setup finished"));
    let state = app.state::<AppState>();
    Ok(crate::setup::snapshot(&state, &settings))
}

async fn prepare_system_input(app: &AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let parent = if crate::platform::status().permission_required {
        Some(crate::platform::portal_parent::Parent::export(window).await?)
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let _ = (app, window);
    #[cfg(target_os = "linux")]
    let _ = app;
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "linux")]
        let id = parent.as_ref().map(|p| p.id.as_str()).unwrap_or("");
        #[cfg(not(target_os = "linux"))]
        let id = "";
        crate::platform::prepare_with_parent(id)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn prepare_desktop(
    app: AppHandle,
    window: tauri::WebviewWindow,
) -> Result<crate::platform::DesktopStatus, String> {
    setup_permissions(app, window).await?;
    Ok(crate::platform::status())
}

#[tauri::command]
pub fn engine_probe() -> Result<engine::ProbeStatus, String> {
    engine::probe_status()
}

#[tauri::command]
pub fn start_dictation(app: AppHandle, state: State<'_, AppState>) -> Result<u64, String> {
    start_dictation_from(app, state, true)
}

pub(crate) fn start_dictation_from(
    app: AppHandle,
    state: State<'_, AppState>,
    wait_for_focus: bool,
) -> Result<u64, String> {
    let mut guard = state
        .session
        .lock()
        .map_err(|_| "session lock poisoned".to_string())?;

    if state.shortcut_capture.load(Ordering::Acquire) {
        return Err("Finish recording the shortcut before starting dictation.".into());
    }
    if let Some(handle) = guard.as_ref()
        && handle.running.load(Ordering::Acquire)
    {
        return Err("dictation is already running".into());
    }
    if let Some(handle) = guard.take() {
        let _ = handle.thread.join();
    }

    {
        let settings = state.settings.lock().map_err(|_| "Settings lock failed")?;
        crate::setup::ensure_ready(&state, &settings)?;
    }
    let model_ready = *state
        .model_status
        .lock()
        .map_err(|_| "Model status lock failed")?
        == crate::app::ModelStatus::Ready;
    if !model_ready {
        crate::live::preload(&app);
        return Err("The model is not ready yet. Loading has been requested; wait for the green Ready indicator before dictating.".into());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let cancel = Arc::new(AtomicBool::new(false));
    let abort = transcribe_cpp::CancelToken::new();
    let running = Arc::new(AtomicBool::new(true));
    let settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned")?
        .clone();
    settings.validate()?;
    {
        let api = state.api.lock().map_err(|_| "API lock poisoned")?;
        state.interactive_wanted.store(true, Ordering::Release);
        if let Some(api) = api.as_ref() {
            api.preempt();
        }
    }
    let id = state.next_session.fetch_add(1, Ordering::AcqRel) + 1;
    let cancel_registered = crate::shortcuts::register_cancel(&app, &settings.cancel_key);
    let thread_stop = stop.clone();
    let thread_cancel = cancel.clone();
    let thread_app = app.clone();
    let (thread_abort, thread_running) = (abort.clone(), running.clone());
    let thread = std::thread::Builder::new()
        .name("dictation".into())
        .spawn(move || {
            live::run(
                thread_app,
                thread_stop,
                thread_cancel,
                thread_abort,
                thread_running,
                settings,
                cancel_registered,
                wait_for_focus,
            )
        })
        .map_err(|e| {
            state.interactive_wanted.store(false, Ordering::Release);
            if cancel_registered && let Ok(settings) = state.settings.lock() {
                crate::shortcuts::unregister_cancel(&app, &settings.cancel_key);
            }
            e.to_string()
        })?;
    *guard = Some(SessionHandle {
        stop,
        cancel,
        abort,
        running,
        thread,
    });
    Ok(id)
}

#[tauri::command]
pub fn cancel_dictation(state: State<'_, AppState>) -> Result<(), String> {
    let guard = state.session.lock().map_err(|_| "session lock poisoned")?;
    if let Some(handle) = guard.as_ref() {
        handle.cancel.store(true, Ordering::Relaxed);
        handle.stop.store(true, Ordering::Relaxed);
        handle.abort.cancel();
    }
    Ok(())
}

pub fn settings_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("settings.json"))
}

#[tauri::command]
pub fn get_settings(
    _app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::settings::Settings, String> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned")?;
    Ok(settings.clone())
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: crate::settings::Settings,
) -> Result<(), String> {
    let session = state.session.lock().map_err(|_| "session lock poisoned")?;
    if state.setup_busy.load(Ordering::Acquire) {
        return Err("Finish the permission check before changing settings.".into());
    }
    if session
        .as_ref()
        .is_some_and(|s| s.running.load(Ordering::Acquire))
    {
        return Err("stop dictation before changing settings".into());
    }
    let mut current = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned")?;
    settings.validate()?;
    crate::shortcuts::validate(&settings.shortcut)?;
    let old = current.clone();
    let api_changed = (
        old.advanced.api_enabled,
        old.advanced.api_port,
        &old.advanced.api_token,
        old.advanced.api_max_seconds,
    ) != (
        settings.advanced.api_enabled,
        settings.advanced.api_port,
        &settings.advanced.api_token,
        settings.advanced.api_max_seconds,
    ) || old.model_path != settings.model_path
        || old.advanced.backend != settings.advanced.backend
        || old.advanced.stream_preset != settings.advanced.stream_preset;
    let mut api = state.api.lock().map_err(|_| "API lock poisoned")?;
    let path = settings_path(&app)?;
    if state
        .setup_error
        .lock()
        .map_err(|_| "setup lock poisoned")?
        .is_some()
        && path.exists()
    {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis();
        std::fs::copy(&path, path.with_extension(format!("invalid-{stamp}.json")))
            .map_err(|e| e.to_string())?;
    }
    let mut shortcut_applied = false;
    let result = (|| {
        if settings.advanced.desktop_enabled && !old.advanced.desktop_enabled {
            // Register before the user returns to the destination window.
            crate::platform::initialize_desktop()?;
        }
        crate::shortcuts::apply(&app, &old.shortcut, &settings.shortcut)?;
        shortcut_applied = true;
        if api_changed {
            configure_api(&app, &mut api, &settings)?;
        }
        settings.save(&path)
    })();
    if let Err(mut error) = result {
        if shortcut_applied
            && let Err(rollback) = crate::shortcuts::apply(&app, &settings.shortcut, &old.shortcut)
        {
            error.push_str(&format!("; shortcut recovery failed: {rollback}"));
        }
        if api_changed && let Err(rollback) = configure_api(&app, &mut api, &old) {
            error.push_str(&format!("; API recovery failed: {rollback}"));
        }
        return Err(error);
    }
    if old.model_path != settings.model_path || old.advanced.backend != settings.advanced.backend {
        if let Ok(mut cache) = state.engine.try_lock() {
            cache.engine = None;
        }
        // Even when an API call still owns the cache, Ready must not describe
        // the old model/backend after settings have changed.
        crate::app::set_model_status(&app, crate::app::ModelStatus::Missing, None);
    }
    let x11_keyboard = settings.advanced.x11_keyboard_enabled;
    *current = settings;
    crate::platform::set_x11_keyboard(x11_keyboard);
    *state
        .setup_error
        .lock()
        .map_err(|_| "setup lock poisoned")? = None;
    Ok(())
}

fn configure_api(
    app: &AppHandle,
    slot: &mut Option<crate::host_api::ApiServer>,
    settings: &crate::settings::Settings,
) -> Result<(), String> {
    if let Some(server) = slot.take() {
        server.shutdown();
    }
    if settings.advanced.api_enabled {
        *slot = Some(crate::host_api::ApiServer::start(
            app.clone(),
            settings.clone(),
        )?);
    }
    Ok(())
}

/// Saves the transcript as UTF-8 text in the user's Documents folder.
#[tauri::command]
pub fn save_transcript(app: AppHandle, text: String, filename: String) -> Result<String, String> {
    if text.trim().is_empty() || text.len() > 4 * 1024 * 1024 {
        return Err("transcript is empty or too large".into());
    }
    if !filename.ends_with(".txt")
        || !filename
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("invalid transcript file name".into());
    }
    let directory = app
        .path()
        .document_dir()
        .or_else(|_| app.path().app_data_dir())
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let path = directory.join(filename);
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

#[derive(serde::Serialize)]
pub struct ModelChoice {
    pub id: String,
    pub name: String,
    pub path: String,
    pub installed: bool,
}

/// Models offered in Settings. Only the pinned Parakeet model is supported.
#[tauri::command]
pub fn model_catalogue(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ModelChoice>, String> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned")?
        .clone();
    let path = match settings.model_path.trim() {
        "" => crate::model_store::default_path(&app)?,
        selected => std::path::PathBuf::from(selected),
    };
    let installed = std::fs::metadata(&path)
        .is_ok_and(|metadata| metadata.len() == crate::engine::DEFAULT_MODEL_SIZE);
    Ok(vec![ModelChoice {
        id: "parakeet-unified-en-0.6b".into(),
        name: crate::model_store::MODEL_NAME.into(),
        path: path.display().to_string(),
        installed,
    }])
}

#[tauri::command]
pub fn input_devices() -> Result<Vec<crate::mic::InputDevice>, String> {
    crate::mic::devices()
}

#[tauri::command]
pub fn model_status(state: State<'_, AppState>) -> serde_json::Value {
    let name = state.loaded_model.lock().ok().and_then(|name| name.clone());
    let model_state = state
        .model_status
        .lock()
        .map(|status| status.as_str().to_string())
        .unwrap_or_else(|_| "error".into());
    let snapshot = state.snapshot.lock().ok().map(|s| s.clone());
    serde_json::json!({"loaded_model":name,"model_state":model_state,"snapshot":snapshot})
}

#[tauri::command]
pub fn preload_model(app: AppHandle) {
    crate::live::preload(&app);
}

#[tauri::command]
pub fn new_api_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[tauri::command]
pub fn download_model(app: AppHandle) -> Result<(), String> {
    crate::model_store::start(&app)
}

#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(download) = state
        .download
        .lock()
        .map_err(|_| "download lock poisoned")?
        .as_ref()
    {
        download.cancel.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub async fn verify_model_file(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::model_store::verified(std::path::Path::new(&path))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn unload_model(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut cache = state.engine.try_lock().map_err(|_| "model is in use")?;
    cache.engine = None;
    drop(cache);
    // The one intentional path to "no model loaded".
    crate::app::set_model_status(&app, crate::app::ModelStatus::Missing, None);
    Ok(())
}

#[tauri::command]
pub fn reset_settings(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::settings::Settings, String> {
    let old = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned")?
        .clone();
    let mut settings = crate::settings::Settings {
        model_path: old.model_path,
        ..Default::default()
    };
    settings.advanced.api_token = new_api_token()?;
    save_settings(app, state, settings.clone())?;
    Ok(settings)
}

#[tauri::command]
pub fn stop_dictation(state: State<'_, AppState>) -> Result<(), String> {
    let guard = state
        .session
        .lock()
        .map_err(|_| "session lock poisoned".to_string())?;
    match guard.as_ref() {
        Some(handle) => {
            handle.stop.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err("dictation is not running".into()),
    }
}

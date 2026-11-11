#[allow(dead_code)]
mod app;
pub mod audio;
pub mod capture;
pub mod cue;
#[allow(dead_code)]
mod delivery;
mod desktop;
pub mod engine;
mod gate;
#[allow(dead_code)]
mod host_api;
mod live;
pub mod mic;
mod model_store;
#[allow(dead_code)]
mod platform;
pub mod segmenter;
pub mod segments;
#[allow(dead_code)]
mod session;
#[allow(dead_code)]
mod settings;
mod setup;
mod shortcuts;
#[allow(dead_code)]
mod submit;
#[allow(dead_code)]
mod target;
mod ui;

pub fn run() {
    use tauri::{Emitter, Manager};
    let app = tauri::Builder::default()
        .manage(app::AppState::default())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let handle = app.handle();
            app::trace(format_args!("startup version={} build={}", app::VERSION, app::BUILD_ID));
            let state = app.state::<app::AppState>();
            let path = ui::settings_path(handle)?;
            let (mut settings, migrated) = match settings::Settings::load_with_migration(&path) {
                Ok(loaded) => loaded,
                Err(error) => {
                    *state.setup_error.lock().map_err(|_| "setup lock poisoned")? = Some(format!("Settings could not be loaded: {error}. Defaults are active; saving will preserve a backup."));
                    (settings::Settings::default(), false)
                }
            };
            // Persist an upgrade so the file matches the effective behavior.
            if migrated && let Err(error) = settings.save(&path) {
                eprintln!("arxell: could not persist migrated settings: {error}");
            }
            if (!path.exists() || state.setup_error.lock().map_err(|_| "setup lock poisoned")?.is_some()) && std::env::var_os("ARXELL_MODEL").is_none() {
                settings.model_path = model_store::default_path(handle)?.to_string_lossy().into_owned();
            }
            if settings.advanced.api_token.is_empty() { settings.advanced.api_token = ui::new_api_token()?; }
            if let Err(error) = shortcuts::apply(handle, "", &settings.shortcut) {
                *state.setup_error.lock().map_err(|_| "setup lock poisoned")? = Some(error);
                // Keep the configured feature required: failed authorization
                // must not silently turn it off and allow a false Ready state.
            }
            if settings.advanced.desktop_enabled && let Err(error) = platform::initialize_desktop() {
                eprintln!("arxell: desktop accessibility setup failed: {error}");
            }
            if settings.advanced.api_enabled {
                match host_api::ApiServer::start(handle.clone(), settings.clone()) {
                    Ok(api) => *state.api.lock().map_err(|_| "API lock poisoned")? = Some(api),
                    Err(error) => { *state.setup_error.lock().map_err(|_| "setup lock poisoned")? = Some(error); settings.advanced.api_enabled = false; }
                }
            }
            let x11_keyboard = settings.advanced.x11_keyboard_enabled;
            *state.settings.lock().map_err(|_| "settings lock poisoned")? = settings;
            crate::platform::set_x11_keyboard(x11_keyboard);
            // First run with the curated default model: fetch it automatically
            // so dictation works without a trip to Settings. A user-chosen
            // custom model path is never replaced (see model_store::auto_fetch).
            if let Err(error) = model_store::auto_fetch(handle) {
                app::trace(format_args!("model auto-fetch not started: {error}"));
            }
            // The app should normally never sit without a loaded model.
            live::preload(handle);
            let handle = handle.clone();
            std::thread::Builder::new().name("idle-unload".into()).spawn(move || {
                let state = handle.state::<app::AppState>();
                while !state.exiting.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let timeout = match state.settings.lock() { Ok(s) => s.advanced.idle_unload_seconds, Err(_) => break };
                    if timeout == 0 || state.interactive_wanted.load(std::sync::atomic::Ordering::Acquire) { continue; }
                    if let Ok(mut cache) = state.engine.try_lock()
                        && cache.engine.is_some() && cache.last_used.is_some_and(|t| t.elapsed().as_secs() >= timeout) {
                            cache.engine = None;
                            drop(cache);
                            app::set_model_status(&handle, app::ModelStatus::Missing, None);
                    }
                }
            })?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Do not leave dictation shortcuts suppressed after a lost WebView,
            // window switch, minimize, or close during shortcut recording.
            if matches!(event, tauri::WindowEvent::Focused(false) | tauri::WindowEvent::Destroyed) {
                shortcuts::end_capture(&window.state::<app::AppState>());
                let _ = window.emit("shortcut-capture-cancelled", ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            ui::app_info,
            ui::engine_probe,
            ui::start_dictation,
            ui::stop_dictation,
            ui::cancel_dictation,
            ui::get_settings,
            ui::save_settings,
            ui::input_devices,
            ui::save_transcript,
            ui::model_catalogue,
            ui::model_status,
            ui::preload_model,
            ui::new_api_token,
            ui::download_model,
            ui::cancel_download,
            ui::verify_model_file,
            ui::unload_model,
            ui::reset_settings,
            ui::prepare_desktop,
            ui::desktop_status,
            ui::setup_status,
            ui::setup_permissions,
            ui::reauthorize_shortcut,
            ui::begin_shortcut_capture,
            ui::end_shortcut_capture
        ])
        .build(tauri::generate_context!())
        .expect("error while running Arxell Speak");
    app.run(|handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let state = handle.state::<app::AppState>();
            state
                .exiting
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Ok(session) = state.session.lock()
                && let Some(session) = session.as_ref()
            {
                session
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                session
                    .stop
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                session.abort.cancel();
            }
            if let Ok(api) = state.api.lock()
                && let Some(api) = api.as_ref()
            {
                api.stop();
            }
            if let Ok(download) = state.download.lock()
                && let Some(download) = download.as_ref()
            {
                download
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });
}

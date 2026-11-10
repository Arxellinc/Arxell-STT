//! User-authorized Wayland input. No uinput/root service or clipboard access.
use futures_util::StreamExt;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::{DynamicType, OwnedObjectPath, OwnedValue, Value},
};

pub const SERVICE: &str = "org.freedesktop.portal.Desktop";
pub const PATH: &str = "/org/freedesktop/portal/desktop";
pub const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
pub const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
static TOKENS: AtomicU64 = AtomicU64::new(1);
static KEYBOARD: Mutex<Option<Arc<Session>>> = Mutex::new(None);

pub fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}
pub fn token() -> String {
    format!(
        "arxell_{}_{}",
        std::process::id(),
        TOKENS.fetch_add(1, Ordering::Relaxed)
    )
}
fn ensure_identity() -> Result<(), String> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".local/share"))
        })
        .ok_or("No user application-data directory is available")?;
    let filename = "dev.arxell.speak.desktop";
    let system =
        std::env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let directory = home.join("applications");
    let target = directory.join(filename);
    let previous = match std::fs::read_to_string(&target) {
        Ok(text) if !text.contains("X-Arxell-Portal-Identity=true") => return Ok(()),
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    if previous.is_none()
        && std::env::split_paths(&system).any(|p| p.join("applications").join(filename).is_file())
    {
        return Ok(());
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = executable.to_str().ok_or("Executable path is not UTF-8")?;
    if path.chars().any(char::is_control) {
        return Err("Executable path contains control characters".into());
    }
    let path = path
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%");
    let entry = include_str!("../../resources/dev.arxell.speak.desktop")
        .replace("Exec=arxell-speak", &format!("Exec=\"{path}\""));
    if previous.as_deref() == Some(entry.as_str()) {
        return Ok(());
    }
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    // Hidden metadata, not a second launcher. Only refresh records marked as
    // ours, to keep a moved/upgraded executable valid. No permissions granted.
    let temporary = directory.join(format!(".{filename}.{}", token()));
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(entry.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| e.to_string())
}

pub fn connection() -> Result<Connection, String> {
    ensure_identity()?;
    let connection = Builder::session()
        .map_err(|e| e.to_string())?
        .method_timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    // Unsandboxed/dev launches do not necessarily have a systemd application
    // scope. Register THIS connection before any portal method, otherwise the
    // desktop can reject GlobalShortcuts/RemoteDesktop with "app id required".
    let registry = Proxy::new(
        &connection,
        SERVICE,
        PATH,
        "org.freedesktop.host.portal.Registry",
    )
    .map_err(|e| e.to_string())?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let result: Result<(), _> = registry.call("Register", &("dev.arxell.speak", options));
    if let Err(error) = result {
        let message = error.to_string();
        // Older portals and sandboxed launches derive their own identity.
        if !message.contains("UnknownInterface")
            && !message.contains("UnknownMethod")
            && !message.contains("sandbox")
        {
            return Err(format!(
                "Desktop application registration failed: {message}"
            ));
        }
    }
    drop(registry);
    Ok(connection)
}

// Human consent has no app-imposed deadline. Still stop on a closed response
// stream or portal service loss, rather than leaving setup permanently busy.
async fn await_response<T>(
    response: impl std::future::Future<Output = Option<T>>,
    disconnected: impl std::future::Future<Output = ()>,
) -> Result<T, String> {
    tokio::select! {
        biased;
        // A simultaneous service loss must never be mistaken for usable consent.
        _ = disconnected => Err("The desktop permission service disconnected or restarted. Retry setup.".into()),
        message = response => message.ok_or_else(|| "The desktop permission request closed without a response. Retry setup.".into()),
    }
}

/// Subscribe BEFORE sending the method (the portal may answer immediately).
/// Wait for approval/cancellation or service loss, never a consent deadline.
pub fn request<B: Serialize + DynamicType>(
    conn: &Connection,
    interface: &str,
    method: &str,
    handle_token: &str,
    body: &B,
) -> Result<HashMap<String, OwnedValue>, String> {
    crate::app::trace(format_args!(
        "portal request interface={interface} method={method}"
    ));
    tauri::async_runtime::block_on(async {
        let sender = conn
            .unique_name()
            .ok_or("Portal connection has no unique name")?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{handle_token}");
        let request = zbus::Proxy::new(
            conn.inner(),
            SERVICE,
            path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .await
        .map_err(|e| e.to_string())?;
        let mut responses = request
            .receive_signal("Response")
            .await
            .map_err(|e| e.to_string())?;
        let portal = zbus::Proxy::new(conn.inner(), SERVICE, PATH, interface)
            .await
            .map_err(|e| e.to_string())?;
        let mut owners = portal
            .receive_owner_changed()
            .await
            .map_err(|e| e.to_string())?;
        let returned: OwnedObjectPath =
            portal.call(method, body).await.map_err(|e| e.to_string())?;
        if returned.as_str() != path {
            return Err("Portal returned an unexpected request path".into());
        }
        let response = await_response(responses.next(), async {
            let _ = owners.next().await;
        })
        .await;
        match response {
            Ok(message) => {
                let (code, results): (u32, HashMap<String, OwnedValue>) =
                    message.body().deserialize().map_err(|e| e.to_string())?;
                crate::app::trace(format_args!("portal response method={method} code={code}"));
                match code {
                    0 => Ok(results),
                    1 => Err("The system permission dialog was cancelled; external typing remains disabled.".into()),
                    _ => Err(format!("The desktop could not complete its permission request (code {code}); access is not verified.")),
                }
            }
            Err(error) => {
                let _: Result<(), _> = request.call("Close", &()).await;
                crate::app::trace(format_args!(
                    "portal response method={method} outcome=unavailable"
                ));
                Err(error)
            }
        }
    })
}

pub struct Session {
    pub connection: Connection,
    pub path: OwnedObjectPath,
    closed: Arc<AtomicBool>,
}
impl Session {
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(proxy) = Proxy::new(
            &self.connection,
            SERVICE,
            self.path.as_str(),
            "org.freedesktop.portal.Session",
        ) {
            let _: Result<(), _> = proxy.call("Close", &());
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}
pub fn create_session(interface: &str) -> Result<Session, String> {
    let connection = connection()?;
    let handle = token();
    let session_token = token();
    let options = HashMap::from([
        ("handle_token", Value::from(handle.as_str())),
        ("session_handle_token", Value::from(session_token.as_str())),
    ]);
    let mut response = request(
        &connection,
        interface,
        "CreateSession",
        &handle,
        &(options,),
    )?;
    let path = response
        .remove("session_handle")
        .ok_or("Portal did not return a session")?;
    // CreateSession's legacy response encodes session_handle as a string.
    let object_path = match OwnedObjectPath::try_from(path.try_clone().map_err(|e| e.to_string())?)
    {
        Ok(path) => path,
        Err(_) => {
            let path: String = path
                .try_into()
                .map_err(|_| "Portal returned an invalid session handle")?;
            OwnedObjectPath::try_from(path).map_err(|e| e.to_string())?
        }
    };
    Ok(Session {
        connection,
        path: object_path,
        closed: Arc::new(AtomicBool::new(false)),
    })
}
pub fn keyboard_ready() -> bool {
    KEYBOARD.lock().is_ok_and(|s| s.is_some())
}
pub fn keyboard() -> Result<Arc<Session>, String> {
    KEYBOARD
        .lock()
        .map_err(|_| "Keyboard permission state failed")?
        .clone()
        .ok_or_else(|| {
            "Grant keyboard access in Settings → Shortcuts & delivery before typing into Electron/browser editors or terminals on Wayland."
                .into()
        })
}
fn watch_keyboard(session: &Arc<Session>) -> Result<(), String> {
    // zbus async stream destructors schedule match-rule removal. They must also
    // be dropped with a Tokio context, including thread-spawn failure paths.
    let runtime = tauri::async_runtime::handle();
    let _context = runtime.inner().enter();
    let proxy: Proxy<'static> = Proxy::new_owned(
        session.connection.clone(),
        SERVICE,
        session.path.clone(),
        "org.freedesktop.portal.Session",
    )
    .map_err(|e| e.to_string())?;
    let closed = tauri::async_runtime::block_on(proxy.inner().receive_signal("Closed"))
        .map_err(|e| e.to_string())?;
    let owners = tauri::async_runtime::block_on(proxy.inner().receive_owner_changed())
        .map_err(|e| e.to_string())?;
    let weak = Arc::downgrade(session);
    let locally_closed = session.closed.clone();
    std::thread::Builder::new()
        .name("keyboard-permission".into())
        .spawn(move || {
            let runtime = tauri::async_runtime::handle();
            let _context = runtime.inner().enter();
            // Move captures into locals so streams drop BEFORE the context.
            let mut closed = closed;
            let mut owners = owners;
            while !locally_closed.load(Ordering::Acquire) {
                let expired = tauri::async_runtime::block_on(async {
                    tokio::select! {
                        _ = closed.next() => true,
                        _ = owners.next() => true,
                        _ = tokio::time::sleep(Duration::from_millis(100)) => false,
                    }
                });
                if expired {
                    break;
                }
            }
            if let Some(expired) = weak.upgrade()
                && let Ok(mut slot) = KEYBOARD.lock()
                && slot
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(active, &expired))
            {
                *slot = None;
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn prepare_keyboard() -> Result<(), String> {
    prepare_keyboard_with_parent("")
}

pub fn prepare_keyboard_with_parent(parent: &str) -> Result<(), String> {
    if !wayland() {
        return Ok(());
    }
    if keyboard_ready() {
        return Ok(());
    }
    let session = create_session(REMOTE)?;
    let handle = token();
    let options = HashMap::from([
        ("handle_token", Value::from(handle.as_str())),
        ("types", Value::from(1u32)),
    ]);
    request(
        &session.connection,
        REMOTE,
        "SelectDevices",
        &handle,
        &(&session.path, options),
    )?;
    let handle = token();
    let options = HashMap::from([("handle_token", Value::from(handle.as_str()))]);
    let response = request(
        &session.connection,
        REMOTE,
        "Start",
        &handle,
        &(&session.path, parent, options),
    )?;
    let devices: u32 = response
        .get("devices")
        .and_then(|v| v.try_into().ok())
        .unwrap_or(0);
    if devices & 1 == 0 {
        return Err("The desktop did not authorize keyboard access.".into());
    }
    let session = Arc::new(session);
    watch_keyboard(&session)?;
    *KEYBOARD
        .lock()
        .map_err(|_| "Keyboard permission state failed")? = Some(session);
    Ok(())
}
pub fn type_character(session: &Session, c: char) -> Result<(), String> {
    if c.is_control() {
        return Err("Control character refused.".into());
    }
    let code = c as u32;
    let keysym = if code <= 0xff {
        code
    } else {
        0x0100_0000 | code
    } as i32;
    let proxy =
        Proxy::new(&session.connection, SERVICE, PATH, REMOTE).map_err(|e| e.to_string())?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let pressed: Result<(), _> = proxy.call(
        "NotifyKeyboardKeysym",
        &(&session.path, &options, keysym, 1u32),
    );
    // Always try to release, including when the press acknowledgement is lost.
    let released: Result<(), _> = proxy.call(
        "NotifyKeyboardKeysym",
        &(&session.path, &options, keysym, 0u32),
    );
    if pressed.is_err() || released.is_err() {
        *KEYBOARD
            .lock()
            .map_err(|_| "Keyboard permission state failed")? = None;
        return Err("Wayland keyboard access was revoked or delivery was not acknowledged. Grant access again in Settings.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn consent_has_no_application_deadline() {
        let (answer, response) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn(await_response(
            async { response.await.ok() },
            std::future::pending(),
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(600)).await;
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "No approval deadline, even after ten virtual minutes"
        );
        answer.send("approved").unwrap();
        assert_eq!(waiting.await.unwrap().unwrap(), "approved");
    }

    #[tokio::test]
    async fn a_closed_request_or_lost_service_does_not_hang_setup() {
        let closed = await_response(std::future::ready(None::<()>), std::future::pending()).await;
        assert!(closed.unwrap_err().contains("closed without a response"));
        let disconnected =
            await_response(std::future::pending::<Option<()>>(), std::future::ready(())).await;
        assert!(
            disconnected
                .unwrap_err()
                .contains("disconnected or restarted")
        );
        let lost_during_approval =
            await_response(std::future::ready(Some("approved")), std::future::ready(())).await;
        assert!(lost_during_approval.is_err());
    }

    #[test]
    #[ignore = "Requires a desktop portal; creates inert sessions only, no permission or input"]
    fn desktop_portal_sessions_can_be_created_and_closed() {
        for interface in [REMOTE, SHORTCUTS] {
            let session = create_session(interface).expect("create inert portal session");
            assert!(
                session
                    .path
                    .as_str()
                    .starts_with("/org/freedesktop/portal/desktop/session/")
            );
            let session = Arc::new(session);
            if interface == REMOTE {
                *KEYBOARD.lock().unwrap() = Some(session.clone());
                watch_keyboard(&session).unwrap();
                session.close();
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while keyboard_ready() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Closed permission session was not cleared"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            } else {
                session.close();
            }
        }
    }
}

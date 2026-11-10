//! An exported, visible parent for Wayland system permission dialogs. GTK
//! access and unexport are always on the UI thread; no raw pointer escapes it.
use gdk_wayland_sys as gdkwayland_sys;
use gtk::{glib::translate::ToGlibPtr, prelude::*};
use std::{
    ffi::{CStr, c_char, c_void},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::WebviewWindow;
use tokio::sync::oneshot;

type Reply = Option<oneshot::Sender<Result<String, String>>>;

pub struct Parent {
    pub id: String,
    window: WebviewWindow,
    exported: Arc<AtomicBool>,
}
impl Parent {
    pub async fn export(window: WebviewWindow) -> Result<Self, String> {
        let exported = Arc::new(AtomicBool::new(false));
        let mut parent = Self {
            id: String::new(),
            window: window.clone(),
            exported: exported.clone(),
        };
        if !super::portal::wayland() {
            return Ok(parent);
        }
        let (reply, response) = oneshot::channel();
        let native_window = window.clone();
        window.run_on_main_thread(move || {
            let _ = native_window.unminimize();
            let _ = native_window.show();
            let _ = native_window.set_focus();
            let native = native_window.gtk_window().ok().and_then(|window| window.window());
            let Some(native) = native else {
                let _ = reply.send(Err("The application window is not available for the system permission dialog.".into()));
                return;
            };
            if native.type_().name() != "GdkWaylandWindow" {
                let _ = reply.send(Err("A Wayland parent window could not be exported. Check the application display backend before requesting keyboard access.".into()));
                return;
            }
            let data = Box::into_raw(Box::new(Some(reply)));
            let native_ptr: *mut gtk::gdk::ffi::GdkWindow = native.to_glib_none().0;
            // SAFETY: `native` is a live GdkWaylandWindow on the GTK thread.
            // GTK owns the boxed sender only on success and calls destroy once.
            let ok = unsafe { gdkwayland_sys::gdk_wayland_window_export_handle(
                native_ptr.cast(), Some(exported_handle), data.cast(), Some(destroy_reply)
            ) };
            if ok == 0 {
                // SAFETY: failed export did not take ownership of user_data.
                let mut reply: Box<Reply> = unsafe { Box::from_raw(data) };
                if let Some(reply) = reply.take() { let _ = reply.send(Err("The desktop could not export a parent for its permission dialog.".into())); }
            } else { exported.store(true, Ordering::Release); }
        }).map_err(|e| e.to_string())?;
        parent.id = tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|_| "Exporting the system dialog parent timed out.")?
            .map_err(|_| "The permission dialog parent was closed.")??;
        crate::app::trace(format_args!("portal parent exported=true"));
        Ok(parent)
    }
}
unsafe extern "C" fn exported_handle(
    _: *mut gdkwayland_sys::GdkWaylandWindow,
    handle: *const c_char,
    data: *mut c_void,
) {
    // SAFETY: GTK supplies a NUL-terminated handle valid for this callback and
    // the sender allocated above, on the same UI thread. Never log the token.
    let reply = unsafe { &mut *data.cast::<Reply>() };
    if let Some(reply) = reply.take() {
        let result = if handle.is_null() {
            Err("The desktop returned no dialog parent handle.".into())
        } else {
            unsafe { CStr::from_ptr(handle) }
                .to_str()
                .map(|handle| format!("wayland:{handle}"))
                .map_err(|e| e.to_string())
        };
        let _ = reply.send(result);
    }
}
unsafe extern "C" fn destroy_reply(data: *mut c_void) {
    // SAFETY: this is GTK's sole destroy callback for the successful export.
    drop(unsafe { Box::from_raw(data.cast::<Reply>()) });
}
impl Drop for Parent {
    fn drop(&mut self) {
        let exported = self.exported.clone();
        let window = self.window.clone();
        let _ = self.window.run_on_main_thread(move || {
            if exported.swap(false, Ordering::AcqRel)
                && let Some(native) = window.gtk_window().ok().and_then(|window| window.window())
            {
                let native_ptr: *mut gtk::gdk::ffi::GdkWindow = native.to_glib_none().0;
                // SAFETY: same GTK thread and live window as export above.
                unsafe {
                    gdkwayland_sys::gdk_wayland_window_unexport_handle(native_ptr.cast());
                }
            }
        });
    }
}

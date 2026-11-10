//! Declare an assistive client interested in window activation. Chromium needs
//! this registration before it reliably exposes foreground/focus state. Merely
//! polling GetState does not activate the same toolkit accessibility behavior.
//! No window titles, field contents or event bodies are inspected or retained.
use futures_util::StreamExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use zbus::{
    MatchRule, MessageStream,
    blocking::{Proxy, connection::Builder},
    message::Type,
};

static STARTED: AtomicBool = AtomicBool::new(false);

pub fn start() -> Result<(), String> {
    if STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let (ready, receiver) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("desktop-focus".into())
        .spawn(move || {
            if let Err(error) = listen(&ready) {
                let _ = ready.send(Err(error));
            }
            STARTED.store(false, Ordering::Release);
        });
    if let Err(error) = spawned {
        STARTED.store(false, Ordering::Release);
        return Err(error.to_string());
    }
    receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|e| e.to_string())?
}

fn listen(ready: &mpsc::SyncSender<Result<(), String>>) -> Result<(), String> {
    let runtime = tauri::async_runtime::handle();
    let _context = runtime.inner().enter();
    let session = Builder::session()
        .map_err(|e| e.to_string())?
        .method_timeout(Duration::from_millis(500))
        .build()
        .map_err(|e| e.to_string())?;
    let bus = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")
        .map_err(|e| e.to_string())?;
    let address: String = bus.call("GetAddress", &()).map_err(|e| e.to_string())?;
    let connection = Builder::address(address.as_str())
        .map_err(|e| e.to_string())?
        .method_timeout(Duration::from_millis(500))
        .build()
        .map_err(|e| e.to_string())?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface("org.a11y.atspi.Event.Window")
        .map_err(|e| e.to_string())?
        .build();
    let mut stream = tauri::async_runtime::block_on(MessageStream::for_match_rule(
        rule,
        connection.inner(),
        Some(64),
    ))
    .map_err(|e| e.to_string())?;
    let registry = Proxy::new(
        &connection,
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/registry",
        "org.a11y.atspi.Registry",
    )
    .map_err(|e| e.to_string())?;
    for event in [
        "window:activate",
        "window:deactivate",
        "window:destroy",
        "object:state-changed:focused",
        "focus",
    ] {
        let _: () = registry
            .call("RegisterEvent", &(event, Vec::<String>::new(), ""))
            .map_err(|e| e.to_string())?;
    }
    let _ = ready.send(Ok(()));
    while let Some(message) = tauri::async_runtime::block_on(stream.next()) {
        message.map_err(|e| e.to_string())?;
    }
    Ok(())
}

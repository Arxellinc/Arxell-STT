//! Loopback WebSocket API. Audio only; no desktop-input capability.

use crate::{app::AppState, capture::AudioFrame, engine::StreamHooks, settings::Settings};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tungstenite::{
    Message, WebSocket,
    handshake::server::{ErrorResponse, Request, Response},
};

pub struct ApiServer {
    stop: Arc<AtomicBool>,
    current: Arc<Mutex<Option<transcribe_cpp::CancelToken>>>,
    thread: Option<JoinHandle<()>>,
}

struct HandshakeSocket {
    inner: TcpStream,
    remaining: Option<usize>,
    deadline: Instant,
}

impl Read for HandshakeSocket {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let limit = if let Some(remaining) = self.remaining {
            let timeout = self
                .deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake timed out")
                })?;
            if remaining == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "handshake headers too large",
                ));
            }
            self.inner.set_read_timeout(Some(timeout))?;
            remaining.min(buffer.len())
        } else {
            buffer.len()
        };
        let count = self.inner.read(&mut buffer[..limit])?;
        if let Some(remaining) = self.remaining.as_mut() {
            *remaining -= count;
        }
        Ok(count)
    }
}
impl Write for HandshakeSocket {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl ApiServer {
    pub fn start(app: AppHandle, settings: Settings) -> Result<Self, String> {
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, settings.advanced.api_port))
                .map_err(|e| format!("API: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(None::<transcribe_cpp::CancelToken>));
        let worker_stop = stop.clone();
        let worker_current = current.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let thread = std::thread::Builder::new()
            .name("local-api".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            if count.load(Ordering::Relaxed) >= 4 {
                                continue;
                            }
                            count.fetch_add(1, Ordering::Relaxed);
                            let (app, settings, stop, current, count) = (
                                app.clone(),
                                settings.clone(),
                                worker_stop.clone(),
                                worker_current.clone(),
                                count.clone(),
                            );
                            if std::thread::Builder::new()
                                .name("api-client".into())
                                .spawn(move || {
                                    let _ = serve(socket, app, settings, stop, current);
                                    count.fetch_sub(1, Ordering::Relaxed);
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            current,
            thread: Some(thread),
        })
    }

    pub fn preempt(&self) {
        if let Ok(current) = self.current.lock()
            && let Some(token) = current.as_ref()
        {
            token.cancel();
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.preempt();
    }
    pub fn shutdown(mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn authorized(request: &Request, settings: &Settings) -> bool {
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let hosts = [
        format!("127.0.0.1:{}", settings.advanced.api_port),
        format!("localhost:{}", settings.advanced.api_port),
    ];
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    let token = &settings.advanced.api_token;
    let mut mismatch = supplied.len() ^ token.len();
    for (a, b) in supplied.bytes().zip(token.bytes()) {
        mismatch |= (a ^ b) as usize;
    }
    request.uri().path() == "/v1/stream"
        && request.uri().query().is_none()
        && hosts.contains(&host.to_ascii_lowercase())
        && !request.headers().contains_key("origin")
        && token.len() >= 32
        && mismatch == 0
}

fn serve(
    socket: TcpStream,
    app: AppHandle,
    settings: Settings,
    stop: Arc<AtomicBool>,
    current: Arc<Mutex<Option<transcribe_cpp::CancelToken>>>,
) -> Result<(), String> {
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    #[allow(clippy::result_large_err)] // Required by tungstenite's handshake callback.
    let callback = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        if authorized(request, &settings) {
            Ok(response)
        } else {
            Err(tungstenite::http::Response::builder()
                .status(403)
                .body(Some("Forbidden".into()))
                .expect("static response"))
        }
    };
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));
    let socket = HandshakeSocket {
        inner: socket,
        remaining: Some(16 * 1024),
        deadline: Instant::now() + Duration::from_secs(3),
    };
    let mut socket = tungstenite::accept_hdr_with_config(socket, callback, Some(config))
        .map_err(|e| e.to_string())?;
    socket.get_mut().remaining = None;
    socket
        .get_mut()
        .inner
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    let busy = || state.interactive_wanted.load(Ordering::Acquire);
    let mut lease = match state.engine.try_lock() {
        Ok(lease) if !busy() => lease,
        _ => {
            send(
                &mut socket,
                serde_json::json!({"kind":"error","message":"model busy; interactive dictation has priority"}),
            )?;
            return Ok(());
        }
    };
    let token = transcribe_cpp::CancelToken::new();
    *current.lock().map_err(|_| "API lock poisoned")? = Some(token.clone());
    // Recheck after publishing the cancellation handle to close the start race.
    if busy() || stop.load(Ordering::Relaxed) {
        token.cancel();
    }
    let result = (|| {
        if token.is_cancelled() {
            return Err("preempted".into());
        }
        if lease.engine.is_none()
            || lease.path != settings.model_path
            || lease.backend != settings.advanced.backend
        {
            crate::app::set_model_status(&app, crate::app::ModelStatus::Loading, None);
        }
        crate::live::load_cached(&mut lease, &settings)?;
        crate::app::set_model_status(
            &app,
            crate::app::ModelStatus::Ready,
            Some(crate::model_store::MODEL_NAME),
        );
        let engine = lease.engine.as_mut().ok_or("model not loaded")?;
        send(
            &mut socket,
            serde_json::json!({"kind":"ready","sample_rate":16000,"format":"f32le","channels":1}),
        )?;
        let mut hooks = ApiHooks {
            socket: &mut socket,
            samples: VecDeque::new(),
            received: 0,
            seq: 0,
            limit: settings.advanced.api_max_seconds * 16000,
            deadline: Instant::now() + Duration::from_secs(settings.advanced.api_max_seconds + 10),
            last_input: Instant::now(),
            ended: false,
            error: None,
            token: token.clone(),
            stop,
        };
        let outcome =
            engine.run_stream_cancellable(&mut hooks, settings.advanced.stream_preset, &token);
        let error = hooks.error.take();
        drop(hooks);
        if let Some(error) = error {
            return Err(error);
        }
        let outcome = outcome?;
        send(
            &mut socket,
            serde_json::json!({"kind":"final","text":outcome.final_text}),
        )
    })();
    lease.last_used = Some(Instant::now());
    *current.lock().map_err(|_| "API lock poisoned")? = None;
    if let Err(message) = result {
        let _ = send(
            &mut socket,
            serde_json::json!({"kind":"error","message":message}),
        );
    }
    let _ = socket.close(None);
    Ok(())
}

fn send<S: Read + Write>(
    socket: &mut WebSocket<S>,
    value: serde_json::Value,
) -> Result<(), String> {
    socket
        .send(Message::Text(value.to_string().into()))
        .map_err(|e| e.to_string())
}

struct ApiHooks<'a, S: Read + Write> {
    socket: &'a mut WebSocket<S>,
    samples: VecDeque<f32>,
    received: u64,
    seq: u64,
    limit: u64,
    deadline: Instant,
    last_input: Instant,
    ended: bool,
    error: Option<String>,
    token: transcribe_cpp::CancelToken,
    stop: Arc<AtomicBool>,
}

impl<S: Read + Write> StreamHooks for ApiHooks<'_, S> {
    fn cancelled(&self) -> bool {
        self.token.is_cancelled() || self.stop.load(Ordering::Relaxed)
    }
    fn next_frame(&mut self) -> Option<AudioFrame> {
        loop {
            if self.cancelled() {
                return None;
            }
            if Instant::now() > self.deadline || self.last_input.elapsed() > Duration::from_secs(10)
            {
                self.error = Some("audio session timed out".into());
                self.token.cancel();
                return None;
            }
            if self.samples.len() >= 256 || (self.ended && !self.samples.is_empty()) {
                let mut pcm: Vec<f32> = self.samples.drain(..self.samples.len().min(256)).collect();
                pcm.resize(256, 0.0);
                let seq = self.seq;
                self.seq += 1;
                return Some(AudioFrame {
                    seq,
                    t_start_ms: seq * 16,
                    t_end_ms: (seq + 1) * 16,
                    pcm,
                    speech: true,
                });
            }
            if self.ended {
                return None;
            }
            match self.socket.read() {
                Ok(Message::Binary(bytes)) => {
                    self.last_input = Instant::now();
                    if bytes.len() % 4 != 0 || self.received + bytes.len() as u64 / 4 > self.limit {
                        self.error = Some("invalid audio size or duration limit exceeded".into());
                        self.token.cancel();
                        return None;
                    }
                    self.received += bytes.len() as u64 / 4;
                    for chunk in bytes.as_chunks::<4>().0 {
                        let sample = f32::from_le_bytes(*chunk);
                        if !sample.is_finite() || sample.abs() > 1.0 {
                            self.error = Some("audio samples must be finite and normalized".into());
                            self.token.cancel();
                            return None;
                        }
                        self.samples.push_back(sample);
                    }
                }
                Ok(Message::Text(text)) if text.as_str() == "end" => self.ended = true,
                Ok(Message::Text(text)) if text.as_str() == "cancel" => {
                    self.token.cancel();
                    return None;
                }
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Ok(_) | Err(_) => {
                    self.token.cancel();
                    return None;
                }
            }
        }
    }
    fn on_text(&mut self, _ms: i64, committed: &str, tentative: &str) {
        if let Err(error) = send(
            self.socket,
            serde_json::json!({"kind":"interim","committed":committed,"tentative":tentative}),
        ) {
            self.error = Some(error);
            self.token.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_buffer_has_a_hard_byte_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let writer = std::thread::spawn(move || {
            let mut socket = TcpStream::connect(address).unwrap();
            socket.write_all(&vec![b'x'; 20_000]).unwrap();
        });
        let (inner, _) = listener.accept().unwrap();
        let mut socket = HandshakeSocket {
            inner,
            remaining: Some(16384),
            deadline: Instant::now() + Duration::from_secs(3),
        };
        let mut count = 0;
        let mut buffer = [0u8; 4096];
        loop {
            match socket.read(&mut buffer) {
                Ok(n) => {
                    assert_ne!(n, 0);
                    count += n;
                }
                Err(error) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
                    break;
                }
            }
        }
        assert_eq!(count, 16384);
        writer.join().unwrap();
    }

    #[test]
    fn loopback_transport_frames_audio_and_finishes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let tcp = TcpStream::connect(address).unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let (mut socket, _) =
                tungstenite::client(format!("ws://{address}/v1/stream"), tcp).unwrap();
            let bytes: Vec<u8> = (0..384).flat_map(|_| 0.2f32.to_le_bytes()).collect();
            socket.send(Message::Binary(bytes.into())).unwrap();
            socket.send(Message::Text("end".into())).unwrap();
            let text = socket.read().unwrap().into_text().unwrap();
            assert!(text.contains("test interim"));
        });
        let (tcp, _) = listener.accept().unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut socket = tungstenite::accept(tcp).unwrap();
        let mut hooks = ApiHooks {
            socket: &mut socket,
            samples: VecDeque::new(),
            received: 0,
            seq: 0,
            limit: 512,
            deadline: Instant::now() + Duration::from_secs(3),
            last_input: Instant::now(),
            ended: false,
            error: None,
            token: transcribe_cpp::CancelToken::new(),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let first = hooks.next_frame().unwrap();
        assert_eq!((first.t_start_ms, first.t_end_ms), (0, 16));
        assert_eq!(first.pcm, vec![0.2; 256]);
        let last = hooks.next_frame().unwrap();
        assert_eq!(&last.pcm[..128], &[0.2; 128]);
        assert_eq!(&last.pcm[128..], &[0.0; 128]);
        assert!(hooks.next_frame().is_none());
        hooks.on_text(32, "test interim", "");
        assert!(hooks.error.is_none());
        client.join().unwrap();
    }
    #[test]
    fn rejects_wrong_hosts_origins_paths_and_tokens() {
        let mut settings = Settings::default();
        settings.advanced.api_token = "a".repeat(64);
        let request = |host: &str, path: &str, token: &str, origin: bool| {
            let mut builder = tungstenite::http::Request::builder()
                .uri(path)
                .header("host", host)
                .header("authorization", format!("Bearer {token}"));
            if origin {
                builder = builder.header("origin", "https://example.com");
            }
            builder.body(()).unwrap()
        };
        let token = &settings.advanced.api_token;
        assert!(authorized(
            &request("127.0.0.1:14321", "/v1/stream", token, false),
            &settings
        ));
        for request in [
            request("evil.test:14321", "/v1/stream", token, false),
            request("localhost:14321", "/v1/stream", token, true),
            request("localhost:14321", "/v1/stream?token=x", token, false),
            request("localhost:14321", "/v1/stream", "wrong", false),
        ] {
            assert!(!authorized(&request, &settings));
        }
    }
}

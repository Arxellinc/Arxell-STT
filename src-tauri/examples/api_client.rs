//! cargo run --example api_client -- <token> <mono-16k-PCM16.wav> [port]

use std::{
    net::TcpStream,
    time::{Duration, Instant},
};
use tungstenite::{Message, client::IntoClientRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let token = args.get(1).ok_or("supply API token and WAV path")?;
    let path = args.get(2).ok_or("supply mono 16 kHz PCM16 WAV path")?;
    let port: u16 = args.get(3).map_or(Ok(14321), |s| s.parse())?;
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16000 || spec.bits_per_sample != 16 {
        return Err("expected mono 16 kHz PCM16 WAV".into());
    }
    let pcm = reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?;
    let tcp = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(120)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut request = format!("ws://127.0.0.1:{port}/v1/stream").into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    let (mut socket, _) = tungstenite::client(request, tcp)?;
    println!("{}", socket.read()?.into_text()?);
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(20)))?;
    for samples in pcm.chunks(2560) {
        let deadline = Instant::now() + Duration::from_millis(160);
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|s| (*s as f32 / 32768.0).to_le_bytes())
            .collect();
        socket.send(Message::Binary(bytes.into()))?;
        while Instant::now() < deadline {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    println!("{text}");
                    if text.contains("\"kind\":\"error\"") {
                        return Err("API refused request".into());
                    }
                }
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Ok(_) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    socket.send(Message::Text("end".into()))?;
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_secs(30)))?;
    loop {
        if let Message::Text(text) = socket.read()? {
            println!("{text}");
            let event: serde_json::Value = serde_json::from_str(text.as_str())?;
            if event["kind"] == "final" {
                break;
            }
            if event["kind"] == "error" {
                return Err("API failed".into());
            }
        }
    }
    Ok(())
}

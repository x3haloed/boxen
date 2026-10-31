use crate::{
    Result,
    state::{self, State},
};
use serde::{Deserialize, Serialize};
use snow::{HandshakeState, TransportState, params::NoiseParams};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TEXT: usize = 4096;
const PROLOGUE: &[u8] = b"boxen-lan-v1";

pub fn params() -> NoiseParams {
    "Noise_XX_25519_ChaChaPoly_BLAKE2s"
        .parse()
        .expect("constant Noise parameters")
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Reply {
    pub from: String,
    pub message: String,
}
#[derive(Serialize, Deserialize)]
struct Request {
    message: String,
}

fn send(stream: &mut TcpStream, message: &[u8]) -> Result<()> {
    let length = u16::try_from(message.len())?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(message)?;
    Ok(())
}
fn receive(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut length = [0; 2];
    stream.read_exact(&mut length)?;
    let mut message = vec![0; u16::from_be_bytes(length) as usize];
    stream.read_exact(&mut message)?;
    Ok(message)
}
fn handshake_send(stream: &mut TcpStream, noise: &mut HandshakeState) -> Result<()> {
    let mut buffer = [0; 65535];
    let length = noise.write_message(&[], &mut buffer)?;
    send(stream, &buffer[..length])
}
fn handshake_receive(stream: &mut TcpStream, noise: &mut HandshakeState) -> Result<()> {
    noise.read_message(&receive(stream)?, &mut [0; 65535])?;
    Ok(())
}
fn encrypt(stream: &mut TcpStream, transport: &mut TransportState, plain: &[u8]) -> Result<()> {
    let mut buffer = [0; 65535];
    let length = transport.write_message(plain, &mut buffer)?;
    send(stream, &buffer[..length])
}
fn decrypt(stream: &mut TcpStream, transport: &mut TransportState) -> Result<Vec<u8>> {
    let mut buffer = vec![0; 65535];
    let length = transport.read_message(&receive(stream)?, &mut buffer)?;
    buffer.truncate(length);
    Ok(buffer)
}
fn configure(stream: &TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.set_nodelay(true)?;
    Ok(())
}
pub fn ping(state: &State, address: &str, expected: &str, message: &str) -> Result<Reply> {
    state.require_trust(expected)?;
    if message.len() > MAX_TEXT {
        return Err("message exceeds 4096 bytes".into());
    }
    let mut connected = None;
    for address in address.to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(_) => continue,
        }
    }
    let mut stream = connected.ok_or("peer unreachable; no message delivered")?;
    configure(&stream)?;
    let private = state.private_key()?;
    let mut noise = snow::Builder::new(params())
        .prologue(PROLOGUE)?
        .local_private_key(&private)?
        .build_initiator()?;
    handshake_send(&mut stream, &mut noise)?;
    handshake_receive(&mut stream, &mut noise)?;
    if noise.get_remote_static() != Some(state::key(expected)?.as_slice()) {
        return Err("remote identity differs from the selected peer; no message sent".into());
    }
    handshake_send(&mut stream, &mut noise)?;
    let mut transport = noise.into_transport_mode()?;
    encrypt(
        &mut stream,
        &mut transport,
        &serde_json::to_vec(&Request {
            message: message.into(),
        })?,
    )?;
    let reply: Reply = serde_json::from_slice(&decrypt(&mut stream, &mut transport)?)?;
    if reply.from != expected || reply.message != message {
        return Err("invalid peer reply".into());
    }
    Ok(reply)
}
fn handle(mut stream: TcpStream, state: &State) -> Result<()> {
    configure(&stream)?;
    let private = state.private_key()?;
    let mut noise = snow::Builder::new(params())
        .prologue(PROLOGUE)?
        .local_private_key(&private)?
        .build_responder()?;
    handshake_receive(&mut stream, &mut noise)?;
    handshake_send(&mut stream, &mut noise)?;
    handshake_receive(&mut stream, &mut noise)?;
    let remote = state::hex(noise.get_remote_static().ok_or("missing peer identity")?);
    state.require_trust(&remote)?;
    let mut transport = noise.into_transport_mode()?;
    let request: Request = serde_json::from_slice(&decrypt(&mut stream, &mut transport)?)?;
    if request.message.len() > MAX_TEXT {
        return Err("message exceeds 4096 bytes".into());
    }
    // Recheck at the operation boundary so revocation applies to new messages.
    state.require_trust(&remote)?;
    let reply = Reply {
        from: state.public_key().into(),
        message: request.message,
    };
    encrypt(&mut stream, &mut transport, &serde_json::to_vec(&reply)?)?;
    println!("Authenticated exchange with {remote}");
    Ok(())
}
pub fn serve(listener: TcpListener, state: Arc<State>) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("accept: {error}");
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= 32 {
            active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let state = state.clone();
        let active = active.clone();
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, &state) {
                eprintln!("Peer exchange rejected: {error}");
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary() -> (std::path::PathBuf, Arc<State>) {
        let directory =
            std::env::temp_dir().join(format!("boxen-test-{}", state::random_hex().unwrap()));
        let state = Arc::new(State::open(directory.clone()).unwrap());
        (directory, state)
    }
    #[test]
    fn mutual_trust_identity_pinning_restart_and_revocation() {
        let (a_dir, a) = temporary();
        let (b_dir, b) = temporary();
        let (c_dir, c) = temporary();
        a.trust(b.public_key(), "B").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let worker_b = b.clone();
        let worker = std::thread::spawn(move || {
            for _ in 0..4 {
                let (stream, _) = listener.accept().unwrap();
                let _ = handle(stream, &worker_b);
            }
        });
        assert!(ping(&a, &address, b.public_key(), "unpaired").is_err());
        b.trust(a.public_key(), "A").unwrap();
        let reopened = State::open(a_dir.clone()).unwrap();
        assert_eq!(a.public_key(), reopened.public_key());
        assert_eq!(
            ping(&reopened, &address, b.public_key(), "hello 🐈")
                .unwrap()
                .message,
            "hello 🐈"
        );
        a.trust(c.public_key(), "C").unwrap();
        assert!(ping(&a, &address, c.public_key(), "wrong identity").is_err());
        b.untrust(a.public_key()).unwrap();
        assert!(ping(&a, &address, b.public_key(), "revoked").is_err());
        worker.join().unwrap();
        for dir in [a_dir, b_dir, c_dir] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn malformed_keys_and_oversized_messages_fail_before_connecting() {
        let (dir, state) = temporary();
        assert!(state.trust("../oops", "bad").is_err());
        assert!(state.trust(state.public_key(), "self").is_err());
        let pair = snow::Builder::new(params()).generate_keypair().unwrap();
        let peer = state::hex(&pair.public);
        state.trust(&peer, "other").unwrap();
        assert!(
            ping(&state, "invalid", &peer, &"x".repeat(4097))
                .unwrap_err()
                .to_string()
                .contains("4096")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! Bounded native Gateway transport. The daemon's correspondence cursor owns
//! accepted sequence state; this thread only maintains the live connection.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::{TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, protocol::WebSocketConfig, stream::MaybeTlsStream};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub session_id: Option<String>,
    pub resume_url: Option<String>,
    pub sequence: u64,
    pub gaps: u64,
    pub last_gap: Option<String>,
}
pub struct Gateway {
    pub events: mpsc::Receiver<Value>,
    state: Arc<Mutex<Session>>,
    stop: Arc<AtomicBool>,
    invalid: Arc<AtomicBool>,
    pub connected: Arc<AtomicBool>,
    pub failures: Arc<AtomicU64>,
    pub pressure: Arc<AtomicU64>,
    presence: Arc<Mutex<Value>>,
    pub last_error: Arc<Mutex<Option<String>>>,
}
impl Gateway {
    pub fn start(url: String, key: String, state: Session) -> Result<Self> {
        validate_url(&url)?;
        let (tx, events) = mpsc::sync_channel(64);
        let state = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let failures = Arc::new(AtomicU64::new(0));
        let pressure = Arc::new(AtomicU64::new(0));
        let invalid = Arc::new(AtomicBool::new(false));
        let presence = Arc::new(Mutex::new(
            json!({"since":null,"activities":[],"status":"invisible","afk":false}),
        ));
        let last_error = Arc::new(Mutex::new(None));
        let out = Self {
            events,
            state: state.clone(),
            stop: stop.clone(),
            connected: connected.clone(),
            failures: failures.clone(),
            pressure: pressure.clone(),
            invalid: invalid.clone(),
            presence: presence.clone(),
            last_error: last_error.clone(),
        };
        thread::Builder::new().name("discord-gateway".into()).spawn(move|| {
            let mut delay=5;
            while !stop.load(Ordering::Relaxed) {
                if invalid.load(Ordering::Relaxed) {thread::sleep(Duration::from_millis(200));continue;}
                let saved=state.lock().unwrap().clone();let gateway=saved.resume_url.as_deref().unwrap_or(&url);
                let result=(||->Result<()> {
                    validate_url(gateway)?;
                    let uri:tungstenite::http::Uri=gateway.parse()?;let host=uri.host().ok_or("Gateway host unavailable")?;let port=uri.port_u16().unwrap_or(if uri.scheme_str()==Some("wss"){443}else{80});
                    let addr=(host,port).to_socket_addrs()?.next().ok_or("Gateway address unavailable")?;
                    let stream=TcpStream::connect_timeout(&addr,Duration::from_secs(5))?;
                    stream.set_read_timeout(Some(Duration::from_secs(10)))?;stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                    // Rebuild the request target: Discord's discovery URL omits '/',
                    // whereas the HTTP upgrade requires an absolute path before the query.
                    let path=if uri.path().is_empty(){"/"}else{uri.path()};
                    let query=uri.query().map(|q|format!("{q}&")).unwrap_or_default();
                    let url=format!("{}://{}{path}?{query}v=10&encoding=json",uri.scheme_str().unwrap_or("wss"),uri.authority().ok_or("Gateway authority unavailable")?);
                    let config=WebSocketConfig::default().read_buffer_size(4096).write_buffer_size(0).max_write_buffer_size(65536).max_message_size(Some(2*1024*1024)).max_frame_size(Some(2*1024*1024));
                    let (mut socket,_)=tungstenite::client_tls_with_config(url.as_str(),stream,Some(config),None).map_err(|e| match e {
                        tungstenite::HandshakeError::Failure(e)=>match e {
                        tungstenite::Error::Url(e)=>format!("Gateway URL rejected: {e}"),
                        tungstenite::Error::Http(r)=>format!("Gateway upgrade status {}",r.status()),
                        tungstenite::Error::Io(e)=>format!("Gateway handshake I/O {:?}",e.kind()),
                        tungstenite::Error::Tls(_)=>"Gateway TLS validation failed".into(),
                        _=>"Gateway handshake protocol failed".into(),
                        },
                        _=>"Gateway handshake interrupted".to_owned(),
                    })?;
                    if stop.load(Ordering::Relaxed){return Ok(());}
                    let hello=socket.read()?;let Message::Text(hello)=hello else{return Err("Gateway hello unavailable".into());};let hello:Value=serde_json::from_str(hello.as_str())?;
                    if hello["op"]!=10 {return Err("Gateway hello opcode unavailable".into());}
                    let interval=hello["d"]["heartbeat_interval"].as_u64().filter(|n|*n>=100 && *n<=120000).ok_or("Gateway heartbeat bounds invalid")?;
                    match socket.get_mut() {
                        MaybeTlsStream::Plain(s)=>s.set_read_timeout(Some(Duration::from_millis(200)))?,
                        MaybeTlsStream::Rustls(s)=>s.sock.set_read_timeout(Some(Duration::from_millis(200)))?,
                        _=>return Err("unsupported Gateway TLS transport".into()),
                    }
                    let identify=if let Some(session)=&saved.session_id {json!({"op":6,"d":{"token":key,"session_id":session,"seq":saved.sequence}})}else{json!({"op":2,"d":{"token":key,"intents":1|512|1024|4096|8192|32768,"properties":{"os":crate::platform::host_os(),"browser":"Boxen","device":"Boxen"},"presence":presence.lock().unwrap().clone()}})};
                    socket.send(Message::Text(identify.to_string().into()))?;
                    let mut last=saved.sequence;let mut session=saved.session_id.clone();let mut due=Instant::now()+Duration::from_millis(interval/2);let mut awaiting=false;let mut last_presence=Value::Null;let mut presence_at=Instant::now();
                    loop {
                        if stop.load(Ordering::Relaxed){return Ok(());}
                        if Instant::now()>=due {
                            if awaiting {return Err("Gateway heartbeat acknowledgment unavailable".into());}
                            socket.send(Message::Text(json!({"op":1,"d":if last==0 {Value::Null}else{json!(last)}}).to_string().into()))?;awaiting=true;due=Instant::now()+Duration::from_millis(interval);
                        }
                        if connected.load(Ordering::Relaxed) && presence_at.elapsed()>=Duration::from_secs(5) {
                            let next=presence.lock().unwrap().clone();
                            if next!=last_presence {socket.send(Message::Text(json!({"op":3,"d":next}).to_string().into()))?;last_presence=next;presence_at=Instant::now();}
                        }
                        let frame=match socket.read() {
                            Ok(Message::Text(text))=>serde_json::from_str::<Value>(text.as_str())?,
                            Ok(Message::Ping(_)|Message::Pong(_))=>{socket.flush()?;continue;},
                            Ok(Message::Close(frame))=>{
                                if frame.as_ref().is_some_and(|f| matches!(u16::from(f.code),4007|4009)) {
                                    invalid.store(true,Ordering::Relaxed);
                                    tx.send(json!({"type":"INVALID_SESSION"})).map_err(|_|"Gateway consumer unavailable")?;
                                }
                                return Err(format!("Gateway closed with code {}",frame.map(|f|u16::from(f.code)).unwrap_or(0)).into());
                            },
                            Ok(_)=>continue,
                            Err(tungstenite::Error::Io(e)) if matches!(e.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut)=>continue,
                            Err(_)=>return Err("Gateway receive unavailable".into()),
                        };
                        match frame["op"].as_u64() {
                            Some(11)=>awaiting=false,
                            Some(1)=>{socket.send(Message::Text(json!({"op":1,"d":if last==0 {Value::Null}else{json!(last)}}).to_string().into()))?;},
                            Some(7)=>return Err("Gateway reconnect requested".into()),
                            Some(9)=>{
                                if frame["d"]!=true {invalid.store(true,Ordering::Relaxed);tx.send(json!({"type":"INVALID_SESSION"})).map_err(|_|"Gateway consumer unavailable")?;}
                                return Err("Gateway session rejected".into());
                            },
                            Some(0)=>{
                                last=frame["s"].as_u64().ok_or("Gateway sequence unavailable")?;
                                let kind=frame["t"].as_str().unwrap_or("");
                                let mut data=frame["d"].clone();
                                if kind=="READY" {session=data["session_id"].as_str().map(str::to_owned);data=json!({"session_id":session,"resume_gateway_url":data["resume_gateway_url"],"user":data["user"]});connected.store(true,Ordering::Relaxed);delay=5;}
                                else if kind=="RESUMED" {connected.store(true,Ordering::Relaxed);delay=5;}
                                else if kind=="GUILD_CREATE" {
                                    let channels:Vec<_>=data["channels"].as_array().into_iter().flatten().chain(data["threads"].as_array().into_iter().flatten()).take(512).map(|c|json!({"id":c["id"],"guild_id":data["id"],"parent_id":c["parent_id"],"type":c["type"]})).collect();data=json!({"id":data["id"],"channels":channels});
                                } else if kind=="MESSAGE_CREATE" {
                                    data=crate::messaging::bounded_native(&json!({"id":data["id"],"channel_id":data["channel_id"],"guild_id":data["guild_id"],"author":data["author"],"body":data["content"].as_str().unwrap_or(""),"attachments":data["attachments"],"native":data,"timestamp":data["timestamp"]}));
                                } else if !matches!(kind,"CHANNEL_CREATE"|"CHANNEL_UPDATE"|"CHANNEL_DELETE"|"THREAD_CREATE"|"THREAD_UPDATE"|"THREAD_DELETE"|"MESSAGE_REACTION_ADD") {data=Value::Null;}
                                let event=json!({"type":kind,"sequence":last,"session_id":session,"data":data});
                                if tx.try_send(event).is_err(){pressure.fetch_add(1,Ordering::Relaxed);return Err("Gateway buffer full; resume remains at accepted sequence".into());}
                            },
                            _=>{},
                        }
                    }
                })();
                connected.store(false,Ordering::Relaxed);
                if let Err(error)=result {*last_error.lock().unwrap()=Some(error.to_string());failures.fetch_add(1,Ordering::Relaxed);}
                let until=Instant::now()+Duration::from_secs(delay);while !stop.load(Ordering::Relaxed)&&Instant::now()<until{thread::sleep(Duration::from_millis(200));}
                delay=(delay*2).min(60);
            }
        })?;
        Ok(out)
    }
    pub fn acknowledge(&self, value: &Value) -> Result<()> {
        let next: Session = serde_json::from_value(value.clone())?;
        *self.state.lock().unwrap() = next;
        self.invalid.store(false, Ordering::Relaxed);
        Ok(())
    }
    pub fn presence(&self, value: Value) {
        *self.presence.lock().unwrap() = value;
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
pub fn validate_url(url: &str) -> Result<()> {
    let uri: tungstenite::http::Uri = url.parse()?;
    let host = uri.host().ok_or("Gateway host unavailable")?;
    let valid = uri.scheme_str() == Some("wss")
        && (host == "gateway.discord.gg" || host.ends_with(".discord.gg"))
        || uri.scheme_str() == Some("ws") && matches!(host, "127.0.0.1" | "localhost" | "[::1]");
    if !valid || uri.authority().is_some_and(|a| a.as_str().contains('@')) {
        return Err("Gateway transport is Discord WSS or an explicit loopback WS fixture".into());
    }
    Ok(())
}

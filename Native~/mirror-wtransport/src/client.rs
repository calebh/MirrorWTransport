//! Native WebTransport client.
//!
//! The browser is the primary client for this transport, but Mirror also has to
//! run in the editor and in standalone players: without this, the only way to
//! test a build would be to deploy it to WebGL first. It speaks exactly the same
//! wire protocol as `MirrorWTransport.jslib`.

use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::sync::watch;
use wtransport::ClientConfig;
use wtransport::Connection;
use wtransport::Endpoint;
use wtransport::VarInt;

use crate::common;
use crate::common::error_code;
use crate::common::BufPool;
use crate::common::Event;
use crate::common::EventReceiver;
use crate::common::EventSender;
use crate::common::CLOSE_CODE_NORMAL;
use crate::common::PROTOCOL_MAGIC;
use crate::common::PROTOCOL_MAGIC_LEN;
use crate::session;
use crate::session::OutMsg;
use crate::session::OutSender;

/// Connection states as seen by C#. Keep in sync with `WTClientState`.
pub mod state {
    pub const DISCONNECTED: i32 = 0;
    pub const CONNECTING: i32 = 1;
    pub const CONNECTED: i32 = 2;
}

/// Mirror does not identify the server connection by id, so the client always
/// reports 0.
const CLIENT_CONNECTION_ID: u32 = 0;

const CLOSE_NORMAL: VarInt = VarInt::from_u32(CLOSE_CODE_NORMAL);

pub struct ClientSettings {
    pub url: String,
    /// SHA-256 of the server certificate, either `aa:bb:...` or `[170, 187, ...]`.
    /// Mirrors what a browser accepts in `serverCertificateHashes`.
    pub certificate_hash: Option<String>,
    /// Accept any certificate. Development only.
    pub allow_invalid_certificates: bool,
    pub connect_timeout_ms: u32,
    pub keep_alive_ms: u32,
    pub idle_timeout_ms: u32,
    pub max_reliable_payload: usize,
    pub max_unreliable_payload: usize,
}

struct ClientCtx {
    events: EventSender,
    state: AtomicI32,
    connection: Mutex<Option<Connection>>,
    reliable: Mutex<Option<OutSender>>,
    pool: BufPool,
    settings: ClientSettings,
}

impl ClientCtx {
    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

pub struct Client {
    runtime: tokio::runtime::Runtime,
    ctx: Arc<ClientCtx>,
    events: EventReceiver,
    shutdown: watch::Sender<bool>,
}

impl Client {
    pub fn connect(settings: ClientSettings) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("mwt-client")
            .build()
            .map_err(|e| format!("failed to create the tokio runtime: {e}"))?;

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let ctx = Arc::new(ClientCtx {
            events: event_tx,
            state: AtomicI32::new(state::CONNECTING),
            connection: Mutex::new(None),
            reliable: Mutex::new(None),
            pool: BufPool::new(),
            settings,
        });

        runtime.spawn(client_main(ctx.clone(), shutdown_rx));

        Ok(Self {
            runtime,
            ctx,
            events: event_rx,
            shutdown: shutdown_tx,
        })
    }

    pub fn state(&self) -> i32 {
        self.ctx.state.load(Ordering::Relaxed)
    }

    pub fn poll(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Hands a payload buffer back to the pool once C# has copied it out.
    pub fn recycle(&self, buffer: Vec<u8>) {
        self.ctx.pool.give(buffer);
    }

    pub fn send(&self, channel: u8, reliable: bool, payload: &[u8]) -> Result<(), String> {
        if self.state() != state::CONNECTED {
            return Err("not connected".to_string());
        }

        if reliable {
            if payload.len() > self.ctx.settings.max_reliable_payload {
                return Err(format!(
                    "reliable message of {} bytes exceeds the configured maximum of {}",
                    payload.len(),
                    self.ctx.settings.max_reliable_payload
                ));
            }

            let queue = self.ctx.reliable.lock().unwrap_or_else(|e| e.into_inner());
            let Some(queue) = queue.as_ref() else {
                return Err("the reliable stream is not open".to_string());
            };

            let mut frame = self.ctx.pool.take(common::FRAME_HEADER_LEN + payload.len());
            common::write_frame(&mut frame, channel, payload);
            queue
                .send(OutMsg::Frame(frame))
                .map_err(|_| "the reliable stream is closed".to_string())
        } else {
            if payload.len() > self.ctx.settings.max_unreliable_payload {
                return Err(format!(
                    "unreliable message of {} bytes exceeds the configured maximum of {}",
                    payload.len(),
                    self.ctx.settings.max_unreliable_payload
                ));
            }

            let mut datagram = self
                .ctx
                .pool
                .take(common::DATAGRAM_HEADER_LEN + payload.len());
            common::write_datagram(&mut datagram, channel, payload);

            let result = {
                let guard = self.ctx.connection.lock().unwrap_or_else(|e| e.into_inner());
                match guard.as_ref() {
                    Some(connection) => connection
                        .send_datagram(&datagram)
                        .map_err(|e| format!("failed to send a datagram: {e}")),
                    None => Err("not connected".to_string()),
                }
            };

            self.ctx.pool.give(datagram);
            result
        }
    }

    /// Asks the session to shut down. The `Disconnected` event still arrives
    /// through `poll`, which is what Mirror needs to hear.
    pub fn disconnect(&self) {
        let queued = {
            let queue = self.ctx.reliable.lock().unwrap_or_else(|e| e.into_inner());
            match queue.as_ref() {
                Some(queue) => queue
                    .send(OutMsg::Close {
                        code: CLOSE_CODE_NORMAL,
                    })
                    .is_ok(),
                None => false,
            }
        };

        if !queued {
            // Either the reliable stream never opened (still connecting) or it
            // is already gone; cancel the session task instead.
            let _ = self.shutdown.send(true);
        }
    }

    pub fn stop(self) {
        let _ = self.shutdown.send(true);

        if let Some(connection) = self
            .ctx
            .connection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            connection.close(CLOSE_NORMAL, b"client shutting down");
        }

        self.runtime.shutdown_timeout(Duration::from_millis(200));
    }
}

async fn client_main(ctx: Arc<ClientCtx>, mut shutdown: watch::Receiver<bool>) {
    let outcome = tokio::select! {
        result = client_run(ctx.clone()) => result,
        _ = shutdown.changed() => Ok((
            error_code::CONNECTION_CLOSED,
            "disconnected locally".to_string(),
        )),
    };

    ctx.state.store(state::DISCONNECTED, Ordering::Relaxed);
    *ctx.reliable.lock().unwrap_or_else(|e| e.into_inner()) = None;

    if let Some(connection) = ctx
        .connection
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        connection.close(CLOSE_NORMAL, b"client disconnected");
    }

    let (code, reason) = match outcome {
        Ok(closed) => closed,
        Err((code, message)) => {
            // Mirror expects the error first and the disconnect right after.
            ctx.emit(Event::Error {
                id: CLIENT_CONNECTION_ID,
                code,
                message: message.clone(),
            });
            (code, message)
        }
    };

    ctx.emit(Event::Disconnected {
        id: CLIENT_CONNECTION_ID,
        code,
        reason,
    });
}

/// `Ok` means the session was established and then ended; `Err` means it never
/// got that far.
async fn client_run(ctx: Arc<ClientCtx>) -> Result<(i32, String), (i32, String)> {
    let config = build_client_config(&ctx.settings)
        .map_err(|message| (error_code::UNEXPECTED, message))?;

    let endpoint = Endpoint::client(config)
        .map_err(|e| (error_code::UNEXPECTED, format!("failed to bind a local UDP socket: {e}")))?;

    let connect_timeout = Duration::from_millis(ctx.settings.connect_timeout_ms as u64);

    let connection = match tokio::time::timeout(
        connect_timeout,
        endpoint.connect(ctx.settings.url.as_str()),
    )
    .await
    {
        Ok(Ok(connection)) => connection,
        Ok(Err(e)) => {
            return Err((
                connecting_error_code(&e),
                format!("could not connect to {}: {e}", ctx.settings.url),
            ))
        }
        Err(_) => {
            return Err((
                error_code::TIMEOUT,
                format!("connecting to {} timed out", ctx.settings.url),
            ))
        }
    };

    // Open the reliable channel and complete the prologue exchange before
    // telling Mirror it is connected, so both delivery modes are usable by the
    // time OnClientConnected fires.
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| (error_code::CONNECTION_CLOSED, format!("could not open the reliable stream: {e}")))?
        .await
        .map_err(|e| (error_code::CONNECTION_CLOSED, format!("could not open the reliable stream: {e}")))?;

    send.write_all(&PROTOCOL_MAGIC)
        .await
        .map_err(|e| (error_code::CONNECTION_CLOSED, format!("handshake failed: {e}")))?;

    let mut prologue = [0u8; PROTOCOL_MAGIC_LEN];
    match tokio::time::timeout(connect_timeout, recv.read_exact(&mut prologue)).await {
        Ok(Ok(())) if prologue == PROTOCOL_MAGIC => {}
        Ok(Ok(())) => {
            return Err((
                error_code::INVALID_RECEIVE,
                "the server is not speaking the MirrorWTransport protocol".to_string(),
            ))
        }
        Ok(Err(e)) => {
            return Err((
                error_code::CONNECTION_CLOSED,
                format!("handshake failed: {e}"),
            ))
        }
        Err(_) => return Err((error_code::TIMEOUT, "the handshake timed out".to_string())),
    }

    let (out_tx, out_rx) = mpsc::unbounded_channel();

    *ctx.connection.lock().unwrap_or_else(|e| e.into_inner()) = Some(connection.clone());
    *ctx.reliable.lock().unwrap_or_else(|e| e.into_inner()) = Some(out_tx);
    ctx.state.store(state::CONNECTED, Ordering::Relaxed);

    ctx.emit(Event::Connected {
        id: CLIENT_CONNECTION_ID,
        address: ctx.settings.url.clone(),
    });

    tokio::select! {
        _ = session::reliable_writer(send, out_rx, connection.clone(), ctx.pool.clone()) => {}
        _ = session::reliable_reader(
                recv,
                CLIENT_CONNECTION_ID,
                connection.clone(),
                ctx.events.clone(),
                ctx.pool.clone(),
                ctx.settings.max_reliable_payload) => {}
        _ = session::datagram_reader(
                connection.clone(),
                CLIENT_CONNECTION_ID,
                ctx.events.clone(),
                ctx.pool.clone(),
                ctx.settings.max_unreliable_payload) => {}
        _ = connection.closed() => {}
    }

    connection.close(CLOSE_NORMAL, b"session ended");
    Ok(session::describe(&connection.closed().await))
}

fn build_client_config(settings: &ClientSettings) -> Result<ClientConfig, String> {
    use wtransport::tls::Sha256Digest;
    use wtransport::tls::Sha256DigestFmt;

    let builder = ClientConfig::builder().with_bind_default();

    let builder = match settings.certificate_hash.as_deref() {
        Some(hash) if !hash.trim().is_empty() => {
            let hash = hash.trim();
            // Accept both formats wtransport can print, so whichever one the
            // user copied out of the server log just works.
            let digest = Sha256Digest::from_str_fmt(hash, Sha256DigestFmt::DottedHex)
                .or_else(|_| Sha256Digest::from_str_fmt(hash, Sha256DigestFmt::BytesArray))
                .map_err(|_| format!("{hash} is not a valid SHA-256 certificate hash"))?;
            builder.with_server_certificate_hashes([digest])
        }
        _ if settings.allow_invalid_certificates => builder.with_no_cert_validation(),
        _ => builder.with_native_certs(),
    };

    let keep_alive = match settings.keep_alive_ms {
        0 => None,
        ms => Some(Duration::from_millis(ms as u64)),
    };

    let idle_timeout = match settings.idle_timeout_ms {
        0 => None,
        ms => Some(Duration::from_millis(ms as u64)),
    };

    let builder = builder
        .keep_alive_interval(keep_alive)
        .max_idle_timeout(idle_timeout)
        .map_err(|_| {
            format!(
                "an idle timeout of {} ms is outside the range QUIC can encode",
                settings.idle_timeout_ms
            )
        })?;

    Ok(builder.build())
}

fn connecting_error_code(error: &wtransport::error::ConnectingError) -> i32 {
    use wtransport::error::ConnectingError;

    match error {
        ConnectingError::InvalidUrl(_) => error_code::DNS_RESOLVE,
        ConnectingError::DnsLookup(_) | ConnectingError::DnsNotFound => error_code::DNS_RESOLVE,
        ConnectingError::SessionRejected => error_code::REFUSED,
        ConnectingError::ConnectionError(e) => session::describe(e).0,
        _ => error_code::UNEXPECTED,
    }
}

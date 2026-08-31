//! WebTransport server: one tokio runtime, one task per session, and a queue
//! of events that Unity drains from the main thread.

use std::collections::HashMap;
use std::future::IntoFuture;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::sync::watch;
use wtransport::endpoint::endpoint_side;
use wtransport::endpoint::IncomingSession;
use wtransport::Connection;
use wtransport::Endpoint;
use wtransport::Identity;
use wtransport::ServerConfig;
use wtransport::VarInt;

use crate::common;
use crate::common::BufPool;
use crate::common::Event;
use crate::common::EventReceiver;
use crate::common::EventSender;
use crate::common::CLOSE_CODE_NORMAL;
use crate::common::CLOSE_CODE_PROTOCOL;
use crate::common::PROTOCOL_MAGIC;
use crate::common::PROTOCOL_MAGIC_LEN;
use crate::session;
use crate::session::OutMsg;
use crate::session::OutSender;

/// Where the TLS identity comes from.
pub enum IdentitySource {
    /// Generate an ECDSA P-256 certificate valid for 14 days. Its SHA-256 hash
    /// can be handed to a browser through `serverCertificateHashes`, which is
    /// the only way to develop against WebTransport without a real certificate.
    SelfSigned { subject_alt_names: Vec<String> },
    /// Load `fullchain.pem` / `privkey.pem` issued by a real CA.
    PemFiles { certificate: String, key: String },
}

pub struct ServerSettings {
    pub port: u16,
    pub bind_mode: i32,
    pub identity: IdentitySource,
    pub keep_alive_ms: u32,
    pub idle_timeout_ms: u32,
    pub handshake_timeout_ms: u32,
    pub max_reliable_payload: usize,
    pub max_unreliable_payload: usize,
    /// 0 means unlimited.
    pub max_connections: u32,
}

struct ConnHandle {
    connection: Connection,
    reliable: OutSender,
    address: String,
}

struct ServerCtx {
    events: EventSender,
    connections: Mutex<HashMap<u32, ConnHandle>>,
    next_id: AtomicU32,
    connection_count: AtomicU32,
    pool: BufPool,
    settings: ServerSettings,
}

impl ServerCtx {
    /// Mirror reserves connection id 0 for the host client, so ids start at 1.
    /// The counter wraps inside the positive `int` range because Mirror stores
    /// connection ids as a signed 32 bit integer.
    fn allocate_id(&self) -> u32 {
        loop {
            let id = self
                .next_id
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(if current >= i32::MAX as u32 { 1 } else { current + 1 })
                })
                .expect("the fetch_update closure never returns None");

            if !self.lock_connections().contains_key(&id) {
                return id;
            }
        }
    }

    fn lock_connections(&self) -> std::sync::MutexGuard<'_, HashMap<u32, ConnHandle>> {
        self.connections.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, event: Event) {
        // Failure only means Unity dropped the receiver, i.e. the server is gone.
        let _ = self.events.send(event);
    }
}

pub struct Server {
    runtime: tokio::runtime::Runtime,
    ctx: Arc<ServerCtx>,
    events: EventReceiver,
    shutdown: watch::Sender<bool>,
    local_port: u16,
    certificate_hash: Option<String>,
}

impl Server {
    pub fn start(settings: ServerSettings) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("mwt-server")
            .build()
            .map_err(|e| format!("failed to create the tokio runtime: {e}"))?;

        let (identity, certificate_hash) = runtime.block_on(build_identity(&settings.identity))?;
        let config = build_server_config(&settings, identity)?;

        // quinn binds its UDP socket through the ambient tokio runtime, so the
        // endpoint has to be created from inside the runtime context.
        let endpoint = {
            let _guard = runtime.enter();
            Endpoint::server(config).map_err(|e| {
                format!(
                    "failed to bind the WebTransport server on port {}: {e}",
                    settings.port
                )
            })?
        };

        let local_port = endpoint
            .local_addr()
            .map(|address| address.port())
            .unwrap_or(settings.port);

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let ctx = Arc::new(ServerCtx {
            events: event_tx,
            connections: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            connection_count: AtomicU32::new(0),
            pool: BufPool::new(),
            settings,
        });

        runtime.spawn(accept_loop(endpoint, ctx.clone(), shutdown_rx));

        Ok(Self {
            runtime,
            ctx,
            events: event_rx,
            shutdown: shutdown_tx,
            local_port,
            certificate_hash,
        })
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn certificate_hash(&self) -> Option<&str> {
        self.certificate_hash.as_deref()
    }

    pub fn poll(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Hands a payload buffer back to the pool once C# has copied it out.
    pub fn recycle(&self, buffer: Vec<u8>) {
        self.ctx.pool.give(buffer);
    }

    pub fn connection_count(&self) -> u32 {
        self.ctx.connection_count.load(Ordering::Relaxed)
    }

    pub fn address_of(&self, id: u32) -> Option<String> {
        self.ctx
            .lock_connections()
            .get(&id)
            .map(|handle| handle.address.clone())
    }

    pub fn send(&self, id: u32, channel: u8, reliable: bool, payload: &[u8]) -> Result<(), String> {
        let connections = self.ctx.lock_connections();

        let Some(handle) = connections.get(&id) else {
            return Err(format!("connection {id} is not connected"));
        };

        if reliable {
            if payload.len() > self.ctx.settings.max_reliable_payload {
                return Err(format!(
                    "reliable message of {} bytes exceeds the configured maximum of {}",
                    payload.len(),
                    self.ctx.settings.max_reliable_payload
                ));
            }

            let mut frame = self.ctx.pool.take(common::FRAME_HEADER_LEN + payload.len());
            common::write_frame(&mut frame, channel, payload);
            handle
                .reliable
                .send(OutMsg::Frame(frame))
                .map_err(|_| format!("the reliable stream of connection {id} is closed"))
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

            // send_datagram copies into its own buffer, so the scratch buffer
            // can go straight back into the pool.
            let result = handle
                .connection
                .send_datagram(&datagram)
                .map_err(|e| format!("failed to send a datagram to connection {id}: {e}"));

            drop(connections);
            self.ctx.pool.give(datagram);
            result
        }
    }

    pub fn disconnect(&self, id: u32) {
        let connections = self.ctx.lock_connections();

        if let Some(handle) = connections.get(&id) {
            // Route the close through the writer queue so anything Mirror sent
            // in the same frame still reaches the wire first.
            let queued = handle
                .reliable
                .send(OutMsg::Close {
                    code: CLOSE_CODE_NORMAL,
                })
                .is_ok();

            if !queued {
                handle
                    .connection
                    .close(VarInt::from_u32(CLOSE_CODE_NORMAL), b"disconnected by server");
            }
        }
    }

    pub fn stop(self) {
        let _ = self.shutdown.send(true);

        for handle in self.ctx.lock_connections().values() {
            handle
                .connection
                .close(VarInt::from_u32(CLOSE_CODE_NORMAL), b"server shutting down");
        }

        // Give the tasks a moment to flush their CONNECTION_CLOSE frames, then
        // tear the runtime down whether or not they finished.
        self.runtime.shutdown_timeout(Duration::from_millis(200));
    }
}

async fn build_identity(source: &IdentitySource) -> Result<(Identity, Option<String>), String> {
    match source {
        IdentitySource::SelfSigned { subject_alt_names } => {
            let identity = Identity::self_signed(subject_alt_names)
                .map_err(|e| format!("failed to generate a self signed certificate: {e}"))?;

            let hash = identity
                .certificate_chain()
                .as_slice()
                .first()
                .map(|certificate| {
                    certificate
                        .hash()
                        .fmt(wtransport::tls::Sha256DigestFmt::DottedHex)
                });

            Ok((identity, hash))
        }
        IdentitySource::PemFiles { certificate, key } => {
            let identity = Identity::load_pemfiles(certificate, key)
                .await
                .map_err(|e| format!("failed to load {certificate} / {key}: {e}"))?;

            Ok((identity, None))
        }
    }
}

fn build_server_config(
    settings: &ServerSettings,
    identity: Identity,
) -> Result<ServerConfig, String> {
    use wtransport::config::IpBindConfig;

    let bind_config = match settings.bind_mode {
        0 => IpBindConfig::InAddrAnyDual,
        1 => IpBindConfig::InAddrAnyV4,
        2 => IpBindConfig::InAddrAnyV6,
        3 => IpBindConfig::LocalDual,
        4 => IpBindConfig::LocalV4,
        5 => IpBindConfig::LocalV6,
        other => return Err(format!("unknown bind mode {other}")),
    };

    let keep_alive = match settings.keep_alive_ms {
        0 => None,
        ms => Some(Duration::from_millis(ms as u64)),
    };

    let idle_timeout = match settings.idle_timeout_ms {
        0 => None,
        ms => Some(Duration::from_millis(ms as u64)),
    };

    let builder = ServerConfig::builder()
        .with_bind_config(bind_config, settings.port)
        .with_identity(identity)
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

async fn accept_loop(
    endpoint: Endpoint<endpoint_side::Server>,
    ctx: Arc<ServerCtx>,
    mut shutdown: watch::Receiver<bool>,
) {
    common::log_info(format!(
        "WebTransport server listening on UDP port {}",
        endpoint
            .local_addr()
            .map(|address| address.port())
            .unwrap_or(ctx.settings.port)
    ));

    loop {
        // Note: the shutdown receiver is mutably borrowed by changed() for as
        // long as this select runs, so the accept arm cannot read it. It does
        // not need to: at worst one more session starts and is then closed by
        // the shutdown below.
        tokio::select! {
            _ = shutdown.changed() => break,
            incoming = endpoint.accept() => {
                tokio::spawn(handle_session(incoming, ctx.clone()));
            }
        }
    }

    endpoint.close(VarInt::from_u32(CLOSE_CODE_NORMAL), b"server shutting down");
}

async fn handle_session(incoming: IncomingSession, ctx: Arc<ServerCtx>) {
    let remote_address = incoming.remote_address().to_string();
    let handshake_timeout = Duration::from_millis(ctx.settings.handshake_timeout_ms as u64);

    if ctx.settings.max_connections > 0
        && ctx.connection_count.load(Ordering::Relaxed) >= ctx.settings.max_connections
    {
        common::log_warn(format!(
            "refused a session from {remote_address}: the server is full"
        ));
        incoming.refuse();
        return;
    }

    let session_request =
        match tokio::time::timeout(handshake_timeout, incoming.into_future()).await {
            Ok(Ok(request)) => request,
            Ok(Err(e)) => {
                common::log_warn(format!("session from {remote_address} failed: {e}"));
                return;
            }
            Err(_) => {
                common::log_warn(format!("session from {remote_address} timed out"));
                return;
            }
        };

    let path = session_request.path().to_string();

    let connection = match tokio::time::timeout(handshake_timeout, session_request.accept()).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(e)) => {
            common::log_warn(format!(
                "could not accept the session from {remote_address}: {e}"
            ));
            return;
        }
        Err(_) => {
            common::log_warn(format!(
                "accepting the session from {remote_address} timed out"
            ));
            return;
        }
    };

    // The reliable channel is a single bidirectional stream opened by the
    // client. Waiting for it here means the connection is only reported to
    // Mirror once both delivery modes are usable.
    let (mut send, mut recv) =
        match tokio::time::timeout(handshake_timeout, connection.accept_bi()).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                common::log_warn(format!(
                    "{remote_address} never opened its reliable stream: {e}"
                ));
                return;
            }
            Err(_) => {
                common::log_warn(format!(
                    "{remote_address} did not open its reliable stream in time"
                ));
                connection.close(VarInt::from_u32(CLOSE_CODE_PROTOCOL), b"handshake timeout");
                return;
            }
        };

    let mut prologue = [0u8; PROTOCOL_MAGIC_LEN];
    match tokio::time::timeout(handshake_timeout, recv.read_exact(&mut prologue)).await {
        Ok(Ok(())) if prologue == PROTOCOL_MAGIC => {}
        Ok(Ok(())) => {
            common::log_warn(format!(
                "{remote_address} sent an unknown protocol prologue"
            ));
            connection.close(VarInt::from_u32(CLOSE_CODE_PROTOCOL), b"bad prologue");
            return;
        }
        Ok(Err(e)) => {
            common::log_warn(format!("{remote_address} failed the handshake: {e}"));
            return;
        }
        Err(_) => {
            common::log_warn(format!("{remote_address} timed out during the handshake"));
            connection.close(VarInt::from_u32(CLOSE_CODE_PROTOCOL), b"handshake timeout");
            return;
        }
    }

    if send.write_all(&PROTOCOL_MAGIC).await.is_err() {
        common::log_warn(format!(
            "{remote_address} disconnected before the handshake completed"
        ));
        return;
    }

    let id = ctx.allocate_id();
    ctx.connection_count.fetch_add(1, Ordering::Relaxed);

    let (out_tx, out_rx) = mpsc::unbounded_channel();

    ctx.lock_connections().insert(
        id,
        ConnHandle {
            connection: connection.clone(),
            reliable: out_tx,
            address: remote_address.clone(),
        },
    );

    common::log_info(format!(
        "connection {id} established from {remote_address} on path {path}"
    ));

    // Emitted before the readers start, so Mirror can never see data for a
    // connection it has not been told about yet.
    ctx.emit(Event::Connected {
        id,
        address: remote_address,
    });

    // All the loops live in this task, so when one of them ends the others are
    // dropped with it and nothing outlives the session.
    tokio::select! {
        _ = session::reliable_writer(send, out_rx, connection.clone(), ctx.pool.clone()) => {}
        _ = session::reliable_reader(
                recv,
                id,
                connection.clone(),
                ctx.events.clone(),
                ctx.pool.clone(),
                ctx.settings.max_reliable_payload) => {}
        _ = session::datagram_reader(
                connection.clone(),
                id,
                ctx.events.clone(),
                ctx.pool.clone(),
                ctx.settings.max_unreliable_payload) => {}
        _ = connection.closed() => {}
    }

    // The select also ends when a loop stops while the session itself is still
    // open (the peer finished its reliable stream, a protocol violation, ...).
    // Close explicitly so the closed() below cannot block forever.
    connection.close(VarInt::from_u32(CLOSE_CODE_NORMAL), b"session ended");
    let close_reason = connection.closed().await;

    ctx.lock_connections().remove(&id);
    ctx.connection_count.fetch_sub(1, Ordering::Relaxed);

    let (code, reason) = session::describe(&close_reason);
    ctx.emit(Event::Disconnected { id, code, reason });
}

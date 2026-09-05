//! The C ABI Unity talks to.
//!
//! Everything here is polling based and copy-on-read: the transport hands a
//! scratch buffer in, the native side copies one event into it, and the payload
//! buffer goes straight back into the pool. No pointer ever outlives the call
//! that produced it, so there is nothing for C# to free.
//!
//! Every exported function catches panics. A panic that unwound into the CLR
//! would take the editor down with it.

use std::ffi::c_char;
use std::ffi::CStr;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;

use crate::client::Client;
use crate::client::ClientSettings;
use crate::common;
use crate::common::event_kind;
use crate::common::Event;
use crate::server::IdentitySource;
use crate::server::Server;
use crate::server::ServerSettings;

/// Bumped whenever the ABI below changes. C# refuses to run against a native
/// library that does not report the version it was built for.
pub const ABI_VERSION: u32 = 2;

static SERVER: Mutex<Option<Server>> = Mutex::new(None);
static CLIENT: Mutex<Option<Client>> = Mutex::new(None);
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

// ---------------------------------------------------------------------------
// Interop types
// ---------------------------------------------------------------------------

/// One polled event. Keep the layout in sync with `WTEvent` on the C# side.
#[repr(C)]
pub struct MwtEvent {
    pub kind: i32,
    pub connection_id: u32,
    pub channel: i32,
    pub data_length: i32,
    pub code: i32,
}

/// Keep the layout in sync with `WTServerConfig`. Pointers come first so the
/// struct has the same shape on 32 and 64 bit targets without any padding.
#[repr(C)]
pub struct MwtServerConfig {
    /// `null` or empty selects a freshly generated self signed certificate.
    pub certificate_path: *const c_char,
    pub key_path: *const c_char,
    /// Comma separated subject alternative names for the self signed certificate.
    pub subject_alt_names: *const c_char,
    pub port: u32,
    pub bind_mode: i32,
    pub keep_alive_ms: u32,
    pub idle_timeout_ms: u32,
    pub handshake_timeout_ms: u32,
    pub max_reliable_payload: u32,
    pub max_unreliable_payload: u32,
    /// 0 means unlimited.
    pub max_connections: u32,
    /// Total validity of a generated self signed certificate, in seconds.
    pub certificate_validity_secs: u32,
    /// How often to replace the certificate on a running server, in seconds.
    /// 0 disables rotation.
    pub certificate_rotation_secs: u32,
}

/// Keep the layout in sync with `WTClientConfig`.
#[repr(C)]
pub struct MwtClientConfig {
    pub url: *const c_char,
    /// `null` or empty means the certificate is validated normally.
    pub certificate_hash: *const c_char,
    pub allow_invalid_certificates: i32,
    pub connect_timeout_ms: u32,
    pub keep_alive_ms: u32,
    pub idle_timeout_ms: u32,
    pub max_reliable_payload: u32,
    pub max_unreliable_payload: u32,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn guard<T>(default: T, body: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_) => {
            set_last_error("the native WebTransport library panicked, see the Unity log");
            common::log_error("the native WebTransport library panicked");
            default
        }
    }
}

fn set_last_error(message: impl Into<String>) {
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = message.into();
}

/// Reads a C string. `null`, an invalid pointer target or an empty string all
/// map to `None`, because C# marshals "not set" as either of the first two.
///
/// # Safety
/// `pointer` must be null or point at a NUL terminated string.
unsafe fn read_string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Copies `text` into `buffer`, truncating on a character boundary so the C#
/// side never has to decode a split UTF-8 sequence. Returns the byte count.
///
/// # Safety
/// `buffer` must be writable for `capacity` bytes, or null when `capacity` is 0.
unsafe fn write_string(text: &str, buffer: *mut u8, capacity: i32) -> i32 {
    if buffer.is_null() || capacity <= 0 {
        return 0;
    }

    let capacity = capacity as usize;
    let mut length = text.len().min(capacity);
    while length > 0 && !text.is_char_boundary(length) {
        length -= 1;
    }

    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), buffer, length) };
    length as i32
}

/// # Safety
/// `buffer` must be writable for `capacity` bytes.
unsafe fn write_bytes(bytes: &[u8], buffer: *mut u8, capacity: i32) -> bool {
    if buffer.is_null() || capacity < 0 || bytes.len() > capacity as usize {
        return false;
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
    true
}

/// Renders one queued event into the caller supplied storage.
///
/// # Safety
/// `out` must point at a writable `MwtEvent`, and `buffer` must be writable for
/// `capacity` bytes.
unsafe fn write_event(
    event: Event,
    out: *mut MwtEvent,
    buffer: *mut u8,
    capacity: i32,
    recycle: impl FnOnce(Vec<u8>),
) -> i32 {
    let mut result = MwtEvent {
        kind: event_kind::NONE,
        connection_id: 0,
        channel: 0,
        data_length: 0,
        code: 0,
    };

    match event {
        Event::Connected { id, address } => {
            result.kind = event_kind::CONNECTED;
            result.connection_id = id;
            result.data_length = unsafe { write_string(&address, buffer, capacity) };
        }
        Event::Data {
            id,
            channel,
            payload,
        } => {
            if unsafe { write_bytes(&payload, buffer, capacity) } {
                result.kind = event_kind::DATA;
                result.connection_id = id;
                result.channel = channel as i32;
                result.data_length = payload.len() as i32;
            } else {
                // The transport sizes its receive buffer from the configured
                // maximums, so this only happens if those disagree with the
                // native side. Surface it instead of silently dropping data.
                let message = format!(
                    "dropped a {} byte message: the receive buffer only holds {capacity} bytes",
                    payload.len()
                );
                result.kind = event_kind::ERROR;
                result.connection_id = id;
                result.code = common::error_code::INVALID_RECEIVE;
                result.data_length = unsafe { write_string(&message, buffer, capacity) };
            }
            recycle(payload);
        }
        Event::Disconnected { id, code, reason } => {
            result.kind = event_kind::DISCONNECTED;
            result.connection_id = id;
            result.code = code;
            result.data_length = unsafe { write_string(&reason, buffer, capacity) };
        }
        Event::Error { id, code, message } => {
            result.kind = event_kind::ERROR;
            result.connection_id = id;
            result.code = code;
            result.data_length = unsafe { write_string(&message, buffer, capacity) };
        }
        Event::CertificateRotated { hash } => {
            result.kind = event_kind::CERTIFICATE_ROTATED;
            result.data_length = unsafe { write_string(&hash, buffer, capacity) };
        }
    }

    if out.is_null() {
        return 0;
    }

    unsafe { std::ptr::write(out, result) };
    1
}

fn parse_sans(raw: Option<String>) -> Vec<String> {
    let names: Vec<String> = raw
        .unwrap_or_default()
        .split(',')
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();

    if names.is_empty() {
        vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ]
    } else {
        names
    }
}

// ---------------------------------------------------------------------------
// Shared entry points
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn mwt_abi_version() -> u32 {
    ABI_VERSION
}

/// Copies the message of the most recent failure. Returns the byte count.
///
/// # Safety
/// `buffer` must be writable for `capacity` bytes.
#[no_mangle]
pub unsafe extern "C" fn mwt_last_error(buffer: *mut u8, capacity: i32) -> i32 {
    guard(0, || {
        let error = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { write_string(&error, buffer, capacity) }
    })
}

/// Pops one queued log line. Returns the byte count, or -1 when the queue is empty.
///
/// # Safety
/// `out_level` must point at a writable `i32`, `buffer` must be writable for
/// `capacity` bytes.
#[no_mangle]
pub unsafe extern "C" fn mwt_poll_log(
    out_level: *mut i32,
    buffer: *mut u8,
    capacity: i32,
) -> i32 {
    guard(-1, || match common::pop_log() {
        Some((level, message)) => {
            if !out_level.is_null() {
                unsafe { std::ptr::write(out_level, level) };
            }
            unsafe { write_string(&message, buffer, capacity) }
        }
        None => -1,
    })
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Starts the server. Returns 0 on success and -1 on failure, in which case
/// `mwt_last_error` explains why.
///
/// # Safety
/// `config` must point at a valid `MwtServerConfig` whose string fields are
/// null or NUL terminated.
#[no_mangle]
pub unsafe extern "C" fn mwt_server_start(config: *const MwtServerConfig) -> i32 {
    guard(-1, || {
        if config.is_null() {
            set_last_error("mwt_server_start was called with a null configuration");
            return -1;
        }

        let config = unsafe { &*config };

        let certificate = unsafe { read_string(config.certificate_path) };
        let key = unsafe { read_string(config.key_path) };

        let identity = match (certificate, key) {
            (Some(certificate), Some(key)) => IdentitySource::PemFiles { certificate, key },
            (None, None) => IdentitySource::SelfSigned {
                subject_alt_names: parse_sans(unsafe { read_string(config.subject_alt_names) }),
            },
            _ => {
                set_last_error("both a certificate and a key path are required for TLS");
                return -1;
            }
        };

        let settings = ServerSettings {
            port: config.port as u16,
            bind_mode: config.bind_mode,
            identity,
            keep_alive_ms: config.keep_alive_ms,
            idle_timeout_ms: config.idle_timeout_ms,
            handshake_timeout_ms: config.handshake_timeout_ms,
            max_reliable_payload: config.max_reliable_payload as usize,
            max_unreliable_payload: config.max_unreliable_payload as usize,
            max_connections: config.max_connections,
            certificate_validity_secs: config.certificate_validity_secs as u64,
            certificate_rotation_secs: config.certificate_rotation_secs as u64,
        };

        let mut slot = SERVER.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            set_last_error("the server is already running");
            return -1;
        }

        match Server::start(settings) {
            Ok(server) => {
                *slot = Some(server);
                0
            }
            Err(message) => {
                set_last_error(message);
                -1
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn mwt_server_stop() {
    guard((), || {
        let server = SERVER.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(server) = server {
            server.stop();
        }
    })
}

#[no_mangle]
pub extern "C" fn mwt_server_is_active() -> i32 {
    guard(0, || {
        i32::from(
            SERVER
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some(),
        )
    })
}

/// Returns the UDP port the server actually bound, or -1 when it is not running.
#[no_mangle]
pub extern "C" fn mwt_server_local_port() -> i32 {
    guard(-1, || {
        SERVER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|server| server.local_port() as i32)
            .unwrap_or(-1)
    })
}

#[no_mangle]
pub extern "C" fn mwt_server_connection_count() -> i32 {
    guard(0, || {
        SERVER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|server| server.connection_count() as i32)
            .unwrap_or(0)
    })
}

/// Copies the SHA-256 hash of the self signed certificate as dotted hex.
/// Returns the byte count, or -1 when the server uses a real certificate.
///
/// # Safety
/// `buffer` must be writable for `capacity` bytes.
#[no_mangle]
pub unsafe extern "C" fn mwt_server_certificate_hash(buffer: *mut u8, capacity: i32) -> i32 {
    guard(-1, || {
        let slot = SERVER.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref().and_then(|server| server.certificate_hash()) {
            Some(hash) => unsafe { write_string(&hash, buffer, capacity) },
            None => -1,
        }
    })
}

/// Copies the `ip:port` of a connected client. Returns the byte count, or -1
/// when the connection is unknown.
///
/// # Safety
/// `buffer` must be writable for `capacity` bytes.
#[no_mangle]
pub unsafe extern "C" fn mwt_server_client_address(
    connection_id: u32,
    buffer: *mut u8,
    capacity: i32,
) -> i32 {
    guard(-1, || {
        let slot = SERVER.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref().and_then(|server| server.address_of(connection_id)) {
            Some(address) => unsafe { write_string(&address, buffer, capacity) },
            None => -1,
        }
    })
}

/// Pops one server event. Returns 1 when an event was written and 0 when the
/// queue is empty.
///
/// # Safety
/// `out` must point at a writable `MwtEvent` and `buffer` must be writable for
/// `capacity` bytes.
#[no_mangle]
pub unsafe extern "C" fn mwt_server_poll(
    out: *mut MwtEvent,
    buffer: *mut u8,
    capacity: i32,
) -> i32 {
    guard(0, || {
        let mut slot = SERVER.lock().unwrap_or_else(|e| e.into_inner());
        let Some(server) = slot.as_mut() else {
            return 0;
        };
        let Some(event) = server.poll() else {
            return 0;
        };
        unsafe { write_event(event, out, buffer, capacity, |payload| server.recycle(payload)) }
    })
}

/// Sends one message. Returns 0 on success and -1 on failure.
///
/// # Safety
/// `data` must be readable for `length` bytes.
#[no_mangle]
pub unsafe extern "C" fn mwt_server_send(
    connection_id: u32,
    channel: i32,
    reliable: i32,
    data: *const u8,
    length: i32,
) -> i32 {
    guard(-1, || {
        let Some(payload) = (unsafe { as_slice(data, length) }) else {
            set_last_error("mwt_server_send was called with an invalid buffer");
            return -1;
        };

        let slot = SERVER.lock().unwrap_or_else(|e| e.into_inner());
        let Some(server) = slot.as_ref() else {
            set_last_error("the server is not running");
            return -1;
        };

        match server.send(connection_id, channel as u8, reliable != 0, payload) {
            Ok(()) => 0,
            Err(message) => {
                set_last_error(message);
                -1
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn mwt_server_disconnect(connection_id: u32) {
    guard((), || {
        if let Some(server) = SERVER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            server.disconnect(connection_id);
        }
    })
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Starts connecting. Returns 0 on success and -1 on failure.
///
/// # Safety
/// `config` must point at a valid `MwtClientConfig` whose string fields are
/// null or NUL terminated.
#[no_mangle]
pub unsafe extern "C" fn mwt_client_connect(config: *const MwtClientConfig) -> i32 {
    guard(-1, || {
        if config.is_null() {
            set_last_error("mwt_client_connect was called with a null configuration");
            return -1;
        }

        let config = unsafe { &*config };

        let Some(url) = (unsafe { read_string(config.url) }) else {
            set_last_error("mwt_client_connect was called without a url");
            return -1;
        };

        let settings = ClientSettings {
            url,
            certificate_hash: unsafe { read_string(config.certificate_hash) },
            allow_invalid_certificates: config.allow_invalid_certificates != 0,
            connect_timeout_ms: config.connect_timeout_ms,
            keep_alive_ms: config.keep_alive_ms,
            idle_timeout_ms: config.idle_timeout_ms,
            max_reliable_payload: config.max_reliable_payload as usize,
            max_unreliable_payload: config.max_unreliable_payload as usize,
        };

        // Replace any previous session rather than refusing: Mirror expects a
        // second ClientConnect to just work.
        let previous = CLIENT.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(previous) = previous {
            previous.stop();
        }

        match Client::connect(settings) {
            Ok(client) => {
                *CLIENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(client);
                0
            }
            Err(message) => {
                set_last_error(message);
                -1
            }
        }
    })
}

/// Asks the session to close. The disconnect still arrives through
/// `mwt_client_poll`.
#[no_mangle]
pub extern "C" fn mwt_client_disconnect() {
    guard((), || {
        if let Some(client) = CLIENT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            client.disconnect();
        }
    })
}

/// Tears the client down immediately, without emitting anything further.
#[no_mangle]
pub extern "C" fn mwt_client_stop() {
    guard((), || {
        let client = CLIENT.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(client) = client {
            client.stop();
        }
    })
}

/// 0 disconnected, 1 connecting, 2 connected.
#[no_mangle]
pub extern "C" fn mwt_client_state() -> i32 {
    guard(0, || {
        CLIENT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|client| client.state())
            .unwrap_or(crate::client::state::DISCONNECTED)
    })
}

/// Pops one client event. Returns 1 when an event was written and 0 when the
/// queue is empty.
///
/// # Safety
/// `out` must point at a writable `MwtEvent` and `buffer` must be writable for
/// `capacity` bytes.
#[no_mangle]
pub unsafe extern "C" fn mwt_client_poll(
    out: *mut MwtEvent,
    buffer: *mut u8,
    capacity: i32,
) -> i32 {
    guard(0, || {
        let mut slot = CLIENT.lock().unwrap_or_else(|e| e.into_inner());
        let Some(client) = slot.as_mut() else {
            return 0;
        };
        let Some(event) = client.poll() else {
            return 0;
        };
        unsafe { write_event(event, out, buffer, capacity, |payload| client.recycle(payload)) }
    })
}

/// Sends one message to the server. Returns 0 on success and -1 on failure.
///
/// # Safety
/// `data` must be readable for `length` bytes.
#[no_mangle]
pub unsafe extern "C" fn mwt_client_send(
    channel: i32,
    reliable: i32,
    data: *const u8,
    length: i32,
) -> i32 {
    guard(-1, || {
        let Some(payload) = (unsafe { as_slice(data, length) }) else {
            set_last_error("mwt_client_send was called with an invalid buffer");
            return -1;
        };

        let slot = CLIENT.lock().unwrap_or_else(|e| e.into_inner());
        let Some(client) = slot.as_ref() else {
            set_last_error("the client is not connected");
            return -1;
        };

        match client.send(channel as u8, reliable != 0, payload) {
            Ok(()) => 0,
            Err(message) => {
                set_last_error(message);
                -1
            }
        }
    })
}

/// # Safety
/// `data` must be readable for `length` bytes when both are non-null/positive.
unsafe fn as_slice<'a>(data: *const u8, length: i32) -> Option<&'a [u8]> {
    if length < 0 {
        return None;
    }
    if length == 0 {
        return Some(&[]);
    }
    if data.is_null() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(data, length as usize) })
}

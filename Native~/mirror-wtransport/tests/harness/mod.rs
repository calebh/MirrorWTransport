//! Shared plumbing for the FFI tests: polling, log draining and event capture.

#![allow(dead_code)]

use std::time::Duration;
use std::time::Instant;

use mirror_wtransport::*;

pub const KIND_CONNECTED: i32 = 1;
pub const KIND_DATA: i32 = 2;
pub const KIND_DISCONNECTED: i32 = 3;
pub const KIND_ERROR: i32 = 4;

pub const RELIABLE: i32 = 1;
pub const UNRELIABLE: i32 = 0;

pub const BUFFER_SIZE: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub kind: i32,
    pub connection_id: u32,
    pub channel: i32,
    pub code: i32,
    pub data: Vec<u8>,
}

fn empty_event() -> MwtEvent {
    MwtEvent {
        kind: 0,
        connection_id: 0,
        channel: 0,
        data_length: 0,
        code: 0,
    }
}

fn capture(event: &MwtEvent, buffer: &[u8]) -> Captured {
    let length = event.data_length.max(0) as usize;
    Captured {
        kind: event.kind,
        connection_id: event.connection_id,
        channel: event.channel,
        code: event.code,
        data: buffer[..length].to_vec(),
    }
}

pub fn poll_server(buffer: &mut [u8]) -> Option<Captured> {
    let mut event = empty_event();
    let popped = unsafe { mwt_server_poll(&mut event, buffer.as_mut_ptr(), buffer.len() as i32) };
    (popped == 1).then(|| capture(&event, buffer))
}

pub fn poll_client(buffer: &mut [u8]) -> Option<Captured> {
    let mut event = empty_event();
    let popped = unsafe { mwt_client_poll(&mut event, buffer.as_mut_ptr(), buffer.len() as i32) };
    (popped == 1).then(|| capture(&event, buffer))
}

pub fn drain_logs() {
    let mut buffer = vec![0u8; 2048];
    loop {
        let mut level = 0;
        let length = unsafe { mwt_poll_log(&mut level, buffer.as_mut_ptr(), buffer.len() as i32) };
        if length < 0 {
            return;
        }
        println!(
            "  [native log {level}] {}",
            String::from_utf8_lossy(&buffer[..length.max(0) as usize])
        );
    }
}

pub fn last_error() -> String {
    let mut buffer = vec![0u8; 2048];
    let length = unsafe { mwt_last_error(buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf8_lossy(&buffer[..length.max(0) as usize]).into_owned()
}

/// Pumps both sides until `done` is satisfied, or panics with what it did see.
pub fn pump<F>(
    server: &mut Vec<Captured>,
    client: &mut Vec<Captured>,
    timeout: Duration,
    what: &str,
    done: F,
) where
    F: Fn(&[Captured], &[Captured]) -> bool,
{
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        while let Some(event) = poll_server(&mut buffer) {
            server.push(event);
        }
        while let Some(event) = poll_client(&mut buffer) {
            client.push(event);
        }
        drain_logs();

        if done(server, client) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    panic!("timed out waiting for {what}\n  server: {server:#?}\n  client: {client:#?}");
}

pub fn utf8(data: &[u8]) -> String {
    String::from_utf8_lossy(data).into_owned()
}

pub fn send_from_client(channel: i32, reliable: i32, payload: &[u8]) {
    let sent = unsafe { mwt_client_send(channel, reliable, payload.as_ptr(), payload.len() as i32) };
    assert_eq!(sent, 0, "client send failed: {}", last_error());
}

pub fn send_from_server(id: u32, channel: i32, reliable: i32, payload: &[u8]) {
    let sent =
        unsafe { mwt_server_send(id, channel, reliable, payload.as_ptr(), payload.len() as i32) };
    assert_eq!(sent, 0, "server send failed: {}", last_error());
}

/// Starts a self signed server on an ephemeral port. Returns the port and the
/// certificate hash a client has to pin.
pub fn start_self_signed_server() -> (i32, String) {
    use std::ffi::c_char;
    use std::ffi::CString;

    let sans = CString::new("localhost,127.0.0.1,::1").unwrap();

    let config = MwtServerConfig {
        certificate_path: std::ptr::null(),
        key_path: std::ptr::null(),
        subject_alt_names: sans.as_ptr() as *const c_char,
        port: 0,
        bind_mode: 0, // any address, dual stack
        keep_alive_ms: 1000,
        idle_timeout_ms: 10000,
        handshake_timeout_ms: 5000,
        max_reliable_payload: 65536 + 64,
        max_unreliable_payload: 1024 + 64,
        max_connections: 0,
    };

    assert_eq!(
        unsafe { mwt_server_start(&config) },
        0,
        "server failed to start: {}",
        last_error()
    );

    let port = mwt_server_local_port();
    assert!(port > 0, "no port was bound");

    let mut buffer = vec![0u8; 256];
    let length =
        unsafe { mwt_server_certificate_hash(buffer.as_mut_ptr(), buffer.len() as i32) };
    assert!(length > 0, "no self signed certificate hash");

    (port, utf8(&buffer[..length as usize]))
}

pub fn connect_client(port: i32, hash: &str, connect_timeout_ms: u32) {
    use std::ffi::c_char;
    use std::ffi::CString;

    let url = CString::new(format!("https://localhost:{port}/")).unwrap();
    let hash = CString::new(hash.to_string()).unwrap();

    let config = MwtClientConfig {
        url: url.as_ptr() as *const c_char,
        certificate_hash: hash.as_ptr() as *const c_char,
        allow_invalid_certificates: 0,
        connect_timeout_ms,
        keep_alive_ms: 1000,
        idle_timeout_ms: 10000,
        max_reliable_payload: 65536 + 64,
        max_unreliable_payload: 1024 + 64,
    };

    assert_eq!(
        unsafe { mwt_client_connect(&config) },
        0,
        "client failed to start connecting: {}",
        last_error()
    );
}

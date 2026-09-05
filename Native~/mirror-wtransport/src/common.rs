//! Wire protocol, buffer recycling and the event queue shared by server and client.
//!
//! Wire protocol (identical in both directions, and identical for the browser
//! client implemented in `MirrorWTransport.jslib`):
//!
//! * One WebTransport *bidirectional stream* per connection carries the
//!   reliable-ordered channel. The client opens it right after the session is
//!   established and writes `PROTOCOL_MAGIC`; the server replies with the same
//!   4 bytes once it has accepted the stream. Everything after the prologue is
//!   a sequence of frames:
//!
//!   ```text
//!   [payload length : u32 big endian][mirror channel id : u8][payload]
//!   ```
//!
//! * WebTransport *datagrams* carry the unreliable channel. A datagram is a
//!   single message, so it only needs the channel byte:
//!
//!   ```text
//!   [mirror channel id : u8][payload]
//!   ```
//!
//! The mirror channel id is carried explicitly because Mirror hands the channel
//! it was sent on back to message handlers, and several Mirror channel ids map
//! onto the same delivery mode.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

/// Prologue exchanged on the reliable stream before any frame.
pub const PROTOCOL_MAGIC: [u8; PROTOCOL_MAGIC_LEN] = *b"MWT1";

pub const PROTOCOL_MAGIC_LEN: usize = 4;

/// `u32` length + `u8` channel.
pub const FRAME_HEADER_LEN: usize = 5;

/// Channel byte + payload.
pub const DATAGRAM_HEADER_LEN: usize = 1;

/// Application error code used whenever we close a session ourselves.
pub const CLOSE_CODE_NORMAL: u32 = 0;

/// Application error code used when the peer violated the wire protocol.
pub const CLOSE_CODE_PROTOCOL: u32 = 1;

// ---------------------------------------------------------------------------
// Event queue
// ---------------------------------------------------------------------------

/// Event kinds as seen by C#. Keep in sync with `WTEventKind`.
#[allow(dead_code)]
pub mod event_kind {
    pub const NONE: i32 = 0;
    pub const CONNECTED: i32 = 1;
    pub const DATA: i32 = 2;
    pub const DISCONNECTED: i32 = 3;
    pub const ERROR: i32 = 4;
    pub const CERTIFICATE_ROTATED: i32 = 5;
}

/// Error codes as seen by C#. Keep in sync with `WTErrorCode`.
#[allow(dead_code)]
pub mod error_code {
    pub const NONE: i32 = 0;
    pub const DNS_RESOLVE: i32 = 1;
    pub const REFUSED: i32 = 2;
    pub const TIMEOUT: i32 = 3;
    pub const CONGESTION: i32 = 4;
    pub const INVALID_RECEIVE: i32 = 5;
    pub const INVALID_SEND: i32 = 6;
    pub const CONNECTION_CLOSED: i32 = 7;
    pub const UNEXPECTED: i32 = 8;
}

pub enum Event {
    /// `address` is the peer ip:port on the server, or the connect url on the client.
    Connected { id: u32, address: String },
    Data { id: u32, channel: u8, payload: Vec<u8> },
    Disconnected { id: u32, code: i32, reason: String },
    Error { id: u32, code: i32, message: String },
    /// The server installed a new certificate. `hash` is the new SHA-256 as
    /// dotted hex, or empty when the identity came from PEM files and there is
    /// nothing for a client to pin.
    CertificateRotated { hash: String },
}

pub type EventSender = tokio::sync::mpsc::UnboundedSender<Event>;
pub type EventReceiver = tokio::sync::mpsc::UnboundedReceiver<Event>;

// ---------------------------------------------------------------------------
// Buffer pool
// ---------------------------------------------------------------------------

/// Recycles the byte buffers that carry payloads between the tokio tasks and
/// the polling FFI calls, so a busy server does not allocate per message.
#[derive(Clone, Default)]
pub struct BufPool(Arc<Mutex<Vec<Vec<u8>>>>);

impl BufPool {
    const MAX_POOLED: usize = 512;

    /// Buffers larger than this are dropped instead of pooled, so that one
    /// oversized message does not pin memory forever.
    const MAX_POOLED_CAPACITY: usize = 128 * 1024;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn take(&self, capacity: usize) -> Vec<u8> {
        let mut pool = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match pool.pop() {
            Some(mut buffer) => {
                buffer.clear();
                buffer.reserve(capacity);
                buffer
            }
            None => Vec::with_capacity(capacity),
        }
    }

    pub fn give(&self, buffer: Vec<u8>) {
        if buffer.capacity() == 0 || buffer.capacity() > Self::MAX_POOLED_CAPACITY {
            return;
        }
        let mut pool = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < Self::MAX_POOLED {
            pool.push(buffer);
        }
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Writes `[len][channel][payload]` into `out`, replacing its contents.
pub fn write_frame(out: &mut Vec<u8>, channel: u8, payload: &[u8]) {
    out.clear();
    out.reserve(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.push(channel);
    out.extend_from_slice(payload);
}

/// Writes `[channel][payload]` into `out`, replacing its contents.
pub fn write_datagram(out: &mut Vec<u8>, channel: u8, payload: &[u8]) {
    out.clear();
    out.reserve(DATAGRAM_HEADER_LEN + payload.len());
    out.push(channel);
    out.extend_from_slice(payload);
}

#[derive(Debug)]
pub enum FrameError {
    /// A peer announced a payload larger than the configured maximum. Treated
    /// as hostile: the only sane response is to drop the connection, because
    /// the stream can no longer be resynchronised.
    TooLarge { announced: usize, maximum: usize },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge { announced, maximum } => write!(
                f,
                "peer announced a {announced} byte reliable message, maximum is {maximum}"
            ),
        }
    }
}

/// Incremental reader that turns the byte stream of a reliable stream back into
/// discrete Mirror messages.
pub struct FrameReader {
    buffer: Vec<u8>,
    start: usize,
    max_payload: usize,
}

impl FrameReader {
    /// Compact once the consumed prefix grows past this, to keep the memmove
    /// amortised instead of doing it after every single frame.
    const COMPACT_THRESHOLD: usize = 64 * 1024;

    pub fn new(max_payload: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(8 * 1024),
            start: 0,
            max_payload,
        }
    }

    pub fn extend(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    fn available(&self) -> usize {
        self.buffer.len() - self.start
    }

    /// Pops the next complete frame, or `None` when more bytes are needed.
    pub fn next_frame(&mut self, pool: &BufPool) -> Result<Option<(u8, Vec<u8>)>, FrameError> {
        loop {
            if self.available() < FRAME_HEADER_LEN {
                self.compact();
                return Ok(None);
            }

            let header = &self.buffer[self.start..self.start + FRAME_HEADER_LEN];
            let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
            let channel = header[4];

            if length > self.max_payload {
                return Err(FrameError::TooLarge {
                    announced: length,
                    maximum: self.max_payload,
                });
            }

            if self.available() < FRAME_HEADER_LEN + length {
                self.compact();
                return Ok(None);
            }

            let payload_start = self.start + FRAME_HEADER_LEN;
            self.start = payload_start + length;

            // Empty frames carry nothing Mirror can parse. Skip them rather
            // than handing Mirror a zero length segment it would only warn about.
            if length == 0 {
                continue;
            }

            let mut payload = pool.take(length);
            payload.extend_from_slice(&self.buffer[payload_start..payload_start + length]);
            return Ok(Some((channel, payload)));
        }
    }

    fn compact(&mut self) {
        if self.start == 0 {
            return;
        }
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        } else if self.start >= Self::COMPACT_THRESHOLD {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Log queue
// ---------------------------------------------------------------------------

/// Log levels as seen by C#. Keep in sync with `WTLogLevel`.
pub mod log_level {
    pub const INFO: i32 = 0;
    pub const WARN: i32 = 1;
    pub const ERROR: i32 = 2;
}

static LOGS: Mutex<VecDeque<(i32, String)>> = Mutex::new(VecDeque::new());

/// Background threads cannot call into Unity, so they queue log lines here and
/// the transport drains them from the main thread every frame.
pub fn log(level: i32, message: impl Into<String>) {
    const MAX_QUEUED: usize = 512;
    let mut logs = LOGS.lock().unwrap_or_else(|e| e.into_inner());
    if logs.len() >= MAX_QUEUED {
        logs.pop_front();
    }
    logs.push_back((level, message.into()));
}

pub fn log_info(message: impl Into<String>) {
    log(log_level::INFO, message);
}

pub fn log_warn(message: impl Into<String>) {
    log(log_level::WARN, message);
}

pub fn log_error(message: impl Into<String>) {
    log(log_level::ERROR, message);
}

pub fn pop_log() -> Option<(i32, String)> {
    LOGS.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(channel: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, channel, payload);
        out
    }

    fn drain(reader: &mut FrameReader, pool: &BufPool) -> Vec<(u8, Vec<u8>)> {
        let mut messages = Vec::new();
        while let Some(message) = reader.next_frame(pool).expect("frame error") {
            messages.push(message);
        }
        messages
    }

    #[test]
    fn reassembles_messages_split_across_reads() {
        let pool = BufPool::new();
        let mut reader = FrameReader::new(65536);

        let mut stream = Vec::new();
        stream.extend_from_slice(&frame(0, b"first"));
        stream.extend_from_slice(&frame(3, b"second message"));
        stream.extend_from_slice(&frame(1, b"x"));

        // One byte at a time is the worst case the reader has to survive.
        let mut messages = Vec::new();
        for byte in &stream {
            reader.extend(std::slice::from_ref(byte));
            messages.extend(drain(&mut reader, &pool));
        }

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], (0, b"first".to_vec()));
        assert_eq!(messages[1], (3, b"second message".to_vec()));
        assert_eq!(messages[2], (1, b"x".to_vec()));
    }

    #[test]
    fn holds_an_incomplete_frame_until_the_rest_arrives() {
        let pool = BufPool::new();
        let mut reader = FrameReader::new(65536);

        let complete = frame(0, b"hello");
        reader.extend(&complete[..7]);
        assert!(drain(&mut reader, &pool).is_empty());

        reader.extend(&complete[7..]);
        assert_eq!(drain(&mut reader, &pool), vec![(0, b"hello".to_vec())]);
    }

    #[test]
    fn skips_empty_frames() {
        let pool = BufPool::new();
        let mut reader = FrameReader::new(65536);

        reader.extend(&frame(0, b""));
        reader.extend(&frame(0, b"real"));

        assert_eq!(drain(&mut reader, &pool), vec![(0, b"real".to_vec())]);
    }

    #[test]
    fn rejects_an_announced_length_above_the_maximum() {
        let pool = BufPool::new();
        let mut reader = FrameReader::new(64);

        reader.extend(&frame(0, &[0u8; 65]));

        match reader.next_frame(&pool) {
            Err(FrameError::TooLarge { announced, maximum }) => {
                assert_eq!(announced, 65);
                assert_eq!(maximum, 64);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn survives_compaction_under_a_long_stream() {
        let pool = BufPool::new();
        let mut reader = FrameReader::new(128 * 1024);

        // Enough traffic to cross the compaction threshold several times.
        let payload = vec![7u8; 1024];
        let mut expected = 0;
        for index in 0..300 {
            reader.extend(&frame((index % 5) as u8, &payload));
            let messages = drain(&mut reader, &pool);
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].0, (index % 5) as u8);
            assert_eq!(messages[0].1, payload);
            expected += 1;
        }
        assert_eq!(expected, 300);
    }

    #[test]
    fn datagram_layout_round_trips() {
        let mut out = Vec::new();
        write_datagram(&mut out, 4, b"payload");

        let (channel, payload) = out.split_first().expect("non empty");
        assert_eq!(*channel, 4);
        assert_eq!(payload, &b"payload"[..]);
    }

    #[test]
    fn pool_reuses_buffers_without_leaking_old_contents() {
        let pool = BufPool::new();

        let mut buffer = pool.take(16);
        buffer.extend_from_slice(b"stale");
        pool.give(buffer);

        let recycled = pool.take(16);
        assert!(recycled.is_empty());
    }
}

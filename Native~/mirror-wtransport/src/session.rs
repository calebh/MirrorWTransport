//! The per-connection read/write loops. Server and client run exactly the same
//! code here, which is what keeps the two ends of the wire protocol in step.

use tokio::sync::mpsc;
use wtransport::error::ConnectionError;
use wtransport::Connection;
use wtransport::RecvStream;
use wtransport::SendStream;
use wtransport::VarInt;

use crate::common::error_code;
use crate::common::BufPool;
use crate::common::Event;
use crate::common::EventSender;
use crate::common::FrameReader;
use crate::common::CLOSE_CODE_PROTOCOL;

/// Queued towards the reliable writer task.
pub enum OutMsg {
    Frame(Vec<u8>),
    /// Flush whatever is queued, finish the stream, then close the session.
    Close { code: u32 },
}

pub type OutSender = mpsc::UnboundedSender<OutMsg>;
pub type OutReceiver = mpsc::UnboundedReceiver<OutMsg>;

/// Serialises everything Mirror sends on the reliable channel onto the single
/// bidirectional stream, coalescing whatever is already queued into one write.
pub async fn reliable_writer(
    mut send: SendStream,
    mut queue: OutReceiver,
    connection: Connection,
    pool: BufPool,
) {
    const MAX_BATCH: usize = 64 * 1024;

    let mut batch: Vec<u8> = Vec::with_capacity(16 * 1024);

    while let Some(first) = queue.recv().await {
        batch.clear();

        let mut close_with = match first {
            OutMsg::Frame(frame) => {
                batch.extend_from_slice(&frame);
                pool.give(frame);
                None
            }
            OutMsg::Close { code } => Some(code),
        };

        while close_with.is_none() && batch.len() < MAX_BATCH {
            match queue.try_recv() {
                Ok(OutMsg::Frame(frame)) => {
                    batch.extend_from_slice(&frame);
                    pool.give(frame);
                }
                Ok(OutMsg::Close { code }) => close_with = Some(code),
                Err(_) => break,
            }
        }

        if !batch.is_empty() && send.write_all(&batch).await.is_err() {
            break;
        }

        if let Some(code) = close_with {
            let _ = send.finish().await;
            connection.close(VarInt::from_u32(code), b"closed by application");
            break;
        }
    }
}

/// Reassembles Mirror messages from the reliable stream.
pub async fn reliable_reader(
    mut recv: RecvStream,
    id: u32,
    connection: Connection,
    events: EventSender,
    pool: BufPool,
    max_payload: usize,
) {
    let mut reader = FrameReader::new(max_payload);
    let mut chunk = vec![0u8; 16 * 1024];

    loop {
        match recv.read(&mut chunk).await {
            Ok(Some(read)) => reader.extend(&chunk[..read]),
            // Ok(None) is a clean end of stream, Err is a reset or a dead
            // connection. Either way this session is over.
            _ => break,
        }

        loop {
            match reader.next_frame(&pool) {
                Ok(Some((channel, payload))) => {
                    let _ = events.send(Event::Data {
                        id,
                        channel,
                        payload,
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = events.send(Event::Error {
                        id,
                        code: error_code::INVALID_RECEIVE,
                        message: e.to_string(),
                    });
                    connection
                        .close(VarInt::from_u32(CLOSE_CODE_PROTOCOL), b"invalid reliable frame");
                    return;
                }
            }
        }
    }
}

/// Turns incoming datagrams back into Mirror messages on the unreliable channel.
pub async fn datagram_reader(
    connection: Connection,
    id: u32,
    events: EventSender,
    pool: BufPool,
    max_payload: usize,
) {
    loop {
        let datagram = match connection.receive_datagram().await {
            Ok(datagram) => datagram,
            Err(_) => break,
        };

        let bytes: &[u8] = &datagram;
        let Some((&channel, payload)) = bytes.split_first() else {
            continue;
        };

        if payload.len() > max_payload {
            continue;
        }

        let mut buffer = pool.take(payload.len());
        buffer.extend_from_slice(payload);
        let _ = events.send(Event::Data {
            id,
            channel,
            payload: buffer,
        });
    }
}

/// Maps a wtransport connection error onto the Mirror-facing error code plus a
/// human readable reason.
pub fn describe(error: &ConnectionError) -> (i32, String) {
    let code = match error {
        ConnectionError::TimedOut => error_code::TIMEOUT,
        ConnectionError::LocallyClosed
        | ConnectionError::ApplicationClosed(_)
        | ConnectionError::ConnectionClosed(_) => error_code::CONNECTION_CLOSED,
        ConnectionError::LocalH3Error(_) | ConnectionError::QuicProto(_) => {
            error_code::INVALID_RECEIVE
        }
        ConnectionError::CidsExhausted => error_code::UNEXPECTED,
    };

    (code, error.to_string())
}

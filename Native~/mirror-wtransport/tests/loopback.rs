//! End to end test of the C ABI: starts a real server, connects a real client
//! to it over loopback QUIC, and pushes traffic both ways on both channels.
//!
//! This drives exactly the entry points Unity calls, in the order Unity calls
//! them, so it covers the session handshake, the framing, the datagram path and
//! the event queues in one go.
//!
//! One test function, because the FFI is deliberately global: there is one
//! server and one client per process, just as there is one of each per player.

use std::time::Duration;

use mirror_wtransport::*;

mod harness;
use harness::*;

#[test]
fn client_and_server_talk_over_loopback() {
    assert_eq!(mwt_abi_version(), 1);

    let mut server_events: Vec<Captured> = Vec::new();
    let mut client_events: Vec<Captured> = Vec::new();

    let (port, hash) = start_self_signed_server();
    assert_eq!(mwt_server_is_active(), 1);
    println!("server on port {port}, certificate {hash}");

    connect_client(port, &hash, 10000);

    pump(
        &mut server_events,
        &mut client_events,
        Duration::from_secs(15),
        "both sides to report a connection",
        |server, client| {
            server.iter().any(|e| e.kind == KIND_CONNECTED)
                && client.iter().any(|e| e.kind == KIND_CONNECTED)
        },
    );

    let connected = server_events
        .iter()
        .find(|e| e.kind == KIND_CONNECTED)
        .expect("server connect event");

    let connection_id = connected.connection_id;
    assert_ne!(
        connection_id, 0,
        "Mirror reserves connection id 0 for the host client"
    );
    println!("connection {connection_id} from {}", utf8(&connected.data));

    let mut address_buffer = vec![0u8; 256];
    let address_length = unsafe {
        mwt_server_client_address(
            connection_id,
            address_buffer.as_mut_ptr(),
            address_buffer.len() as i32,
        )
    };
    assert!(address_length > 0, "no address for the connection");
    assert_eq!(mwt_server_connection_count(), 1);

    // Neither side sees any traffic before its own connect event.
    assert_eq!(client_events[0].kind, KIND_CONNECTED);
    assert_eq!(server_events[0].kind, KIND_CONNECTED);

    server_events.clear();
    client_events.clear();

    // ---- client to server ----------------------------------------------
    // Ordering only means anything with a burst, and the channel byte only
    // means anything if several channels are in flight at once.
    for index in 0..50u32 {
        let message = format!("c2s reliable {index}");
        send_from_client(0, RELIABLE, message.as_bytes());
    }
    send_from_client(3, RELIABLE, b"c2s on channel three");
    send_from_client(1, UNRELIABLE, b"c2s datagram");

    // A message right at the configured limit, to prove the framing survives it.
    let big = vec![0xABu8; 65536];
    send_from_client(0, RELIABLE, &big);

    pump(
        &mut server_events,
        &mut client_events,
        Duration::from_secs(15),
        "the server to receive everything the client sent",
        |server, _| {
            server
                .iter()
                .filter(|e| e.kind == KIND_DATA && e.channel == 0)
                .count()
                >= 51
                && server.iter().any(|e| e.channel == 3)
                && server.iter().any(|e| e.channel == 1)
        },
    );

    let reliable: Vec<&Captured> = server_events
        .iter()
        .filter(|e| e.kind == KIND_DATA && e.channel == 0)
        .collect();

    for (index, event) in reliable.iter().take(50).enumerate() {
        assert_eq!(
            utf8(&event.data),
            format!("c2s reliable {index}"),
            "reliable messages arrived out of order"
        );
    }

    assert_eq!(
        reliable[50].data.len(),
        65536,
        "the large message was truncated"
    );
    assert!(reliable[50].data.iter().all(|&b| b == 0xAB));

    let channel_three = server_events
        .iter()
        .find(|e| e.channel == 3)
        .expect("channel 3 message");
    assert_eq!(utf8(&channel_three.data), "c2s on channel three");

    let datagram = server_events
        .iter()
        .find(|e| e.kind == KIND_DATA && e.channel == 1)
        .expect("datagram");
    assert_eq!(utf8(&datagram.data), "c2s datagram");

    assert!(
        server_events.iter().all(|e| e.kind != KIND_ERROR),
        "server reported an error: {server_events:#?}"
    );

    server_events.clear();
    client_events.clear();

    // ---- server to client ----------------------------------------------
    for index in 0..50u32 {
        let message = format!("s2c reliable {index}");
        send_from_server(connection_id, 0, RELIABLE, message.as_bytes());
    }
    send_from_server(connection_id, 4, UNRELIABLE, b"s2c datagram");

    pump(
        &mut server_events,
        &mut client_events,
        Duration::from_secs(15),
        "the client to receive everything the server sent",
        |_, client| {
            client
                .iter()
                .filter(|e| e.kind == KIND_DATA && e.channel == 0)
                .count()
                >= 50
                && client.iter().any(|e| e.channel == 4)
        },
    );

    let received: Vec<&Captured> = client_events
        .iter()
        .filter(|e| e.kind == KIND_DATA && e.channel == 0)
        .collect();

    for (index, event) in received.iter().take(50).enumerate() {
        assert_eq!(
            utf8(&event.data),
            format!("s2c reliable {index}"),
            "reliable messages arrived out of order"
        );
    }

    let datagram = client_events
        .iter()
        .find(|e| e.kind == KIND_DATA && e.channel == 4)
        .expect("datagram");
    assert_eq!(utf8(&datagram.data), "s2c datagram");

    assert!(
        client_events.iter().all(|e| e.kind != KIND_ERROR),
        "client reported an error: {client_events:#?}"
    );

    // ---- oversized sends are refused, not truncated ---------------------
    let too_big = vec![0u8; 4096];
    let refused = unsafe { mwt_client_send(1, UNRELIABLE, too_big.as_ptr(), too_big.len() as i32) };
    assert_eq!(refused, -1, "an oversized datagram should be refused");
    assert!(
        last_error().contains("exceeds"),
        "unhelpful error: {}",
        last_error()
    );

    server_events.clear();
    client_events.clear();

    // ---- disconnect from the server -------------------------------------
    // Anything queued in the same breath still has to reach the client.
    send_from_server(connection_id, 0, RELIABLE, b"goodbye");
    mwt_server_disconnect(connection_id);

    pump(
        &mut server_events,
        &mut client_events,
        Duration::from_secs(15),
        "both sides to report the disconnect",
        |server, client| {
            server.iter().any(|e| e.kind == KIND_DISCONNECTED)
                && client.iter().any(|e| e.kind == KIND_DISCONNECTED)
        },
    );

    assert!(
        client_events
            .iter()
            .any(|e| e.kind == KIND_DATA && utf8(&e.data) == "goodbye"),
        "the message sent just before the disconnect was dropped: {client_events:#?}"
    );

    // The disconnect is always the last thing either side hears.
    assert_eq!(client_events.last().unwrap().kind, KIND_DISCONNECTED);
    assert_eq!(server_events.last().unwrap().kind, KIND_DISCONNECTED);
    assert_eq!(server_events.last().unwrap().connection_id, connection_id);

    assert_eq!(mwt_client_state(), 0, "the client should be disconnected");
    assert_eq!(mwt_server_connection_count(), 0);

    // ---- shut down -------------------------------------------------------
    mwt_client_stop();
    mwt_server_stop();
    assert_eq!(mwt_server_is_active(), 0);
    drain_logs();
}

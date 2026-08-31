//! The paths Mirror leans on hardest, because its Transport contract says a
//! connect attempt "can not end in limbo": every one of these has to produce a
//! disconnect event rather than hanging.
//!
//! One test function, because the FFI is deliberately global: there is one
//! server and one client per process.

use std::time::Duration;

use mirror_wtransport::*;

mod harness;
use harness::*;

#[test]
fn every_failure_still_ends_in_a_disconnect() {
    let mut server: Vec<Captured> = Vec::new();
    let mut client: Vec<Captured> = Vec::new();

    // ---- nothing listening on the other end -----------------------------
    println!("phase 1: connecting to a port with no server on it");
    {
        // Port 1 is never a WebTransport server, and QUIC has no way to fail
        // fast on a closed UDP port, so this has to fall out of the timeout.
        connect_client(1, "", 2000);

        pump(
            &mut server,
            &mut client,
            Duration::from_secs(20),
            "the client to give up",
            |_, client| client.iter().any(|e| e.kind == KIND_DISCONNECTED),
        );

        assert!(
            client.iter().any(|e| e.kind == KIND_ERROR),
            "an error should be reported before the disconnect: {client:#?}"
        );
        assert_eq!(
            client.last().unwrap().kind,
            KIND_DISCONNECTED,
            "the disconnect has to come last"
        );
        assert_eq!(mwt_client_state(), 0);

        mwt_client_stop();
        client.clear();
        server.clear();
    }

    // ---- wrong certificate hash ------------------------------------------
    println!("phase 2: connecting with the wrong certificate hash");
    {
        let (port, _real_hash) = start_self_signed_server();

        let wrong_hash = (0..32)
            .map(|i| format!("{:02x}", i as u8))
            .collect::<Vec<_>>()
            .join(":");

        connect_client(port, &wrong_hash, 5000);

        pump(
            &mut server,
            &mut client,
            Duration::from_secs(20),
            "the client to reject the certificate",
            |_, client| client.iter().any(|e| e.kind == KIND_DISCONNECTED),
        );

        assert!(
            client.iter().all(|e| e.kind != KIND_CONNECTED),
            "a mismatched hash must never produce a connection: {client:#?}"
        );
        assert!(
            client.iter().any(|e| e.kind == KIND_ERROR),
            "the certificate failure should be reported: {client:#?}"
        );
        assert!(
            server.iter().all(|e| e.kind != KIND_CONNECTED),
            "the server must not report a connection either: {server:#?}"
        );

        mwt_client_stop();
        mwt_server_stop();
        client.clear();
        server.clear();
    }

    // ---- client hangs up --------------------------------------------------
    println!("phase 3: the client disconnects");
    {
        let (port, hash) = start_self_signed_server();
        connect_client(port, &hash, 5000);

        pump(
            &mut server,
            &mut client,
            Duration::from_secs(15),
            "both sides to connect",
            |server, client| {
                server.iter().any(|e| e.kind == KIND_CONNECTED)
                    && client.iter().any(|e| e.kind == KIND_CONNECTED)
            },
        );

        let connection_id = server
            .iter()
            .find(|e| e.kind == KIND_CONNECTED)
            .unwrap()
            .connection_id;

        server.clear();
        client.clear();

        // Queued in the same breath as the disconnect: it still has to arrive.
        send_from_client(0, RELIABLE, b"last words");
        mwt_client_disconnect();

        pump(
            &mut server,
            &mut client,
            Duration::from_secs(15),
            "both sides to notice the client leaving",
            |server, client| {
                server.iter().any(|e| e.kind == KIND_DISCONNECTED)
                    && client.iter().any(|e| e.kind == KIND_DISCONNECTED)
            },
        );

        assert!(
            server
                .iter()
                .any(|e| e.kind == KIND_DATA && utf8(&e.data) == "last words"),
            "the message queued just before the disconnect was dropped: {server:#?}"
        );
        assert_eq!(server.last().unwrap().kind, KIND_DISCONNECTED);
        assert_eq!(server.last().unwrap().connection_id, connection_id);
        assert_eq!(mwt_server_connection_count(), 0);
        assert_eq!(mwt_client_state(), 0);

        mwt_client_stop();
        mwt_server_stop();
        client.clear();
        server.clear();
    }

    // ---- the server goes away ---------------------------------------------
    println!("phase 4: the server shuts down under a live connection");
    {
        let (port, hash) = start_self_signed_server();
        connect_client(port, &hash, 5000);

        pump(
            &mut server,
            &mut client,
            Duration::from_secs(15),
            "both sides to connect",
            |server, client| {
                server.iter().any(|e| e.kind == KIND_CONNECTED)
                    && client.iter().any(|e| e.kind == KIND_CONNECTED)
            },
        );

        server.clear();
        client.clear();

        mwt_server_stop();
        assert_eq!(mwt_server_is_active(), 0);

        pump(
            &mut server,
            &mut client,
            Duration::from_secs(15),
            "the client to notice the server is gone",
            |_, client| client.iter().any(|e| e.kind == KIND_DISCONNECTED),
        );

        assert_eq!(mwt_client_state(), 0);
        mwt_client_stop();
    }

    // ---- reconnecting after all that still works ---------------------------
    println!("phase 5: reconnecting after a torn down session");
    {
        server.clear();
        client.clear();

        let (port, hash) = start_self_signed_server();
        connect_client(port, &hash, 5000);

        pump(
            &mut server,
            &mut client,
            Duration::from_secs(15),
            "a fresh connection",
            |server, client| {
                server.iter().any(|e| e.kind == KIND_CONNECTED)
                    && client.iter().any(|e| e.kind == KIND_CONNECTED)
            },
        );

        send_from_client(0, RELIABLE, b"still working");

        pump(
            &mut server,
            &mut client,
            Duration::from_secs(15),
            "traffic on the new connection",
            |server, _| {
                server
                    .iter()
                    .any(|e| e.kind == KIND_DATA && utf8(&e.data) == "still working")
            },
        );

        mwt_client_stop();
        mwt_server_stop();
    }

    // ---- calls against a stopped server are harmless ------------------------
    println!("phase 6: calls against a stopped server");
    {
        assert_eq!(mwt_server_is_active(), 0);
        assert_eq!(mwt_server_local_port(), -1);
        assert_eq!(mwt_server_connection_count(), 0);

        let mut buffer = vec![0u8; 64];
        assert_eq!(
            unsafe { mwt_server_certificate_hash(buffer.as_mut_ptr(), buffer.len() as i32) },
            -1
        );
        assert_eq!(
            unsafe { mwt_server_client_address(1, buffer.as_mut_ptr(), buffer.len() as i32) },
            -1
        );

        let payload = b"nobody is listening";
        assert_eq!(
            unsafe { mwt_server_send(1, 0, RELIABLE, payload.as_ptr(), payload.len() as i32) },
            -1
        );
        assert_eq!(
            unsafe { mwt_client_send(0, RELIABLE, payload.as_ptr(), payload.len() as i32) },
            -1
        );

        // Idempotent, and safe to call in any order.
        mwt_server_disconnect(1);
        mwt_client_disconnect();
        mwt_server_stop();
        mwt_client_stop();

        assert_eq!(poll_server(&mut buffer), None);
        assert_eq!(poll_client(&mut buffer), None);
    }

    drain_logs();
}

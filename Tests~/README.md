# Tests

Two suites, both covering the part of this transport that is easiest to get
subtly wrong: turning a byte stream back into discrete Mirror messages.

## Browser plugin

```bash
node Tests~/jslib-framing.test.js
```

Loads `Runtime/Plugins/WebGL/MirrorWTransport.jslib` with the Emscripten globals
stubbed out, then exercises the prologue handshake, frame reassembly across
awkward chunk boundaries, buffer growth and compaction, the event queue, the
heap writes `MirrorWT_Poll` performs, and certificate hash parsing. Needs
Node 18 or newer, and nothing else.

## Native library

```bash
cd Native~/mirror-wtransport && cargo test
```

Three suites:

* **unit tests in `common.rs`** cover the same framing rules as the browser
  suite above, so the two implementations cannot drift apart silently;
* **`tests/loopback.rs`** starts a real server and connects a real client to it
  over loopback QUIC, calling the exact FFI entry points Unity calls, in the
  order Unity calls them. It checks the handshake, ordering of a 50 message
  burst, a message at the size limit, the channel id round trip, datagrams both
  ways, refusal of oversized sends, and that a message queued in the same breath
  as a disconnect still arrives;
* **`tests/failure_paths.rs`** covers what has to happen when things go wrong.
  Mirror's transport contract says a connect attempt "can not end in limbo", so
  each of these has to produce a disconnect rather than hanging: no server
  listening, a mismatched certificate hash, the client hanging up, the server
  shutting down under a live connection, reconnecting afterwards, and calls made
  against a stopped server.

The failure suite deliberately waits out real timeouts, so it takes around 15
seconds.

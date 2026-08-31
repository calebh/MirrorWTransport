# Mirror WebTransport

A [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) transport for
[Mirror](https://mirror-networking.com). WebTransport runs over HTTP/3, which runs over QUIC, so a
browser gets both a reliable ordered channel and an unreliable datagram channel over a single
encrypted UDP connection — the thing WebSockets could never give a WebGL build.

| Role | Implementation |
| --- | --- |
| Browser client (WebGL) | The browser's own `WebTransport` API, through `Runtime/Plugins/WebGL/MirrorWTransport.jslib` |
| Editor / standalone client | Native library built from `Native~/mirror-wtransport` (Rust, [wtransport](https://github.com/BiagioFesta/wtransport)) |
| Server | Same native library |

The native client is not strictly required by the design, but without it you could only ever test the
transport by deploying a WebGL build first. It speaks the identical wire protocol, so a native client
and a browser client can sit in the same game.

## Requirements

* Unity 2021.3 or newer.
* Mirror (developed against 96.0.1).
* A [Rust toolchain](https://rustup.rs) to build the native library. Rust 1.88 or newer.
* A browser with WebTransport: Chrome/Edge 97+, Firefox 114+. Safari does not support it yet.

## Installation

1. Copy this folder into your project, or add it through the Package Manager
   (*Add package from disk…* → `package.json`).
2. Build the native library:

   ```bash
   powershell -ExecutionPolicy Bypass -File Native~/build.ps1
   ```

   ```bash
   ./Native~/build.sh
   ```

   Both drop the result into `Runtime/Plugins/x86_64/`. Unity imports it on the next domain reload;
   check the Plugin Inspector once to confirm the platform and CPU settings look right.
3. Add the **Web Transport Transport** component to your `NetworkManager` object and assign it to the
   `Transport` field.

If the library is missing, the transport inspector says so and `Available()` returns false. WebGL
builds do not need it at all.

## Development: self signed certificates

WebTransport always uses TLS, so there is no plaintext mode to fall back on during development.
The way around it is the same one browsers offer natively: generate a short lived certificate and
pin its hash.

With `Certificate Mode` set to `SelfSigned` the server generates a fresh ECDSA P-256 certificate on
every start and logs its SHA-256 hash:

```
[WebTransport] server listening on port 7777 with a self signed certificate.
serverCertificateHashes value for browser clients:
b1:7c:9f:...:2e
```

Paste that into the **Client Certificate Hash** field on the client's transport (the inspector has a
*Copy into Client Certificate Hash* button while the server is running in the editor). Both the
browser client and the native client accept it.

Two things to keep in mind:

* **The hash changes every time the server restarts.** For a client and server in the same editor
  session that is fine; across machines you have to move the new hash over each time.
* Browsers only accept pinned hashes for certificates that are ECDSA P-256 and valid for at most two
  weeks. The generated certificate satisfies both; a certificate from your own CA usually will not.

For a client running in the editor or a standalone player there is also
`Client Allow Invalid Certificates`, which skips validation entirely. It is a development shortcut
with no browser equivalent — never ship it enabled.

## Production: real certificates

Set `Certificate Mode` to `PemFiles` and point `Certificate Path` and `Key Path` at a real chain,
for example the `fullchain.pem` / `privkey.pem` that Let's Encrypt produces. Leave
`Client Certificate Hash` empty; clients then validate normally.

Two deployment details that are easy to miss:

* WebTransport needs **UDP** open on the server port, not TCP. Most reverse proxies do not forward
  HTTP/3 to a backend, so the transport usually wants its own port.
* The page hosting the WebGL build must be served from a **secure context** (https, or localhost).
  The `WebTransport` constructor does not exist otherwise, and `Available()` returns false.

## Addresses

The transport builds `https://host:port/path` from whatever `ClientConnect` receives, so a
`NetworkManager` address field keeps working unchanged. All of these are accepted:

```
example.com                     -> https://example.com:7777/
example.com:9000                -> https://example.com:9000/
203.0.113.10                    -> https://203.0.113.10:7777/
[2001:db8::1]:9000              -> https://[2001:db8::1]:9000/
https://example.com:9000/game   -> used as given
https://example.com/game        -> https://example.com:7777/game  (no port given)
```

`Path` is sent as the request path. It is only useful when something in front of the server routes
by path; the server itself accepts any path and logs the one it was given.

## Channels

| Delivery | Carried by | Ordered | Reliable | Size limit |
| --- | --- | --- | --- | --- |
| Reliable | one bidirectional QUIC stream per connection | yes | yes | `Reliable Max Message Size` (64 KB by default) |
| Unreliable | QUIC datagrams | no | no | `Unreliable Max Message Size` (1 KB by default) |

`Unreliable Channels` lists the Mirror channel ids that go out as datagrams; everything else uses the
reliable stream. It defaults to `{ 1 }`, which is Mirror's `Channels.Unreliable`. If your project
defines extra channels — this Mirror build also has `MapUnreliable` (2), `DissonanceReliable` (3) and
`DissonanceUnreliable` (4) — add the ones that should be droppable, for example `{ 1, 4 }` to send
voice as datagrams.

The channel id travels on the wire and is handed back to Mirror unchanged, so message handlers see
the channel they were actually sent on.

A note on the unreliable limit: QUIC datagrams cannot be fragmented, so they are bounded by the path
MTU. Anything much above ~1200 bytes will start failing to send, and browsers report a smaller
`maxDatagramSize` than that. 1 KB is a safe default. Oversized messages are dropped with an error in
the log rather than silently.

## How it works

```
Mirror ─ ClientSend / ServerSend
   │
   ├─ reliable channel ──► one bidirectional WebTransport stream per connection
   │                       [length u32 BE][channel u8][payload] …
   │
   └─ unreliable channel ► WebTransport datagrams
                           [channel u8][payload]
```

Using **one** long lived stream for the reliable channel (rather than one stream per message, which
is the obvious thing to do with WebTransport) is what makes the channel ordered. Separate QUIC
streams are each ordered internally but not ordered relative to each other, and Mirror's reliable
channel must be ordered — spawn messages have to arrive before the updates that reference them.

The client opens that stream immediately after the session is established and writes a four byte
prologue; the server replies with the same bytes once it has accepted the stream. Only then does
either side report a connection to Mirror, so by the time `OnClientConnected` fires both delivery
modes are usable on both ends. The prologue also serves a QUIC detail: a stream is not announced to
the peer until something is written on it, so a silent client would otherwise be invisible to the
server.

Nothing calls back into Unity from a background thread. The Rust side pushes events onto a queue and
the transport drains it from `ClientEarlyUpdate` / `ServerEarlyUpdate`; the browser plugin does the
same with a JavaScript array. Payload buffers are recycled through a pool on the Rust side, so a
busy server does not allocate per message.

`Documentation~/Protocol.md` has the full wire format and the FFI contract.

## Settings

| Setting | Meaning |
| --- | --- |
| `Server Port` | UDP port to listen on, and the default port clients connect to |
| `Bind Mode` | Which local addresses the server socket binds to |
| `Certificate Mode` | `SelfSigned` for development, `PemFiles` for a real certificate |
| `Max Connections` | 0 for unlimited; extra sessions are refused during the handshake |
| `Path` | Url path requested by the client and advertised by `ServerUri` |
| `Client Certificate Hash` | SHA-256 of the server certificate, for self signed development servers |
| `Client Allow Invalid Certificates` | Native client only. Skips validation. Development only |
| `Connect Timeout Ms` | How long the client waits for the session and handshake |
| `Unreliable Channels` | Channel ids delivered as datagrams |
| `Reliable Max Message Size` | Largest message accepted on a reliable channel |
| `Unreliable Max Message Size` | Largest message accepted on an unreliable channel |
| `Reliable Batch Threshold` | How much Mirror batches before starting a new reliable message |
| `Keep Alive Interval Ms` | QUIC keep-alive period. 0 disables it |
| `Idle Timeout Ms` | Drop a connection after this long without traffic. 0 means never |
| `Handshake Timeout Ms` | How long the server waits for a client to finish connecting |
| `Max Receives Per Tick` | Caps queued events processed per frame so a flood cannot stall the frame |
| `Debug Log` | Log connection lifecycle details. Warnings and errors are always logged |

## Limitations

* **No host mode over the network.** Mirror's host mode uses a local connection and never touches the
  transport, so that works; but a WebGL build cannot run the server, because browsers can only be
  WebTransport clients. `ServerStart` logs an error there.
* **Safari** has no WebTransport support at the time of writing.
* **Tail data on disconnect.** When either side disconnects, frames already queued locally are
  flushed first, but data still in flight on the network can be lost when the QUIC connection closes.
  This is normal for UDP based transports and matches how Mirror's KCP transport behaves.
* **Certificate hash rotation.** Self signed hashes change on every server start, by design.
* The native library is built for the host architecture by default. Pass `--target` /
  `-Target` to the build script to cross compile for a dedicated server platform.

## Testing

`Samples~/BrowserSmokeTest/index.html` connects to a running server, completes the handshake and
sends one message on each channel. Serve it over https or from localhost and it tells you whether
the server is reachable and whether its certificate is accepted — useful for separating "the server
is broken" from "the browser will not trust this certificate" before a WebGL build is involved.

There are also test suites for both halves. The Rust side goes further than framing: it stands up a
real server, connects a real client to it over loopback QUIC and drives the exact FFI entry points
Unity calls, including the failure paths that must end in a disconnect rather than a hang.

```bash
node Tests~/jslib-framing.test.js
```

```bash
cd Native~/mirror-wtransport && cargo test
```

See `Tests~/README.md`.

## Troubleshooting

**`could not load 'mirror_wtransport'`** — the native library was not built, or landed somewhere
Unity does not scan. Re-run the build script and check `Runtime/Plugins/x86_64/`.

**The browser logs `WebTransport connection rejected` / the connect promise rejects immediately** —
almost always the certificate. Confirm the hash in `Client Certificate Hash` is the one the running
server printed, and that the page is on https or localhost.

**Client connects, then disconnects a few seconds later** — check that `Keep Alive Interval Ms` is
comfortably below `Idle Timeout Ms` on both ends. The effective idle timeout is the smaller of the
two peers' values.

**`refused to send N bytes ... the limit for this channel is M`** — a message exceeded the channel
limit. Raise `Reliable Max Message Size`, or move the message off the unreliable channel: datagrams
cannot be made bigger than the path MTU.

## Licence

MIT, see `LICENSE`.

The native library links [wtransport](https://github.com/BiagioFesta/wtransport) (MIT OR Apache-2.0).

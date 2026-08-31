# Wire protocol and interop contract

Three implementations have to agree on this: the Rust server, the Rust client
(`Native~/mirror-wtransport/src`) and the browser client
(`Runtime/Plugins/WebGL/MirrorWTransport.jslib`). Change one, change all three.

## 1. Session establishment

```
client                                            server
  │  WebTransport CONNECT https://host:port/path    │
  │ ───────────────────────────────────────────────►│  Endpoint::accept
  │                                                 │  SessionRequest::accept
  │  open bidirectional stream, write "MWT1"        │
  │ ───────────────────────────────────────────────►│  Connection::accept_bi
  │                                                 │  read_exact 4 bytes, verify
  │                    "MWT1"                       │
  │ ◄───────────────────────────────────────────────│
  │                                                 │  register connection id
  │  OnClientConnected                              │  OnServerConnectedWithAddress
```

Why the prologue exists at all:

* A QUIC stream is not announced to the peer until something is written on it.
  Without the four bytes, a client that has nothing to send yet would be
  invisible to the server, and the server's `accept_bi` would hit its handshake
  timeout.
* It gives both sides a definite point at which the reliable channel is known to
  work end to end, which is what makes it safe to report the connection to
  Mirror only once.

The server applies `handshake_timeout_ms` to each step (session request, accept,
`accept_bi`, prologue read). A peer that stalls anywhere is dropped rather than
occupying a task forever.

Neither side sends anything else on a connection before its own connect event.
The client starts reading datagrams only after the prologue arrives, so a
datagram cannot be surfaced to Mirror before `OnClientConnected` — streams and
datagrams are not ordered relative to each other.

## 2. Reliable channel

One bidirectional stream per connection, opened by the client, carrying frames:

```
 0        1        2        3        4        5
 +--------+--------+--------+--------+--------+---------------------+
 |         payload length (u32 BE)   | channel|      payload        |
 +--------+--------+--------+--------+--------+---------------------+
```

* `payload length` counts only the payload, not the header.
* `channel` is the Mirror channel id the message was sent on, so the receiver can
  hand Mirror back the exact channel. Ids above 255 are rejected before sending.
* Frames with a zero length payload are skipped by the reader rather than passed
  to Mirror.
* A length above the configured maximum is a protocol violation: the stream can
  no longer be resynchronised, so the receiver raises `InvalidReceive` and closes
  the session with `CLOSE_CODE_PROTOCOL`.

A single stream (rather than one per message) is what makes this channel
*ordered*. Independent QUIC streams are each ordered internally but not ordered
against each other, and Mirror requires its reliable channel to be ordered.

The writer coalesces everything already queued into one `write_all`, capped at
64 KB per write.

## 3. Unreliable channel

One WebTransport datagram per message:

```
 0        1
 +--------+---------------------+
 | channel|       payload       |
 +--------+---------------------+
```

Datagrams are not fragmented, so the payload is bounded by the path MTU.
Oversized datagrams fail to send (native) or are dropped (browser); oversized
incoming datagrams are ignored rather than trusted.

## 4. Shutdown

A local disconnect is queued through the reliable writer as a `Close` marker, so
frames already queued are written before the session closes. Data still in
flight on the network can still be lost when the QUIC connection closes — normal
for a UDP based transport.

Both ends report a disconnect exactly once:

* Rust: the per-connection task owns all its loops in one `tokio::select!`, so
  when any of them ends the rest are dropped with it. The task then closes the
  connection explicitly before awaiting `closed()`, so the await cannot block on
  a session nobody is going to close.
* Browser: `MWT.Close` is idempotent and nulls `MWT.transport`, and the `closed`
  promise handler only fires for the session that is still current.

## 5. FFI contract

Everything is polling based and copy-on-read. No pointer handed to C# outlives
the call that produced it, so C# never frees anything the native side allocated.

```c
uint32_t mwt_abi_version(void);
int32_t  mwt_last_error(uint8_t *buffer, int32_t capacity);
int32_t  mwt_poll_log(int32_t *out_level, uint8_t *buffer, int32_t capacity);

int32_t  mwt_server_start(const MwtServerConfig *config);
void     mwt_server_stop(void);
int32_t  mwt_server_is_active(void);
int32_t  mwt_server_local_port(void);
int32_t  mwt_server_connection_count(void);
int32_t  mwt_server_certificate_hash(uint8_t *buffer, int32_t capacity);
int32_t  mwt_server_client_address(uint32_t id, uint8_t *buffer, int32_t capacity);
int32_t  mwt_server_poll(MwtEvent *out, uint8_t *buffer, int32_t capacity);
int32_t  mwt_server_send(uint32_t id, int32_t channel, int32_t reliable,
                         const uint8_t *data, int32_t length);
void     mwt_server_disconnect(uint32_t id);

int32_t  mwt_client_connect(const MwtClientConfig *config);
void     mwt_client_disconnect(void);
void     mwt_client_stop(void);
int32_t  mwt_client_state(void);
int32_t  mwt_client_poll(MwtEvent *out, uint8_t *buffer, int32_t capacity);
int32_t  mwt_client_send(int32_t channel, int32_t reliable,
                         const uint8_t *data, int32_t length);
```

`*_poll` returns 1 when it wrote an event and 0 when the queue was empty. The
payload, the peer address, the disconnect reason and the error message all come
back through the same `buffer`, with `MwtEvent.data_length` saying how many bytes
were written. Strings are UTF-8 and are truncated on a character boundary rather
than mid-sequence.

`mwt_poll_log` drains the log lines that background threads produced, because
those threads cannot call into Unity.

Every exported function wraps its body in `catch_unwind`: a Rust panic unwinding
into the CLR would take the editor down with it. `panic = "abort"` must therefore
stay out of the release profile.

Structs put pointers first so their layout has no padding on either 32 or 64 bit
targets, and matches the `[StructLayout(LayoutKind.Sequential)]` structs in
`Runtime/Common/WTTypes.cs`.

`ABI_VERSION` guards the whole thing: C# refuses to use a library that reports a
different number. Bump it whenever anything above changes.

## 6. Browser plugin contract

```js
MirrorWT_IsSupported()                        -> 0 | 1
MirrorWT_State()                              -> 0 disconnected | 1 connecting | 2 connected
MirrorWT_MaxDatagramSize()                    -> bytes, or 0
MirrorWT_Connect(url, hash, maxRel, maxUnrel)
MirrorWT_Disconnect()
MirrorWT_Send(dataPtr, offset, length, channel, reliable) -> 0 | 1
MirrorWT_Poll(headerPtr, bufferPtr, capacity)             -> 0 | 1
```

`MirrorWT_Poll` fills four `int32` at `headerPtr`: kind, channel, length, code —
the same values `MwtEvent` carries — and writes the payload at `bufferPtr`.

Reliable writes are issued without awaiting each one. That is safe and keeps the
channel ordered: a `WritableStream` processes writes in call order. It does mean
backpressure is ignored, which is the usual trade for a game transport.

The plugin is written in ES5 style (`var`, `function`, promise chains) because
Emscripten embeds it verbatim and it may be run through a minifier targeting ES5.

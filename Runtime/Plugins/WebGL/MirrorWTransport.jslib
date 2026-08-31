// Browser half of the Mirror WebTransport transport.
//
// Speaks the same wire protocol as the native library in
// Native~/mirror-wtransport (see common.rs):
//
//   reliable stream : "MWT1" prologue, then [length u32 BE][channel u8][payload]
//   datagrams       : [channel u8][payload]
//
// Everything is queued and handed to C# from MirrorWT_Poll rather than pushed
// through Emscripten function pointers, so messages arrive during Mirror's
// ClientEarlyUpdate instead of at arbitrary points inside a frame.
//
// Written in ES5 style (var, function, promise chains) because Emscripten
// embeds this verbatim and it may be run through a minifier targeting ES5.

var MirrorWTransportLibrary = {

    $MWT: {
        // ---- protocol constants (keep in sync with common.rs) ----
        PROLOGUE: [77, 87, 84, 49], // "MWT1"
        FRAME_HEADER: 5,

        STATE_DISCONNECTED: 0,
        STATE_CONNECTING: 1,
        STATE_CONNECTED: 2,

        KIND_CONNECTED: 1,
        KIND_DATA: 2,
        KIND_DISCONNECTED: 3,
        KIND_ERROR: 4,

        ERROR_NONE: 0,
        ERROR_TIMEOUT: 3,
        ERROR_INVALID_RECEIVE: 5,
        ERROR_CONNECTION_CLOSED: 7,
        ERROR_UNEXPECTED: 8,

        // ---- session state ----
        transport: null,
        state: 0,
        url: "",
        datagramWriter: null,
        reliableWriter: null,
        prologueDone: false,
        maxReliablePayload: 65536,
        maxUnreliablePayload: 1024,
        events: [],

        // Incoming reliable bytes, reassembled into frames.
        buffer: null,
        bufferStart: 0,
        bufferEnd: 0,

        // -----------------------------------------------------------------
        // helpers
        // -----------------------------------------------------------------

        Reset: function () {
            MWT.transport = null;
            MWT.state = MWT.STATE_DISCONNECTED;
            MWT.datagramWriter = null;
            MWT.reliableWriter = null;
            MWT.prologueDone = false;
            MWT.buffer = new Uint8Array(16384);
            MWT.bufferStart = 0;
            MWT.bufferEnd = 0;
        },

        Encode: function (text) {
            return new TextEncoder().encode(text);
        },

        Push: function (event) {
            MWT.events.push(event);
        },

        PushError: function (code, message) {
            console.error("[MirrorWTransport] " + message);
            MWT.Push({ kind: MWT.KIND_ERROR, channel: 0, code: code, data: MWT.Encode(message) });
        },

        // Ends the session and tells C# about it exactly once.
        Close: function (code, reason) {
            if (MWT.state === MWT.STATE_DISCONNECTED) return;

            MWT.state = MWT.STATE_DISCONNECTED;
            MWT.datagramWriter = null;
            MWT.reliableWriter = null;

            var transport = MWT.transport;
            MWT.transport = null;
            if (transport) {
                try { transport.close(); } catch (e) { /* already closing */ }
            }

            MWT.Push({ kind: MWT.KIND_DISCONNECTED, channel: 0, code: code, data: MWT.Encode(reason) });
        },

        Fail: function (code, message) {
            MWT.PushError(code, message);
            MWT.Close(code, message);
        },

        // Accepts the dotted hex the server prints ("aa:bb:.."), plain hex, and
        // the byte array form ("[170, 187, ...]").
        ParseCertificateHash: function (text) {
            if (!text) return null;
            text = text.replace(/^\s+|\s+$/g, "");
            if (text.length === 0) return null;

            try {
                if (text.charAt(0) === "[") {
                    var parts = text.substring(1, text.length - 1).split(",");
                    var values = new Uint8Array(parts.length);
                    for (var p = 0; p < parts.length; ++p) {
                        var value = parseInt(parts[p], 10);
                        if (isNaN(value) || value < 0 || value > 255) return null;
                        values[p] = value;
                    }
                    return values.length === 32 ? values : null;
                }

                var hex = text.replace(/[^0-9a-fA-F]/g, "");
                if (hex.length !== 64) return null;

                var bytes = new Uint8Array(32);
                for (var i = 0; i < 32; ++i) {
                    bytes[i] = parseInt(hex.substr(i * 2, 2), 16);
                }
                return bytes;
            } catch (e) {
                return null;
            }
        },

        // -----------------------------------------------------------------
        // reliable stream
        // -----------------------------------------------------------------

        Append: function (chunk) {
            var pending = MWT.bufferEnd - MWT.bufferStart;
            var needed = pending + chunk.length;

            if (needed > MWT.buffer.length) {
                var size = MWT.buffer.length;
                while (size < needed) size *= 2;
                var grown = new Uint8Array(size);
                grown.set(MWT.buffer.subarray(MWT.bufferStart, MWT.bufferEnd), 0);
                MWT.buffer = grown;
                MWT.bufferStart = 0;
                MWT.bufferEnd = pending;
            } else if (MWT.bufferEnd + chunk.length > MWT.buffer.length) {
                MWT.buffer.copyWithin(0, MWT.bufferStart, MWT.bufferEnd);
                MWT.bufferStart = 0;
                MWT.bufferEnd = pending;
            }

            MWT.buffer.set(chunk, MWT.bufferEnd);
            MWT.bufferEnd += chunk.length;
        },

        Parse: function () {
            if (!MWT.prologueDone) {
                if (MWT.bufferEnd - MWT.bufferStart < MWT.PROLOGUE.length) return;

                for (var i = 0; i < MWT.PROLOGUE.length; ++i) {
                    if (MWT.buffer[MWT.bufferStart + i] !== MWT.PROLOGUE[i]) {
                        MWT.Fail(MWT.ERROR_INVALID_RECEIVE, "the server is not speaking the MirrorWTransport protocol");
                        return;
                    }
                }

                MWT.bufferStart += MWT.PROLOGUE.length;
                MWT.prologueDone = true;
                MWT.OnHandshakeComplete();
                if (MWT.state !== MWT.STATE_CONNECTED) return;
            }

            while (true) {
                var available = MWT.bufferEnd - MWT.bufferStart;
                if (available < MWT.FRAME_HEADER) break;

                var at = MWT.bufferStart;
                var length = ((MWT.buffer[at] << 24) |
                              (MWT.buffer[at + 1] << 16) |
                              (MWT.buffer[at + 2] << 8) |
                              MWT.buffer[at + 3]) >>> 0;
                var channel = MWT.buffer[at + 4];

                if (length > MWT.maxReliablePayload) {
                    MWT.Fail(MWT.ERROR_INVALID_RECEIVE,
                        "the server announced a " + length + " byte message, the limit is " + MWT.maxReliablePayload);
                    return;
                }

                if (available < MWT.FRAME_HEADER + length) break;

                var from = at + MWT.FRAME_HEADER;
                MWT.bufferStart = from + length;

                // Empty frames carry nothing Mirror can parse.
                if (length > 0) {
                    MWT.Push({
                        kind: MWT.KIND_DATA,
                        channel: channel,
                        code: 0,
                        data: MWT.buffer.slice(from, from + length)
                    });
                }
            }

            // Reclaim the consumed prefix once it is worth the memmove.
            if (MWT.bufferStart === MWT.bufferEnd) {
                MWT.bufferStart = 0;
                MWT.bufferEnd = 0;
            } else if (MWT.bufferStart >= 65536) {
                MWT.buffer.copyWithin(0, MWT.bufferStart, MWT.bufferEnd);
                MWT.bufferEnd -= MWT.bufferStart;
                MWT.bufferStart = 0;
            }
        },

        ReadReliable: function (reader) {
            function step() {
                return reader.read().then(function (result) {
                    if (result.done) {
                        MWT.Close(MWT.ERROR_CONNECTION_CLOSED, "the server closed the reliable stream");
                        return;
                    }
                    if (MWT.state === MWT.STATE_DISCONNECTED) return;

                    MWT.Append(result.value);
                    MWT.Parse();

                    if (MWT.state === MWT.STATE_DISCONNECTED) return;
                    return step();
                });
            }

            step().catch(function (e) {
                MWT.Close(MWT.ERROR_CONNECTION_CLOSED, "reliable stream ended: " + e);
            });
        },

        ReadDatagrams: function (reader) {
            function step() {
                return reader.read().then(function (result) {
                    if (result.done) return;
                    if (MWT.state === MWT.STATE_DISCONNECTED) return;

                    var value = result.value;
                    if (value && value.length > 0) {
                        var length = value.length - 1;
                        if (length <= MWT.maxUnreliablePayload) {
                            MWT.Push({
                                kind: MWT.KIND_DATA,
                                channel: value[0],
                                code: 0,
                                data: value.slice(1)
                            });
                        }
                    }

                    return step();
                });
            }

            step().catch(function () {
                // The datagram reader always errors out when the session ends;
                // the closed promise reports the actual reason.
            });
        },

        // -----------------------------------------------------------------
        // connection lifecycle
        // -----------------------------------------------------------------

        OnReady: function () {
            if (!MWT.transport || MWT.state !== MWT.STATE_CONNECTING) return;

            var transport = MWT.transport;

            try {
                MWT.datagramWriter = transport.datagrams.writable.getWriter();
            } catch (e) {
                MWT.Fail(MWT.ERROR_UNEXPECTED, "could not open the datagram writer: " + e);
                return;
            }

            transport.createBidirectionalStream().then(function (stream) {
                if (MWT.transport !== transport) return;

                MWT.reliableWriter = stream.writable.getWriter();

                // The stream only reaches the server once something is written
                // on it, so the prologue doubles as the "here I am" signal.
                return MWT.reliableWriter.write(new Uint8Array(MWT.PROLOGUE)).then(function () {
                    if (MWT.transport !== transport) return;
                    MWT.ReadReliable(stream.readable.getReader());
                });
            }).catch(function (e) {
                MWT.Fail(MWT.ERROR_CONNECTION_CLOSED, "could not open the reliable stream: " + e);
            });
        },

        OnHandshakeComplete: function () {
            if (MWT.state !== MWT.STATE_CONNECTING) return;

            MWT.state = MWT.STATE_CONNECTED;
            MWT.Push({ kind: MWT.KIND_CONNECTED, channel: 0, code: 0, data: MWT.Encode(MWT.url) });

            // Started only now, so a datagram cannot be surfaced before the
            // connect event that Mirror needs to see first.
            try {
                MWT.ReadDatagrams(MWT.transport.datagrams.readable.getReader());
            } catch (e) {
                console.warn("[MirrorWTransport] datagrams are unavailable: " + e);
            }
        }
    },

    // ---------------------------------------------------------------------
    // exported to C#
    // ---------------------------------------------------------------------

    MirrorWT_IsSupported: function () {
        return (typeof WebTransport !== "undefined") ? 1 : 0;
    },

    MirrorWT_State: function () {
        return MWT.state;
    },

    MirrorWT_MaxDatagramSize: function () {
        if (!MWT.transport || !MWT.transport.datagrams) return 0;
        var size = MWT.transport.datagrams.maxDatagramSize;
        return size ? (size | 0) : 0;
    },

    MirrorWT_Connect: function (urlPointer, hashPointer, maxReliablePayload, maxUnreliablePayload) {
        if (typeof WebTransport === "undefined") {
            MWT.events.push({
                kind: MWT.KIND_ERROR,
                channel: 0,
                code: MWT.ERROR_UNEXPECTED,
                data: MWT.Encode("this browser does not support WebTransport")
            });
            MWT.events.push({
                kind: MWT.KIND_DISCONNECTED,
                channel: 0,
                code: MWT.ERROR_UNEXPECTED,
                data: MWT.Encode("this browser does not support WebTransport")
            });
            return;
        }

        MWT.Reset();

        var url = UTF8ToString(urlPointer);
        var hashText = hashPointer ? UTF8ToString(hashPointer) : "";

        MWT.url = url;
        MWT.maxReliablePayload = maxReliablePayload;
        MWT.maxUnreliablePayload = maxUnreliablePayload;

        var options = {};
        if (hashText && hashText.replace(/^\s+|\s+$/g, "").length > 0) {
            var hash = MWT.ParseCertificateHash(hashText);
            if (!hash) {
                MWT.state = MWT.STATE_CONNECTING;
                MWT.Fail(MWT.ERROR_UNEXPECTED, "'" + hashText + "' is not a valid SHA-256 certificate hash");
                return;
            }
            options.serverCertificateHashes = [{ algorithm: "sha-256", value: hash }];
        }

        var transport;
        try {
            transport = new WebTransport(url, options);
        } catch (e) {
            MWT.state = MWT.STATE_CONNECTING;
            MWT.Fail(MWT.ERROR_UNEXPECTED, "could not create the WebTransport session: " + e);
            return;
        }

        MWT.transport = transport;
        MWT.state = MWT.STATE_CONNECTING;

        // Only the session that is still current may report a disconnect: by
        // the time this settles the user may already have reconnected, and
        // MWT.Close nulls the field so a local close does not report twice.
        transport.closed.then(function () {
            if (MWT.transport !== transport) return;
            MWT.Close(MWT.ERROR_CONNECTION_CLOSED, "the session was closed");
        }).catch(function (e) {
            if (MWT.transport !== transport) return;
            MWT.Close(MWT.ERROR_CONNECTION_CLOSED, "the session was closed: " + e);
        });

        transport.ready.then(function () {
            if (MWT.transport !== transport) return;
            MWT.OnReady();
        }).catch(function (e) {
            if (MWT.transport !== transport) return;
            MWT.Fail(MWT.ERROR_CONNECTION_CLOSED, "could not connect to " + url + ": " + e);
        });
    },

    MirrorWT_Disconnect: function () {
        if (MWT.state === MWT.STATE_DISCONNECTED) return;

        if (MWT.transport) {
            // Let the closed promise raise the disconnect, so a local close and
            // a remote one take the same path.
            try {
                MWT.transport.close();
                return;
            } catch (e) {
                /* fall through */
            }
        }

        MWT.Close(MWT.ERROR_CONNECTION_CLOSED, "disconnected locally");
    },

    MirrorWT_Send: function (dataPointer, offset, length, channel, reliable) {
        if (MWT.state !== MWT.STATE_CONNECTED) return 0;

        var from = dataPointer + offset;
        var payload = HEAPU8.subarray(from, from + length);

        if (reliable) {
            if (!MWT.reliableWriter) return 0;

            var frame = new Uint8Array(MWT.FRAME_HEADER + length);
            frame[0] = (length >>> 24) & 255;
            frame[1] = (length >>> 16) & 255;
            frame[2] = (length >>> 8) & 255;
            frame[3] = length & 255;
            frame[4] = channel & 255;
            frame.set(payload, MWT.FRAME_HEADER);

            // Writes on a WritableStream are queued in call order, so this keeps
            // the channel ordered without awaiting each one.
            MWT.reliableWriter.write(frame).catch(function (e) {
                MWT.Close(MWT.ERROR_CONNECTION_CLOSED, "reliable send failed: " + e);
            });
            return 1;
        }

        if (!MWT.datagramWriter) return 0;

        var datagram = new Uint8Array(1 + length);
        datagram[0] = channel & 255;
        datagram.set(payload, 1);

        // Datagrams are droppable by definition, so a failed write is not fatal.
        MWT.datagramWriter.write(datagram).catch(function () {});
        return 1;
    },

    MirrorWT_Poll: function (headerPointer, bufferPointer, capacity) {
        if (MWT.events.length === 0) return 0;

        var event = MWT.events.shift();
        var kind = event.kind;
        var code = event.code | 0;
        var channel = event.channel | 0;
        var data = event.data;

        if (data && data.length > capacity) {
            // The transport sizes its buffer from the configured maximums, so
            // this means the two disagree. Report it instead of truncating.
            kind = MWT.KIND_ERROR;
            code = MWT.ERROR_INVALID_RECEIVE;
            channel = 0;
            data = MWT.Encode("dropped a " + data.length + " byte message: the receive buffer only holds " + capacity + " bytes");
            if (data.length > capacity) data = data.subarray(0, capacity);
        }

        var length = data ? data.length : 0;
        if (length > 0) HEAPU8.set(data, bufferPointer);

        var header = headerPointer >> 2;
        HEAP32[header] = kind;
        HEAP32[header + 1] = channel;
        HEAP32[header + 2] = length;
        HEAP32[header + 3] = code;

        return 1;
    }
};

autoAddDeps(MirrorWTransportLibrary, "$MWT");
mergeInto(LibraryManager.library, MirrorWTransportLibrary);

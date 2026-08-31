// Exercises the framing / event-queue logic of MirrorWTransport.jslib outside
// Unity, by stubbing the Emscripten globals it relies on.
const fs = require("fs");
const vm = require("vm");
const path = require("path");

const jslib = process.argv[2] ||
    path.join(__dirname, "..", "Runtime", "Plugins", "WebGL", "MirrorWTransport.jslib");

const source = fs.readFileSync(jslib, "utf8");

const heap = new ArrayBuffer(1 << 20);
const HEAPU8 = new Uint8Array(heap);
const HEAP32 = new Int32Array(heap);

const sandbox = {
    console,
    TextEncoder,
    TextDecoder,
    Uint8Array,
    HEAPU8,
    HEAP32,
    autoAddDeps() {},
    LibraryManager: { library: {} },
    mergeInto(target, source) { Object.assign(target, source); },
    UTF8ToString(pointer) {
        let end = pointer;
        while (HEAPU8[end] !== 0) ++end;
        return new TextDecoder().decode(HEAPU8.subarray(pointer, end));
    },
    WebTransport: function () { throw new Error("not used in this test"); }
};
vm.createContext(sandbox);
vm.runInContext(source, sandbox, { filename: path.basename(jslib) });

const lib = sandbox.LibraryManager.library;
// Emscripten emits the $-prefixed object as a plain global of that name.
sandbox.MWT = lib.$MWT;
const MWT = sandbox.MWT;

let failures = 0;
function check(name, condition, detail) {
    if (condition) {
        console.log("  ok    " + name);
    } else {
        failures++;
        console.log("  FAIL  " + name + (detail ? "  (" + detail + ")" : ""));
    }
}

function frame(channel, payload) {
    const out = new Uint8Array(5 + payload.length);
    out[0] = (payload.length >>> 24) & 255;
    out[1] = (payload.length >>> 16) & 255;
    out[2] = (payload.length >>> 8) & 255;
    out[3] = payload.length & 255;
    out[4] = channel;
    out.set(payload, 5);
    return out;
}

function concat(chunks) {
    const total = chunks.reduce((sum, c) => sum + c.length, 0);
    const out = new Uint8Array(total);
    let at = 0;
    for (const c of chunks) { out.set(c, at); at += c.length; }
    return out;
}

function freshSession() {
    MWT.Reset();
    MWT.events.length = 0;
    MWT.url = "https://localhost:7777/";
    MWT.maxReliablePayload = 65536;
    MWT.maxUnreliablePayload = 1024;
    MWT.state = MWT.STATE_CONNECTING;
    // OnHandshakeComplete starts the datagram reader; give it something inert.
    MWT.transport = {
        datagrams: { readable: { getReader: () => ({ read: () => new Promise(() => {}) }) } },
        close() {}
    };
}

const encode = t => new TextEncoder().encode(t);
const decode = b => new TextDecoder().decode(b);

// ---------------------------------------------------------------------------
console.log("prologue + framing, delivered in awkward chunks");
{
    freshSession();

    const stream = concat([
        new Uint8Array(MWT.PROLOGUE),
        frame(0, encode("first")),
        frame(3, encode("second message")),
        frame(1, encode("x"))
    ]);

    // One byte at a time is the worst case the reassembler has to survive.
    for (let i = 0; i < stream.length; ++i) {
        MWT.Append(stream.subarray(i, i + 1));
        MWT.Parse();
    }

    const events = MWT.events;
    check("connected event comes first", events[0] && events[0].kind === MWT.KIND_CONNECTED);
    check("connect event carries the url", events[0] && decode(events[0].data) === "https://localhost:7777/");
    check("three data events", events.length === 4, "got " + events.length);
    check("payload 1", decode(events[1].data) === "first" && events[1].channel === 0);
    check("payload 2", decode(events[2].data) === "second message" && events[2].channel === 3);
    check("payload 3", decode(events[3].data) === "x" && events[3].channel === 1);
    check("state is connected", MWT.state === MWT.STATE_CONNECTED);
}

// ---------------------------------------------------------------------------
console.log("everything in one chunk");
{
    freshSession();
    MWT.Append(concat([
        new Uint8Array(MWT.PROLOGUE),
        frame(2, encode("a")),
        frame(2, encode("bb"))
    ]));
    MWT.Parse();
    check("two data events", MWT.events.length === 3, "got " + MWT.events.length);
    check("payloads intact", decode(MWT.events[1].data) === "a" && decode(MWT.events[2].data) === "bb");
}

// ---------------------------------------------------------------------------
console.log("zero length frames are skipped, not forwarded");
{
    freshSession();
    MWT.Append(concat([new Uint8Array(MWT.PROLOGUE), frame(0, new Uint8Array(0)), frame(0, encode("real"))]));
    MWT.Parse();
    check("only the real message surfaces", MWT.events.length === 2 && decode(MWT.events[1].data) === "real");
}

// ---------------------------------------------------------------------------
console.log("a bad prologue fails the session");
{
    freshSession();
    MWT.Append(encode("NOPE"));
    MWT.Parse();
    check("error then disconnect", MWT.events.length === 2 &&
        MWT.events[0].kind === MWT.KIND_ERROR &&
        MWT.events[1].kind === MWT.KIND_DISCONNECTED);
    check("session torn down", MWT.state === MWT.STATE_DISCONNECTED);
}

// ---------------------------------------------------------------------------
console.log("an oversized announced length fails the session");
{
    freshSession();
    MWT.maxReliablePayload = 64;
    MWT.Append(concat([new Uint8Array(MWT.PROLOGUE), frame(0, new Uint8Array(65))]));
    MWT.Parse();
    const kinds = MWT.events.map(e => e.kind);
    check("connected, then error, then disconnect",
        kinds.join(",") === [MWT.KIND_CONNECTED, MWT.KIND_ERROR, MWT.KIND_DISCONNECTED].join(","),
        kinds.join(","));
}

// ---------------------------------------------------------------------------
console.log("a 3 byte tail is held until the rest arrives");
{
    freshSession();
    MWT.Append(concat([new Uint8Array(MWT.PROLOGUE), frame(0, encode("hello")).subarray(0, 7)]));
    MWT.Parse();
    check("nothing but the connect event yet", MWT.events.length === 1);
    MWT.Append(frame(0, encode("hello")).subarray(7));
    MWT.Parse();
    check("message completes", MWT.events.length === 2 && decode(MWT.events[1].data) === "hello");
}

// ---------------------------------------------------------------------------
console.log("buffer growth and compaction");
{
    freshSession();
    MWT.Append(new Uint8Array(MWT.PROLOGUE));
    MWT.Parse();

    const big = new Uint8Array(40000).fill(7);
    MWT.Append(frame(0, big));
    MWT.Parse();
    check("large message survives growth",
        MWT.events.length === 2 && MWT.events[1].data.length === 40000 && MWT.events[1].data[39999] === 7);

    // 200 more messages, to drive the compaction path.
    MWT.events.length = 0;
    for (let i = 0; i < 200; ++i) MWT.Append(frame(1, encode("m" + i)));
    MWT.Parse();
    check("all 200 arrive in order",
        MWT.events.length === 200 && decode(MWT.events[199].data) === "m199");
}

// ---------------------------------------------------------------------------
console.log("Poll writes the header and payload into the heap");
{
    freshSession();
    MWT.Append(concat([new Uint8Array(MWT.PROLOGUE), frame(4, encode("payload"))]));
    MWT.Parse();

    const headerPointer = 64;   // 4 byte aligned
    const bufferPointer = 4096;

    check("connect event polled", lib.MirrorWT_Poll(headerPointer, bufferPointer, 1024) === 1);
    check("kind is connected", HEAP32[headerPointer >> 2] === MWT.KIND_CONNECTED);

    check("data event polled", lib.MirrorWT_Poll(headerPointer, bufferPointer, 1024) === 1);
    const kind = HEAP32[headerPointer >> 2];
    const channel = HEAP32[(headerPointer >> 2) + 1];
    const length = HEAP32[(headerPointer >> 2) + 2];
    check("kind is data", kind === MWT.KIND_DATA);
    check("channel round trips", channel === 4, "got " + channel);
    check("length is right", length === 7, "got " + length);
    check("bytes landed in the heap", decode(HEAPU8.subarray(bufferPointer, bufferPointer + length)) === "payload");

    check("queue is empty afterwards", lib.MirrorWT_Poll(headerPointer, bufferPointer, 1024) === 0);
}

// ---------------------------------------------------------------------------
console.log("Poll turns an over-capacity message into an error");
{
    freshSession();
    MWT.state = MWT.STATE_CONNECTED;
    MWT.events.length = 0;
    MWT.Push({ kind: MWT.KIND_DATA, channel: 0, code: 0, data: new Uint8Array(500) });

    const headerPointer = 64;
    const bufferPointer = 4096;
    lib.MirrorWT_Poll(headerPointer, bufferPointer, 100);
    check("reported as an error", HEAP32[headerPointer >> 2] === MWT.KIND_ERROR);
    check("error code is InvalidReceive", HEAP32[(headerPointer >> 2) + 3] === MWT.ERROR_INVALID_RECEIVE);
    check("message fits the buffer", HEAP32[(headerPointer >> 2) + 2] <= 100);
}

// ---------------------------------------------------------------------------
console.log("Send builds the wire format");
{
    freshSession();
    MWT.state = MWT.STATE_CONNECTED;

    const written = [];
    MWT.reliableWriter = { write: chunk => { written.push(chunk); return Promise.resolve(); } };
    MWT.datagramWriter = { write: chunk => { written.push(chunk); return Promise.resolve(); } };

    const payload = encode("hello wire");
    const dataPointer = 8192;
    HEAPU8.set(payload, dataPointer + 3);   // non zero offset, like an ArraySegment

    check("reliable send accepted", lib.MirrorWT_Send(dataPointer, 3, payload.length, 3, 1) === 1);
    const reliable = written[0];
    check("reliable header", reliable.length === 5 + payload.length &&
        reliable[3] === payload.length && reliable[4] === 3);
    check("reliable payload", decode(reliable.subarray(5)) === "hello wire");

    check("unreliable send accepted", lib.MirrorWT_Send(dataPointer, 3, payload.length, 1, 0) === 1);
    const datagram = written[1];
    check("datagram header", datagram.length === 1 + payload.length && datagram[0] === 1);
    check("datagram payload", decode(datagram.subarray(1)) === "hello wire");
}

// ---------------------------------------------------------------------------
console.log("certificate hash parsing");
{
    const dotted = Array.from({ length: 32 }, (_, i) => (i + 1).toString(16).padStart(2, "0")).join(":");
    const parsedDotted = MWT.ParseCertificateHash(dotted);
    check("dotted hex", parsedDotted && parsedDotted.length === 32 && parsedDotted[0] === 1 && parsedDotted[31] === 32);

    const bare = dotted.replace(/:/g, "");
    check("bare hex", (MWT.ParseCertificateHash(bare) || []).length === 32);

    const array = "[" + Array.from({ length: 32 }, (_, i) => i + 1).join(", ") + "]";
    const parsedArray = MWT.ParseCertificateHash(array);
    check("byte array form", parsedArray && parsedArray.length === 32 && parsedArray[31] === 32);

    check("garbage rejected", MWT.ParseCertificateHash("not a hash") === null);
    check("wrong length rejected", MWT.ParseCertificateHash("aa:bb:cc") === null);
    check("empty rejected", MWT.ParseCertificateHash("") === null);
}

console.log("");
console.log(failures === 0 ? "all checks passed" : failures + " CHECK(S) FAILED");
process.exit(failures === 0 ? 0 : 1);

// Exercises the session lifecycle of MirrorWTransport.jslib: reconnects,
// stale callbacks from earlier sessions, and the receive / send caps. Uses a
// scripted fake WebTransport so each promise can be settled by hand, in the
// awkward orders a slow mobile browser produces.
const fs = require("fs");
const vm = require("vm");
const path = require("path");

const jslib = process.argv[2] ||
    path.join(__dirname, "..", "Runtime", "Plugins", "WebGL", "MirrorWTransport.jslib");

const source = fs.readFileSync(jslib, "utf8");

const heap = new ArrayBuffer(1 << 20);
const HEAPU8 = new Uint8Array(heap);
const HEAP32 = new Int32Array(heap);

// ---------------------------------------------------------------------------
// fake WebTransport
// ---------------------------------------------------------------------------

function deferred() {
    let resolve, reject;
    const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
    promise.catch(() => {});
    return { promise, resolve, reject };
}

// A reader whose reads are settled by the test.
function scriptedReader() {
    const pending = [];
    return {
        pending,
        read() { const d = deferred(); pending.push(d); return d.promise; },
        next() { return pending.shift(); }
    };
}

// A writer whose writes are either settled immediately or held by the test.
function scriptedWriter(hold) {
    const writes = [];
    return {
        writes,
        write(chunk) {
            const d = deferred();
            writes.push({ chunk, d });
            if (!hold) d.resolve();
            return d.promise;
        }
    };
}

const sessions = [];

function FakeWebTransport(url, options) {
    this.url = url;
    this.options = options;
    this.closeCalls = 0;
    this._ready = deferred();
    this._closed = deferred();
    this.ready = this._ready.promise;
    this.closed = this._closed.promise;
    this.reliableReader = scriptedReader();
    this.reliableWriter = scriptedWriter(false);
    this.datagramReader = scriptedReader();
    this.datagramWriter = scriptedWriter(false);
    const self = this;
    this.datagrams = {
        maxDatagramSize: 1200,
        readable: { getReader: () => self.datagramReader },
        writable: { getWriter: () => self.datagramWriter }
    };
    sessions.push(this);
}
FakeWebTransport.prototype.close = function () { this.closeCalls++; };
FakeWebTransport.prototype.createBidirectionalStream = function () {
    const self = this;
    return Promise.resolve({
        readable: { getReader: () => self.reliableReader },
        writable: { getWriter: () => self.reliableWriter }
    });
};

// ---------------------------------------------------------------------------
// load the library
// ---------------------------------------------------------------------------

const sandbox = {
    console: { log: console.log, warn() {}, error() {} },  // expected failures are noisy
    TextEncoder,
    TextDecoder,
    Uint8Array,
    Math,
    HEAPU8,
    HEAP32,
    autoAddDeps() {},
    LibraryManager: { library: {} },
    mergeInto(target, src) { Object.assign(target, src); },
    UTF8ToString(pointer) {
        let end = pointer;
        while (HEAPU8[end] !== 0) ++end;
        return new TextDecoder().decode(HEAPU8.subarray(pointer, end));
    },
    WebTransport: FakeWebTransport
};
vm.createContext(sandbox);
vm.runInContext(source, sandbox, { filename: path.basename(jslib) });

const lib = sandbox.LibraryManager.library;
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

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

const URL_POINTER = 1024;
const HASH_POINTER = 2048;
const HEADER_POINTER = 4096;
const BUFFER_POINTER = 8192;
const CAPACITY = 128 * 1024;

function writeString(pointer, text) {
    const bytes = new TextEncoder().encode(text);
    HEAPU8.set(bytes, pointer);
    HEAPU8[pointer + bytes.length] = 0;
}
writeString(URL_POINTER, "https://localhost:7777/");
writeString(HASH_POINTER, "");

const flush = () => new Promise(resolve => setImmediate(resolve));

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

const PROLOGUE = new Uint8Array([77, 87, 84, 49]);

function connect() {
    lib.MirrorWT_Connect(URL_POINTER, HASH_POINTER, 65536 + 64, 1024 + 64);
    return sessions[sessions.length - 1];
}

// Drives a fresh session all the way to "connected".
async function connectFully() {
    const session = connect();
    session._ready.resolve();
    await flush();
    session.reliableReader.next().resolve({ done: false, value: PROLOGUE });
    await flush();
    return session;
}

// Polls every queued event the way the C# client does.
function drain() {
    const kinds = [];
    while (lib.MirrorWT_Poll(HEADER_POINTER, BUFFER_POINTER, CAPACITY)) {
        kinds.push(HEAP32[HEADER_POINTER >> 2]);
    }
    return kinds;
}

const CONNECTED = 1, DATA = 2, DISCONNECTED = 3, ERROR = 4;

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

(async () => {

console.log("a leftover disconnect event does not end the next session");
{
    await connectFully();
    drain();
    lib.MirrorWT_Disconnect();
    // C# Shutdown() flips its flags and stops polling here, leaving the
    // DISCONNECTED event in the queue.
    check("disconnect was queued", MWT.events.length === 1 && MWT.events[0].kind === DISCONNECTED);

    connect();
    check("reconnect starts with an empty queue", MWT.events.length === 0);
    check("queued byte count reset", MWT.queuedBytes === 0);
}

console.log("Disconnect reports immediately, even if `closed` never settles");
{
    const session = await connectFully();
    drain();
    lib.MirrorWT_Disconnect();
    check("disconnect event is queued at once", drain().join() === String(DISCONNECTED));
    check("state is disconnected", lib.MirrorWT_State() === 0);
    check("session was asked to close", session.closeCalls === 1);

    session._closed.resolve();
    await flush();
    check("late `closed` adds nothing", MWT.events.length === 0);
}

console.log("disconnecting while still connecting");
{
    connect();
    lib.MirrorWT_Disconnect();
    check("disconnect event is queued at once", drain().join() === String(DISCONNECTED));
}

console.log("late callbacks from an old session cannot touch the new one");
{
    const old = await connectFully();
    drain();
    const oldRead = old.reliableReader.next();       // still outstanding
    const oldDatagramRead = old.datagramReader.next();
    lib.MirrorWT_Disconnect();
    drain();

    const current = await connectFully();
    check("new session connected", drain().join() === String(CONNECTED));

    oldRead.reject(new Error("stream reset"));
    await flush();
    check("old reader's failure does not close the new session", lib.MirrorWT_State() === 2);

    oldDatagramRead.resolve({ done: false, value: new Uint8Array([1, 9, 9, 9]) });
    await flush();
    check("old session's datagram is not delivered", MWT.events.length === 0);

    old._closed.reject(new Error("connection lost"));
    await flush();
    check("old `closed` does not close the new session", lib.MirrorWT_State() === 2);
    check("nothing queued by the old session", MWT.events.length === 0);

    current.reliableReader.next().resolve({ done: false, value: frame(0, new Uint8Array([42])) });
    await flush();
    check("new session still receives", drain().join() === String(DATA));
}

console.log("late reliable data from an old session is not parsed into the new one");
{
    const old = await connectFully();
    drain();
    const oldRead = old.reliableReader.next();
    lib.MirrorWT_Disconnect();
    drain();

    await connectFully();
    drain();
    oldRead.resolve({ done: false, value: frame(0, new Uint8Array([1, 2, 3])) });
    await flush();
    check("stale frame dropped", MWT.events.length === 0 && MWT.bufferEnd === MWT.bufferStart);
}

console.log("a failed write from an old session does not close the new one");
{
    const old = await connectFully();
    drain();
    old.reliableWriter.write = () => Promise.reject(new Error("stream reset"));
    HEAPU8.set([7, 7, 7], 16384);
    // Sent while the old session is current; the write rejects asynchronously.
    lib.MirrorWT_Send(16384, 0, 3, 0, 1);
    lib.MirrorWT_Disconnect();
    drain();

    await connectFully();
    drain();
    await flush();
    check("new session survives", lib.MirrorWT_State() === 2 && MWT.events.length === 0);
}

console.log("the receive queue is capped while nobody polls");
{
    const session = await connectFully();
    drain();
    const saved = MWT.MAX_QUEUED_BYTES;
    MWT.MAX_QUEUED_BYTES = 1000;

    for (let i = 0; i < 20 && lib.MirrorWT_State() === 2; ++i) {
        session.reliableReader.next().resolve({ done: false, value: frame(0, new Uint8Array(100)) });
        await flush();
    }
    const kinds = drain();
    check("session ended instead of growing", lib.MirrorWT_State() === 0);
    check("error then disconnect at the end",
        kinds[kinds.length - 2] === ERROR && kinds[kinds.length - 1] === DISCONNECTED, kinds.join());
    check("queued data stayed under the cap", kinds.filter(k => k === DATA).length <= 10);
    check("session was closed", session.closeCalls === 1);
    MWT.MAX_QUEUED_BYTES = saved;
}

console.log("polling keeps the receive count honest");
{
    const session = await connectFully();
    drain();
    for (let i = 0; i < 5; ++i) {
        session.reliableReader.next().resolve({ done: false, value: frame(0, new Uint8Array(100)) });
        await flush();
    }
    check("bytes counted while queued", MWT.queuedBytes === 500);
    drain();
    check("count returns to zero once polled", MWT.queuedBytes === 0);
    lib.MirrorWT_Disconnect();
    drain();
}

console.log("reliable sends are capped when the uplink stalls");
{
    const session = await connectFully();
    drain();
    const saved = MWT.MAX_PENDING_SEND_BYTES;
    MWT.MAX_PENDING_SEND_BYTES = 1000;

    const held = scriptedWriter(true);
    MWT.reliableWriter = held;
    HEAPU8.set(new Uint8Array(195), 16384);

    let accepted = 0;
    for (let i = 0; i < 10; ++i) accepted += lib.MirrorWT_Send(16384, 0, 195, 0, 1);
    check("five 200 byte frames fit under 1000", accepted === 5, String(accepted));
    check("session failed on the sixth", lib.MirrorWT_State() === 0);
    check("session was closed", session.closeCalls === 1);
    MWT.MAX_PENDING_SEND_BYTES = saved;
    drain();
}

console.log("completed sends free their budget");
{
    await connectFully();
    drain();
    const saved = MWT.MAX_PENDING_SEND_BYTES;
    MWT.MAX_PENDING_SEND_BYTES = 1000;
    HEAPU8.set(new Uint8Array(195), 16384);

    let accepted = 0;
    for (let i = 0; i < 50; ++i) {
        accepted += lib.MirrorWT_Send(16384, 0, 195, 0, 1);
        await flush();
    }
    check("all 50 accepted when writes complete", accepted === 50, String(accepted));
    check("nothing left pending", MWT.pendingSendBytes === 0);
    MWT.MAX_PENDING_SEND_BYTES = saved;
    lib.MirrorWT_Disconnect();
    drain();
}

console.log("connecting again closes a session that never reported its end");
{
    const old = connect();
    // No Disconnect: Connect is called again directly.
    connect();
    check("old session closed", old.closeCalls === 1);
}

console.log("");
console.log(failures === 0 ? "all checks passed" : failures + " CHECK(S) FAILED");
process.exit(failures === 0 ? 0 : 1);

})().catch(e => { console.error(e); process.exit(1); });

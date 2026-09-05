using System.Runtime.InteropServices;

namespace Mirror.WTransport
{
    /// <summary>Event kinds produced by the native library and the browser plugin.</summary>
    // Values are part of the interop contract: keep them in step with
    // common.rs (event_kind) and MirrorWTransport.jslib.
    public enum WTEventKind
    {
        None = 0,
        Connected = 1,
        Data = 2,
        Disconnected = 3,
        Error = 4,
        /// <summary>The server installed a replacement certificate; the payload is the new hash.</summary>
        CertificateRotated = 5
    }

    /// <summary>Native error codes, mapped 1:1 onto <see cref="TransportError"/>.</summary>
    // Keep in step with common.rs (error_code) and MirrorWTransport.jslib.
    public enum WTErrorCode
    {
        None = 0,
        DnsResolve = 1,
        Refused = 2,
        Timeout = 3,
        Congestion = 4,
        InvalidReceive = 5,
        InvalidSend = 6,
        ConnectionClosed = 7,
        Unexpected = 8
    }

    /// <summary>How a Mirror channel is carried over the WebTransport session.</summary>
    public enum WTDelivery
    {
        /// <summary>
        /// Ordered and guaranteed, over the connection's single bidirectional
        /// stream. Mirror requires this for <see cref="Channels.Reliable"/>.
        /// </summary>
        Reliable = 0,

        /// <summary>
        /// A WebTransport datagram: unordered, droppable, and bounded by the
        /// path MTU rather than by the reliable message size.
        /// </summary>
        Unreliable = 1
    }

    /// <summary>Severity of a log line queued by a background thread.</summary>
    public enum WTLogLevel
    {
        Info = 0,
        Warn = 1,
        Error = 2
    }

    /// <summary>Client connection state. Keep in step with client.rs and the jslib.</summary>
    public enum WTClientState
    {
        Disconnected = 0,
        Connecting = 1,
        Connected = 2
    }

    /// <summary>Which local address the server binds its UDP socket to.</summary>
    // Keep in step with build_server_config in server.rs.
    public enum WTBindMode
    {
        /// <summary>Any address, IPv4 and IPv6 (the usual choice for a dedicated server).</summary>
        AnyDualStack = 0,
        AnyIPv4 = 1,
        AnyIPv6 = 2,
        /// <summary>Loopback only, IPv4 and IPv6.</summary>
        LoopbackDualStack = 3,
        LoopbackIPv4 = 4,
        LoopbackIPv6 = 5
    }

    /// <summary>Where the server gets its TLS identity from.</summary>
    public enum WTCertificateMode
    {
        /// <summary>
        /// Generate a short lived ECDSA P-256 certificate on every start. Browsers
        /// reject it during validation, so clients have to pin its SHA-256 hash
        /// through serverCertificateHashes. Development only.
        /// </summary>
        SelfSigned = 0,

        /// <summary>Load PEM files issued by a real certificate authority.</summary>
        PemFiles = 1
    }

    /// <summary>One polled event. Layout must match <c>MwtEvent</c> in ffi.rs.</summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct WTEvent
    {
        public int kind;
        public uint connectionId;
        public int channel;
        public int dataLength;
        public int code;
    }

    /// <summary>Server settings passed to the native library. Must match <c>MwtServerConfig</c>.</summary>
    // Pointers come first so the struct has no padding on either 32 or 64 bit.
    [StructLayout(LayoutKind.Sequential)]
    public struct WTServerConfig
    {
        public System.IntPtr certificatePath;
        public System.IntPtr keyPath;
        public System.IntPtr subjectAltNames;
        public uint port;
        public int bindMode;
        public uint keepAliveMs;
        public uint idleTimeoutMs;
        public uint handshakeTimeoutMs;
        public uint maxReliablePayload;
        public uint maxUnreliablePayload;
        public uint maxConnections;
        public uint certificateValiditySeconds;
        public uint certificateRotationSeconds;
    }

    /// <summary>Client settings passed to the native library. Must match <c>MwtClientConfig</c>.</summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct WTClientConfig
    {
        public System.IntPtr url;
        public System.IntPtr certificateHash;
        public int allowInvalidCertificates;
        public uint connectTimeoutMs;
        public uint keepAliveMs;
        public uint idleTimeoutMs;
        public uint maxReliablePayload;
        public uint maxUnreliablePayload;
    }
}

using System;

namespace Mirror.WTransport
{
    /// <summary>
    /// Snapshot of the inspector settings, so the server and client wrappers do
    /// not have to reach back into the MonoBehaviour while they run.
    /// </summary>
    public class WTSettings
    {
        // server
        public ushort port = 7777;
        public WTBindMode bindMode = WTBindMode.AnyDualStack;
        public WTCertificateMode certificateMode = WTCertificateMode.SelfSigned;
        public string certificatePath = "";
        public string keyPath = "";
        public string subjectAltNames = "localhost,127.0.0.1,::1";
        public int maxConnections;

        /// <summary>
        /// Total validity of a generated self signed certificate. Two weeks is
        /// the ceiling: neither browsers nor the native client will pin one that
        /// lives longer.
        /// </summary>
        public int certificateValiditySeconds = 14 * 24 * 60 * 60;

        /// <summary>
        /// How often a running server replaces its certificate. 0 disables it,
        /// which means the server stops accepting new connections once the
        /// certificate expires.
        /// </summary>
        public int certificateRotationSeconds = 8 * 24 * 60 * 60;

        // client
        public string path = "/";
        /// <summary>Empty means the certificate is validated normally.</summary>
        public string certificateHash = "";
        /// <summary>Native client only. Browsers have no equivalent escape hatch.</summary>
        public bool allowInvalidCertificates;
        public int connectTimeoutMs = 10000;

        // limits
        public int reliableMaxMessageSize = 64 * 1024;
        public int unreliableMaxMessageSize = 1024;

        // timing
        public int keepAliveIntervalMs = 3000;
        public int idleTimeoutMs = 10000;
        public int handshakeTimeoutMs = 10000;
        public int maxEventsPerTick = 10000;

        /// <summary>
        /// Size of the buffer the native side copies incoming payloads into. It
        /// has to hold the largest message either channel can carry, plus the
        /// error strings that are delivered through the same buffer.
        /// </summary>
        public int ReceiveBufferSize =>
            Math.Max(2048, Math.Max(reliableMaxMessageSize, unreliableMaxMessageSize) + WTUtils.SizeHeadroom);

        public uint NativeReliableLimit => (uint)(reliableMaxMessageSize + WTUtils.SizeHeadroom);

        public uint NativeUnreliableLimit => (uint)(unreliableMaxMessageSize + WTUtils.SizeHeadroom);
    }
}

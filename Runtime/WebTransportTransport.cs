// WebTransport (HTTP/3 over QUIC) transport for Mirror.
//
//   * Browser clients use the WebTransport API built into the browser, through
//     Runtime/Plugins/WebGL/MirrorWTransport.jslib.
//   * Servers, and clients running in the editor or in a standalone player, use
//     the native library built from Native~/mirror-wtransport, which wraps the
//     wtransport Rust crate.
//
// Delivery modes:
//   * Reliable ordered channels travel over a single bidirectional WebTransport
//     stream per connection, with a length prefix so message boundaries survive.
//     One stream (rather than one stream per message) is what makes the channel
//     ordered, which Mirror requires.
//   * Unreliable channels travel as WebTransport datagrams, which are bounded by
//     the QUIC datagram size and may be dropped or reordered.
using System;
using System.Net;
using UnityEngine;
using UnityEngine.Serialization;

namespace Mirror.WTransport
{
    [HelpURL("https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API")]
    [DisallowMultipleComponent]
    public class WebTransportTransport : Transport, PortTransport
    {
        /// <summary>WebTransport runs over HTTP/3, so the url scheme is always https.</summary>
        public const string Scheme = WTUtils.Scheme;

        /// <summary>
        /// Two weeks. A pinned certificate may not be valid for longer than this
        /// on either the browser or the native client, so it is a hard ceiling.
        /// </summary>
        public const int MaxCertificateValidityMinutes = 14 * 24 * 60;

        /// <summary>
        /// Raised on the server when it installed a replacement certificate, with
        /// the new SHA-256 hash (empty in PemFiles mode).
        /// </summary>
        /// <remarks>
        /// A client that pins the hash cannot connect with a stale one, so
        /// whatever hands the hash out - a master server, a lobby listing - has
        /// to be told the new value from here, not just once at startup.
        /// </remarks>
        public Action<string> OnServerCertificateRotated;

        [Header("Server")]
        [Tooltip("UDP port the server listens on, and the default port clients connect to.")]
        [SerializeField] ushort serverPort = 7777;

        public ushort Port
        {
            get => serverPort;
            set => serverPort = value;
        }

        [Tooltip("Which local addresses the server socket binds to.")]
        public WTBindMode bindMode = WTBindMode.AnyDualStack;

        [Tooltip("SelfSigned generates a throwaway certificate on every start and prints its hash, which is the only way to develop against WebTransport without a real certificate. PemFiles loads a real one.")]
        public WTCertificateMode certificateMode = WTCertificateMode.SelfSigned;

        [Tooltip("Path to the full certificate chain in PEM format, e.g. fullchain.pem. Used when certificateMode is PemFiles.")]
        public string certificatePath = "fullchain.pem";

        [Tooltip("Path to the private key in PEM format, e.g. privkey.pem. Used when certificateMode is PemFiles.")]
        public string keyPath = "privkey.pem";

        [Tooltip("Comma separated host names and IPs the self signed certificate is valid for.")]
        public string selfSignedSubjectAltNames = "localhost,127.0.0.1,::1";

        [Tooltip("Maximum simultaneous connections. 0 means unlimited.")]
        public int maxConnections = 0;

        [Tooltip("Total lifetime of a generated self signed certificate, in minutes. Capped at 20160 (14 days): neither browsers nor the native client will pin one that lives longer. Ignored in PemFiles mode.")]
        public int certificateValidityMinutes = MaxCertificateValidityMinutes;

        [Tooltip("How often a running server replaces its certificate, in minutes. 0 disables it, and the server then quietly stops accepting new connections once the certificate expires. In PemFiles mode this re-reads the files instead, so a renewal is picked up without a restart.")]
        public int certificateRotationMinutes = 8 * 24 * 60;

        [Header("Client")]
        [Tooltip("Url path the client requests, and the path advertised by ServerUri. Useful when a reverse proxy routes several games through one host.")]
        public string path = "/";

        [Tooltip("SHA-256 hash of the server certificate, as printed by the server when it generates a self signed one. Required for browsers to accept a self signed certificate. Leave empty when the server uses a real certificate.")]
        public string clientCertificateHash = "";

        [Tooltip("Native client only (editor and standalone): accept any certificate. Browsers offer no such escape hatch. Never ship this enabled.")]
        public bool clientAllowInvalidCertificates = false;

        [Tooltip("How long the client waits for the connection and handshake to complete, in milliseconds.")]
        public int connectTimeoutMs = 10000;

        [Header("Channels")]
        [Tooltip("Channel ids delivered as WebTransport datagrams: unordered, size limited, and droppable. Every other channel id travels over the reliable ordered stream. Mirror's defaults are 0 = Reliable and 1 = Unreliable.")]
        public int[] unreliableChannels = { Channels.Unreliable };

        [Header("Limits")]
        [Tooltip("Largest message accepted on a reliable channel, in bytes.")]
        public int reliableMaxMessageSize = 64 * 1024;

        [Tooltip("Largest message accepted on an unreliable channel, in bytes. QUIC datagrams are bounded by the path MTU, so values much above 1200 will start failing to send.")]
        public int unreliableMaxMessageSize = 1024;

        [Tooltip("How much Mirror batches together before starting a new reliable message. Larger batches mean fewer length prefixes, smaller ones mean less latency.")]
        public int reliableBatchThreshold = 8 * 1024;

        [Header("Timing")]
        [Tooltip("How often to send a keep alive packet, in milliseconds. 0 disables it. Must be well below the idle timeout of both peers.")]
        public int keepAliveIntervalMs = 3000;

        [Tooltip("Drop a connection after this long without traffic, in milliseconds. 0 means never.")]
        public int idleTimeoutMs = 10000;

        [Tooltip("How long the server waits for a client to finish the session and stream handshake, in milliseconds.")]
        public int handshakeTimeoutMs = 10000;

        [Tooltip("Caps how many queued events are processed per tick, so a flood cannot stall the frame indefinitely.")]
        [FormerlySerializedAs("maxEventsPerTick")]
        public int maxReceivesPerTick = 10000;

        [Header("Debug")]
        [Tooltip("Log connection lifecycle details. Warnings and errors are always logged.")]
        public bool debugLog = false;

        // runtime state ///////////////////////////////////////////////////////
        readonly WTSettings settings = new WTSettings();

        WTServer server;
        WTClient client;

        /// <summary>Lookup built from unreliableChannels, indexed by channel id.</summary>
        bool[] unreliableLookup;

        /// <summary>
        /// SHA-256 hash of the certificate the running server generated, as
        /// dotted hex. Empty unless the server is running in SelfSigned mode.
        /// Hand this to browser clients as their serverCertificateHashes value.
        /// </summary>
        public string ServerCertificateHash => server != null ? server.CertificateHash : string.Empty;

        // setup ///////////////////////////////////////////////////////////////
        protected virtual void Awake()
        {
            WTLog.verbose = debugLog;
            ApplySettings();
            WTLog.Info("WebTransport transport initialised");
        }

        protected virtual void OnValidate()
        {
            if (reliableMaxMessageSize < 1024) reliableMaxMessageSize = 1024;
            if (unreliableMaxMessageSize < 64) unreliableMaxMessageSize = 64;
            if (reliableBatchThreshold < 512) reliableBatchThreshold = 512;
            if (reliableBatchThreshold > reliableMaxMessageSize) reliableBatchThreshold = reliableMaxMessageSize;
            if (connectTimeoutMs < 1000) connectTimeoutMs = 1000;
            if (handshakeTimeoutMs < 1000) handshakeTimeoutMs = 1000;
            if (maxReceivesPerTick < 1) maxReceivesPerTick = 1;
            if (maxConnections < 0) maxConnections = 0;

            if (certificateValidityMinutes < 1) certificateValidityMinutes = 1;
            if (certificateValidityMinutes > MaxCertificateValidityMinutes)
                certificateValidityMinutes = MaxCertificateValidityMinutes;

            if (certificateRotationMinutes < 0) certificateRotationMinutes = 0;

            // Rotate well before expiry, so a rotation that fails (unreadable
            // PEM files, say) still has room for the next attempt to succeed.
            int latestRotation = Math.Max(1, certificateValidityMinutes * 3 / 4);
            if (certificateRotationMinutes > latestRotation)
                certificateRotationMinutes = latestRotation;

            unreliableLookup = null;
            WTLog.verbose = debugLog;
        }

        /// <summary>Copies the inspector values into the snapshot the backends read.</summary>
        void ApplySettings()
        {
            settings.port = serverPort;
            settings.bindMode = bindMode;
            settings.certificateMode = certificateMode;
            settings.certificatePath = certificatePath;
            settings.keyPath = keyPath;
            settings.subjectAltNames = selfSignedSubjectAltNames;
            settings.maxConnections = maxConnections;
            settings.certificateValiditySeconds = certificateValidityMinutes * 60;
            settings.certificateRotationSeconds = certificateRotationMinutes * 60;

            settings.path = WTUtils.NormalizePath(path);
            settings.certificateHash = clientCertificateHash != null ? clientCertificateHash.Trim() : string.Empty;
            settings.allowInvalidCertificates = clientAllowInvalidCertificates;
            settings.connectTimeoutMs = connectTimeoutMs;

            settings.reliableMaxMessageSize = reliableMaxMessageSize;
            settings.unreliableMaxMessageSize = unreliableMaxMessageSize;

            settings.keepAliveIntervalMs = keepAliveIntervalMs;
            settings.idleTimeoutMs = idleTimeoutMs;
            settings.handshakeTimeoutMs = handshakeTimeoutMs;
            settings.maxEventsPerTick = maxReceivesPerTick;

            WTLog.verbose = debugLog;
        }

        // channels ////////////////////////////////////////////////////////////

        /// <summary>
        /// True when the channel is delivered over the reliable ordered stream,
        /// false when it is delivered as a datagram.
        /// </summary>
        public bool IsReliableChannel(int channelId)
        {
            if (unreliableLookup == null) RebuildChannelLookup();

            if (channelId < 0 || channelId >= unreliableLookup.Length) return true;
            return !unreliableLookup[channelId];
        }

        void RebuildChannelLookup()
        {
            int highest = 0;
            if (unreliableChannels != null)
            {
                foreach (int channelId in unreliableChannels)
                    if (channelId > highest) highest = channelId;
            }

            unreliableLookup = new bool[highest + 1];

            if (unreliableChannels != null)
            {
                foreach (int channelId in unreliableChannels)
                {
                    // Channel ids travel as a single byte on the wire.
                    if (channelId < 0 || channelId > byte.MaxValue)
                    {
                        WTLog.Error($"channel id {channelId} is out of the supported range 0..255 and was ignored");
                        continue;
                    }

                    unreliableLookup[channelId] = true;
                }
            }
        }

        bool ValidateChannel(int channelId)
        {
            if (channelId >= 0 && channelId <= byte.MaxValue) return true;
            WTLog.Error($"channel id {channelId} is out of the supported range 0..255");
            return false;
        }

        // transport ///////////////////////////////////////////////////////////
        public override bool Available()
        {
#if UNITY_WEBGL && !UNITY_EDITOR
            return WTWebGLClient.IsSupportedByBrowser;
#else
            return WTNative.Available;
#endif
        }

        // Every WebTransport session runs over QUIC, which always uses TLS 1.3.
        public override bool IsEncrypted => true;
        public override string EncryptionCipher => "TLS 1.3";

        public override int GetMaxPacketSize(int channelId = Channels.Reliable) =>
            IsReliableChannel(channelId) ? reliableMaxMessageSize : unreliableMaxMessageSize;

        public override int GetBatchThreshold(int channelId) =>
            IsReliableChannel(channelId) ? reliableBatchThreshold : unreliableMaxMessageSize;

        public override void Shutdown()
        {
            if (client != null)
            {
                client.Shutdown();
                client = null;
            }

            if (server != null)
            {
                server.Stop();
                server = null;
            }
        }

        public override string ToString() => $"WebTransport [{serverPort}]";

        // client //////////////////////////////////////////////////////////////
        public override bool ClientConnected() => client != null && client.Connected;

        public override void ClientConnect(string address)
        {
            string url;
            try
            {
                url = WTUtils.BuildUrl(address, serverPort, path);
            }
            catch (Exception e)
            {
                WTLog.Error($"could not turn '{address}' into a WebTransport url: {e.Message}");
                OnClientError?.Invoke(TransportError.DnsResolve, e.Message);
                OnClientDisconnected?.Invoke();
                return;
            }

            ConnectTo(url);
        }

        public override void ClientConnect(Uri uri)
        {
            if (!uri.Scheme.Equals(Scheme, StringComparison.OrdinalIgnoreCase))
            {
                string reason = $"invalid url {uri}, WebTransport needs {Scheme}://host:port";
                WTLog.Error(reason);
                OnClientError?.Invoke(TransportError.DnsResolve, reason);
                OnClientDisconnected?.Invoke();
                return;
            }

            UriBuilder builder = new UriBuilder(uri);
            if (uri.IsDefaultPort) builder.Port = serverPort;
            if (string.IsNullOrEmpty(uri.AbsolutePath) || uri.AbsolutePath == "/")
                builder.Path = WTUtils.NormalizePath(path);

            ConnectTo(builder.Uri.AbsoluteUri);
        }

        void ConnectTo(string url)
        {
            if (ClientConnected())
            {
                WTLog.Warn("already connected");
                return;
            }

            ApplySettings();

            // Rebuilt on every connect so inspector changes (buffer sizes in
            // particular) are picked up.
            if (client == null || client.State == WTClientState.Disconnected)
                client = CreateClient();

            client.Connect(url);
        }

        WTClient CreateClient()
        {
#if UNITY_WEBGL && !UNITY_EDITOR
            WTClient created = new WTWebGLClient(settings);
#else
            WTClient created = new WTNativeClient(settings);
#endif
            created.OnConnected = () => OnClientConnected?.Invoke();
            created.OnData = (segment, channelId) => OnClientDataReceived?.Invoke(segment, channelId);
            created.OnDisconnected = () => OnClientDisconnected?.Invoke();
            created.OnError = (error, reason) => OnClientError?.Invoke(error, reason);
            return created;
        }

        public override void ClientSend(ArraySegment<byte> segment, int channelId = Channels.Reliable)
        {
            if (client == null || !client.Connected)
            {
                WTLog.Warn("cannot send while not connected");
                return;
            }

            if (!ValidateChannel(channelId)) return;

            if (client.Send(segment, channelId, IsReliableChannel(channelId)))
                OnClientDataSent?.Invoke(segment, channelId);
        }

        public override void ClientDisconnect()
        {
            if (client != null) client.Disconnect();
        }

        // Incoming messages are processed in early update so the world updates
        // with data that is at most one frame old.
        public override void ClientEarlyUpdate()
        {
            if (!enabled) return;
            if (client != null) client.Tick();
        }

        // server //////////////////////////////////////////////////////////////
        public override Uri ServerUri()
        {
            string host;
            try
            {
                host = Dns.GetHostName();
            }
            catch (Exception)
            {
                // WebGL and some sandboxed platforms have no name resolution.
                host = "localhost";
            }

            return new UriBuilder
            {
                Scheme = Scheme,
                Host = host,
                Port = server != null && server.Active ? server.LocalPort : serverPort,
                Path = WTUtils.NormalizePath(path)
            }.Uri;
        }

        public override bool ServerActive() => server != null && server.Active;

        public override void ServerStart()
        {
#if UNITY_WEBGL && !UNITY_EDITOR
            WTLog.Error("a WebGL build cannot host a WebTransport server, only connect to one");
            return;
#else
            if (ServerActive())
            {
                WTLog.Warn("the server is already running");
                return;
            }

            ApplySettings();

            server = new WTServer(settings)
            {
                OnConnected = (connectionId, address) => OnServerConnectedWithAddress?.Invoke(connectionId, address),
                OnData = (connectionId, segment, channelId) => OnServerDataReceived?.Invoke(connectionId, segment, channelId),
                OnDisconnected = connectionId => OnServerDisconnected?.Invoke(connectionId),
                OnError = (connectionId, error, reason) => OnServerError?.Invoke(connectionId, error, reason),
                OnCertificateRotated = hash => OnServerCertificateRotated?.Invoke(hash)
            };

            if (!server.Start()) server = null;
#endif
        }

        public override void ServerSend(int connectionId, ArraySegment<byte> segment, int channelId = Channels.Reliable)
        {
            if (!ServerActive()) return;
            if (!ValidateChannel(channelId)) return;

            if (server.Send(connectionId, segment, channelId, IsReliableChannel(channelId)))
                OnServerDataSent?.Invoke(connectionId, segment, channelId);
        }

        public override void ServerDisconnect(int connectionId)
        {
            if (server != null) server.Disconnect(connectionId);
        }

        public override string ServerGetClientAddress(int connectionId) =>
            server != null ? server.GetClientAddress(connectionId) : string.Empty;

        public override void ServerStop()
        {
            if (server == null) return;
            server.Stop();
            server = null;
        }

        public override void ServerEarlyUpdate()
        {
            if (!enabled) return;
            if (server != null) server.Tick();
        }
    }
}

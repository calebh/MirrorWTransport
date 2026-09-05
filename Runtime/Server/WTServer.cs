using System;

namespace Mirror.WTransport
{
    /// <summary>
    /// Managed side of the native WebTransport server. Owns the receive buffer,
    /// drains the native event queue once per Mirror tick and forwards
    /// everything to the transport on the main thread.
    /// </summary>
    public class WTServer
    {
        public Action<int, string> OnConnected;
        public Action<int, ArraySegment<byte>, int> OnData;
        public Action<int> OnDisconnected;
        public Action<int, TransportError, string> OnError;

        /// <summary>
        /// Raised when the server replaced its certificate, with the new hash
        /// (empty in PemFiles mode). Clients that pin the hash need to be told
        /// the new one, so republish it from here.
        /// </summary>
        public Action<string> OnCertificateRotated;

        readonly WTSettings settings;
        readonly byte[] receiveBuffer;

        public bool Active { get; private set; }

        /// <summary>
        /// Dotted hex SHA-256 of the certificate currently being served, or
        /// empty. Updated in place when the server rotates the certificate.
        /// </summary>
        public string CertificateHash { get; private set; } = string.Empty;

        /// <summary>The UDP port actually bound, which differs from the requested one when it was 0.</summary>
        public ushort LocalPort { get; private set; }

        public WTServer(WTSettings settings)
        {
            this.settings = settings;
            receiveBuffer = new byte[settings.ReceiveBufferSize];
        }

        public bool Start()
        {
            if (Active)
            {
                WTLog.Warn("the server is already running");
                return true;
            }

            if (!WTNative.Available) return false;

            bool selfSigned = settings.certificateMode == WTCertificateMode.SelfSigned;

            IntPtr certificatePath = IntPtr.Zero;
            IntPtr keyPath = IntPtr.Zero;
            IntPtr subjectAltNames = IntPtr.Zero;

            try
            {
                if (!selfSigned)
                {
                    if (string.IsNullOrEmpty(settings.certificatePath) || string.IsNullOrEmpty(settings.keyPath))
                    {
                        WTLog.Error("certificate mode is PemFiles but the certificate or key path is empty");
                        return false;
                    }

                    certificatePath = WTUtils.AllocUtf8(settings.certificatePath);
                    keyPath = WTUtils.AllocUtf8(settings.keyPath);
                }
                else
                {
                    subjectAltNames = WTUtils.AllocUtf8(settings.subjectAltNames);
                }

                WTServerConfig config = new WTServerConfig
                {
                    certificatePath = certificatePath,
                    keyPath = keyPath,
                    subjectAltNames = subjectAltNames,
                    port = settings.port,
                    bindMode = (int)settings.bindMode,
                    keepAliveMs = (uint)Math.Max(0, settings.keepAliveIntervalMs),
                    idleTimeoutMs = (uint)Math.Max(0, settings.idleTimeoutMs),
                    handshakeTimeoutMs = (uint)Math.Max(1000, settings.handshakeTimeoutMs),
                    maxReliablePayload = settings.NativeReliableLimit,
                    maxUnreliablePayload = settings.NativeUnreliableLimit,
                    maxConnections = (uint)Math.Max(0, settings.maxConnections),
                    certificateValiditySeconds = (uint)Math.Max(60, settings.certificateValiditySeconds),
                    certificateRotationSeconds = (uint)Math.Max(0, settings.certificateRotationSeconds)
                };

                if (WTNative.mwt_server_start(ref config) != 0)
                {
                    WTLog.Error($"could not start the server: {WTNative.LastError()}");
                    return false;
                }
            }
            finally
            {
                WTUtils.FreeUtf8(certificatePath);
                WTUtils.FreeUtf8(keyPath);
                WTUtils.FreeUtf8(subjectAltNames);
            }

            Active = true;

            int boundPort = WTNative.mwt_server_local_port();
            LocalPort = boundPort > 0 ? (ushort)boundPort : settings.port;
            CertificateHash = selfSigned ? WTNative.ServerCertificateHash() : string.Empty;

            if (selfSigned && !string.IsNullOrEmpty(CertificateHash))
            {
                // Logged unconditionally: without this hash a browser cannot
                // connect to a development server at all.
                UnityEngine.Debug.Log(
                    $"[WebTransport] server listening on port {LocalPort} with a self signed certificate.\n" +
                    $"serverCertificateHashes value for browser clients:\n{CertificateHash}");
            }

            return true;
        }

        public void Stop()
        {
            if (!Active) return;

            Active = false;
            WTNative.mwt_server_stop();
            CertificateHash = string.Empty;
            WTLog.Info("server stopped");
        }

        public string GetClientAddress(int connectionId) =>
            Active ? WTNative.ServerClientAddress((uint)connectionId) : string.Empty;

        public int ConnectionCount => Active ? WTNative.mwt_server_connection_count() : 0;

        public void Disconnect(int connectionId)
        {
            if (Active) WTNative.mwt_server_disconnect((uint)connectionId);
        }

        public unsafe bool Send(int connectionId, ArraySegment<byte> segment, int channelId, bool reliable)
        {
            if (!Active) return false;

            if (segment.Array == null || segment.Count == 0)
            {
                WTLog.Warn($"refused to send an empty message to connection {connectionId}");
                return false;
            }

            int maximum = reliable ? settings.reliableMaxMessageSize : settings.unreliableMaxMessageSize;
            if (segment.Count > maximum)
            {
                WTLog.Error($"refused to send {segment.Count} bytes to connection {connectionId} on channel {channelId}: the limit for this channel is {maximum}");
                return false;
            }

            fixed (byte* data = &segment.Array[segment.Offset])
            {
                if (WTNative.mwt_server_send(
                        (uint)connectionId,
                        channelId,
                        reliable ? 1 : 0,
                        data,
                        segment.Count) == 0)
                    return true;
            }

            WTLog.Warn($"failed to send to connection {connectionId}: {WTNative.LastError()}");
            return false;
        }

        /// <summary>Drains queued events. Must run on the main thread.</summary>
        public void Tick()
        {
            WTNative.DrainLogs();

            if (!Active) return;

            for (int processed = 0; processed < settings.maxEventsPerTick; ++processed)
            {
                // A handler may have stopped the server, in which case there is
                // nothing left to drain.
                if (!Active) return;

                if (WTNative.mwt_server_poll(out WTEvent evt, receiveBuffer, receiveBuffer.Length) == 0)
                    return;

                Dispatch(evt);
            }
        }

        void Dispatch(WTEvent evt)
        {
            int connectionId = (int)evt.connectionId;

            switch ((WTEventKind)evt.kind)
            {
                case WTEventKind.Connected:
                {
                    string address = WTUtils.ReadUtf8(receiveBuffer, evt.dataLength);
                    WTLog.Info($"connection {connectionId} connected from {address}");
                    OnConnected?.Invoke(connectionId, address);
                    break;
                }
                case WTEventKind.Data:
                {
                    OnData?.Invoke(connectionId, new ArraySegment<byte>(receiveBuffer, 0, evt.dataLength), evt.channel);
                    break;
                }
                case WTEventKind.Disconnected:
                {
                    WTLog.Info($"connection {connectionId} disconnected: {WTUtils.ReadUtf8(receiveBuffer, evt.dataLength)}");
                    OnDisconnected?.Invoke(connectionId);
                    break;
                }
                case WTEventKind.Error:
                {
                    string message = WTUtils.ReadUtf8(receiveBuffer, evt.dataLength);
                    OnError?.Invoke(connectionId, WTUtils.ToTransportError(evt.code), message);
                    break;
                }
                case WTEventKind.CertificateRotated:
                {
                    CertificateHash = WTUtils.ReadUtf8(receiveBuffer, evt.dataLength);

                    // Logged unconditionally: every client that pins the hash
                    // needs the new value, so this must not be easy to miss.
                    UnityEngine.Debug.Log(string.IsNullOrEmpty(CertificateHash)
                        ? "[WebTransport] reloaded the server certificate from disk"
                        : "[WebTransport] rotated the server certificate.\n" +
                          $"new serverCertificateHashes value for browser clients:\n{CertificateHash}");

                    OnCertificateRotated?.Invoke(CertificateHash);
                    break;
                }
            }
        }
    }
}

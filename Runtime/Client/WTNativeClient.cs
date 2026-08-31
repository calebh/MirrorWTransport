using System;

namespace Mirror.WTransport
{
    /// <summary>
    /// Client backend for the editor and for standalone players, driving the
    /// native wtransport client. Without it the transport could only be tested
    /// by deploying a WebGL build first.
    /// </summary>
    public class WTNativeClient : WTClient
    {
        readonly WTSettings settings;
        readonly byte[] receiveBuffer;

        // Mirrors the native state machine, so ordering is enforced on this side
        // as well: data is never surfaced before the connect event.
        bool connected;
        bool connecting;

        public WTNativeClient(WTSettings settings)
        {
            this.settings = settings;
            receiveBuffer = new byte[settings.ReceiveBufferSize];
        }

        public override WTClientState State
        {
            get
            {
                if (connected) return WTClientState.Connected;
                return connecting ? WTClientState.Connecting : WTClientState.Disconnected;
            }
        }

        public override void Connect(string url)
        {
            if (connecting || connected)
            {
                WTLog.Warn("already connected or connecting");
                return;
            }

            if (!WTNative.Available)
            {
                OnError?.Invoke(TransportError.Unexpected, "the native WebTransport library is not available");
                OnDisconnected?.Invoke();
                return;
            }

            IntPtr urlPointer = IntPtr.Zero;
            IntPtr hashPointer = IntPtr.Zero;

            try
            {
                urlPointer = WTUtils.AllocUtf8(url);
                hashPointer = WTUtils.AllocUtf8(settings.certificateHash);

                WTClientConfig config = new WTClientConfig
                {
                    url = urlPointer,
                    certificateHash = hashPointer,
                    allowInvalidCertificates = settings.allowInvalidCertificates ? 1 : 0,
                    connectTimeoutMs = (uint)Math.Max(1000, settings.connectTimeoutMs),
                    keepAliveMs = (uint)Math.Max(0, settings.keepAliveIntervalMs),
                    idleTimeoutMs = (uint)Math.Max(0, settings.idleTimeoutMs),
                    maxReliablePayload = settings.NativeReliableLimit,
                    maxUnreliablePayload = settings.NativeUnreliableLimit
                };

                if (WTNative.mwt_client_connect(ref config) != 0)
                {
                    string reason = WTNative.LastError();
                    WTLog.Error($"could not start connecting to {url}: {reason}");
                    OnError?.Invoke(TransportError.Unexpected, reason);
                    OnDisconnected?.Invoke();
                    return;
                }
            }
            finally
            {
                WTUtils.FreeUtf8(urlPointer);
                WTUtils.FreeUtf8(hashPointer);
            }

            connecting = true;
            WTLog.Info($"connecting to {url}");
        }

        public override void Disconnect()
        {
            if (!connecting && !connected) return;
            WTNative.mwt_client_disconnect();
        }

        public override unsafe bool Send(ArraySegment<byte> segment, int channelId, bool reliable)
        {
            if (!connected) return false;

            if (segment.Array == null || segment.Count == 0)
            {
                WTLog.Warn("refused to send an empty message");
                return false;
            }

            int maximum = reliable ? settings.reliableMaxMessageSize : settings.unreliableMaxMessageSize;
            if (segment.Count > maximum)
            {
                WTLog.Error($"refused to send {segment.Count} bytes on channel {channelId}: the limit for this channel is {maximum}");
                return false;
            }

            fixed (byte* data = &segment.Array[segment.Offset])
            {
                if (WTNative.mwt_client_send(channelId, reliable ? 1 : 0, data, segment.Count) == 0)
                    return true;
            }

            WTLog.Warn($"failed to send: {WTNative.LastError()}");
            return false;
        }

        public override void Tick()
        {
            WTNative.DrainLogs();

            if (!connecting && !connected) return;

            for (int processed = 0; processed < settings.maxEventsPerTick; ++processed)
            {
                if (!connecting && !connected) return;

                if (WTNative.mwt_client_poll(out WTEvent evt, receiveBuffer, receiveBuffer.Length) == 0)
                    return;

                Dispatch(evt);
            }
        }

        void Dispatch(WTEvent evt)
        {
            switch ((WTEventKind)evt.kind)
            {
                case WTEventKind.Connected:
                {
                    connecting = false;
                    connected = true;
                    WTLog.Info($"connected to {WTUtils.ReadUtf8(receiveBuffer, evt.dataLength)}");
                    OnConnected?.Invoke();
                    break;
                }
                case WTEventKind.Data:
                {
                    // Datagrams can outrun the connect handshake; anything that
                    // arrives before it is not addressed to a session Mirror knows.
                    if (!connected) break;
                    OnData?.Invoke(new ArraySegment<byte>(receiveBuffer, 0, evt.dataLength), evt.channel);
                    break;
                }
                case WTEventKind.Disconnected:
                {
                    if (!connecting && !connected) break;
                    connecting = false;
                    connected = false;
                    WTLog.Info($"disconnected: {WTUtils.ReadUtf8(receiveBuffer, evt.dataLength)}");
                    OnDisconnected?.Invoke();
                    break;
                }
                case WTEventKind.Error:
                {
                    string message = WTUtils.ReadUtf8(receiveBuffer, evt.dataLength);
                    OnError?.Invoke(WTUtils.ToTransportError(evt.code), message);
                    break;
                }
            }
        }

        public override void Shutdown()
        {
            connecting = false;
            connected = false;
            WTNative.mwt_client_stop();
        }
    }
}

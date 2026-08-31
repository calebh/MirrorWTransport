using System;

namespace Mirror.WTransport
{
    /// <summary>
    /// Client backend for WebGL builds. All the real work happens in
    /// <c>MirrorWTransport.jslib</c>; this drains its event queue once per tick
    /// and turns it into Mirror callbacks.
    /// </summary>
    public class WTWebGLClient : WTClient
    {
        readonly WTSettings settings;
        readonly byte[] receiveBuffer;
        readonly int[] header = new int[WTWebGLNative.HeaderSize];

        bool connected;
        bool connecting;

        public WTWebGLClient(WTSettings settings)
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

        public static bool IsSupportedByBrowser => WTWebGLNative.MirrorWT_IsSupported() != 0;

        public override void Connect(string url)
        {
            if (connecting || connected)
            {
                WTLog.Warn("already connected or connecting");
                return;
            }

            if (!IsSupportedByBrowser)
            {
                const string reason = "this browser does not support WebTransport";
                WTLog.Error(reason);
                OnError?.Invoke(TransportError.Unexpected, reason);
                OnDisconnected?.Invoke();
                return;
            }

            connecting = true;
            WTLog.Info($"connecting to {url}");
            WTWebGLNative.MirrorWT_Connect(
                url,
                settings.certificateHash ?? string.Empty,
                (int)settings.NativeReliableLimit,
                (int)settings.NativeUnreliableLimit);
        }

        public override void Disconnect()
        {
            if (!connecting && !connected) return;
            WTWebGLNative.MirrorWT_Disconnect();
        }

        public override bool Send(ArraySegment<byte> segment, int channelId, bool reliable)
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

            return WTWebGLNative.MirrorWT_Send(
                segment.Array,
                segment.Offset,
                segment.Count,
                channelId,
                reliable ? 1 : 0) != 0;
        }

        public override void Tick()
        {
            if (!connecting && !connected) return;

            for (int processed = 0; processed < settings.maxEventsPerTick; ++processed)
            {
                if (!connecting && !connected) return;

                if (WTWebGLNative.MirrorWT_Poll(header, receiveBuffer, receiveBuffer.Length) == 0)
                    return;

                Dispatch();
            }
        }

        void Dispatch()
        {
            int length = header[WTWebGLNative.HeaderLength];

            switch ((WTEventKind)header[WTWebGLNative.HeaderKind])
            {
                case WTEventKind.Connected:
                {
                    connecting = false;
                    connected = true;
                    WTLog.Info($"connected to {WTUtils.ReadUtf8(receiveBuffer, length)}");
                    WarnIfDatagramsAreTooSmall();
                    OnConnected?.Invoke();
                    break;
                }
                case WTEventKind.Data:
                {
                    if (!connected) break;
                    OnData?.Invoke(new ArraySegment<byte>(receiveBuffer, 0, length), header[WTWebGLNative.HeaderChannel]);
                    break;
                }
                case WTEventKind.Disconnected:
                {
                    if (!connecting && !connected) break;
                    connecting = false;
                    connected = false;
                    WTLog.Info($"disconnected: {WTUtils.ReadUtf8(receiveBuffer, length)}");
                    OnDisconnected?.Invoke();
                    break;
                }
                case WTEventKind.Error:
                {
                    OnError?.Invoke(
                        WTUtils.ToTransportError(header[WTWebGLNative.HeaderCode]),
                        WTUtils.ReadUtf8(receiveBuffer, length));
                    break;
                }
            }
        }

        /// <summary>
        /// The browser only reports its datagram limit once the session is up.
        /// Configuring a larger unreliable message size than the browser will
        /// send produces silent drops, so say so once, loudly.
        /// </summary>
        void WarnIfDatagramsAreTooSmall()
        {
            int browserLimit = WTWebGLNative.MirrorWT_MaxDatagramSize();
            if (browserLimit <= 0) return;

            // +1 for the channel byte the transport prefixes.
            int needed = settings.unreliableMaxMessageSize + 1;
            if (needed <= browserLimit) return;

            WTLog.Warn(
                $"Unreliable Max Message Size is {settings.unreliableMaxMessageSize} bytes, but this browser " +
                $"only sends datagrams up to {browserLimit} bytes. Anything larger will be dropped without " +
                $"reaching the server. Lower it to {browserLimit - 1} or less.");
        }

        public override void Shutdown()
        {
            if (connecting || connected) WTWebGLNative.MirrorWT_Disconnect();
            connecting = false;
            connected = false;
        }
    }
}

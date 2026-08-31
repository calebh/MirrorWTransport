using System;

namespace Mirror.WTransport
{
    /// <summary>
    /// Common surface of the two client backends: the browser one built on the
    /// WebTransport API, and the native one built on wtransport. The transport
    /// picks whichever the current platform can run and never sees the difference.
    /// </summary>
    public abstract class WTClient
    {
        public Action OnConnected;
        public Action<ArraySegment<byte>, int> OnData;
        public Action OnDisconnected;
        public Action<TransportError, string> OnError;

        public abstract WTClientState State { get; }

        public bool Connected => State == WTClientState.Connected;

        /// <summary>Starts connecting to an absolute https:// url.</summary>
        public abstract void Connect(string url);

        /// <summary>
        /// Requests a disconnect. The backend still raises
        /// <see cref="OnDisconnected"/> afterwards, which is what Mirror needs.
        /// </summary>
        public abstract void Disconnect();

        public abstract bool Send(ArraySegment<byte> segment, int channelId, bool reliable);

        /// <summary>Drains queued events. Called once per Mirror tick on the main thread.</summary>
        public abstract void Tick();

        /// <summary>Tears everything down without raising further events.</summary>
        public abstract void Shutdown();
    }
}

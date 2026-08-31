using System;
#if UNITY_WEBGL && !UNITY_EDITOR
using System.Runtime.InteropServices;
#endif

namespace Mirror.WTransport
{
    /// <summary>
    /// Bindings for <c>MirrorWTransport.jslib</c>, which drives the browser
    /// WebTransport API.
    /// </summary>
    /// <remarks>
    /// Deliberately polling based rather than callback based. Emscripten
    /// function pointer callbacks would deliver messages at arbitrary points
    /// inside a frame, whereas Mirror wants everything handed over from
    /// ClientEarlyUpdate; polling also avoids the dynCall compatibility dance
    /// between Emscripten versions.
    /// </remarks>
    public static class WTWebGLNative
    {
        /// <summary>Indices into the header array filled by <c>MirrorWT_Poll</c>.</summary>
        public const int HeaderKind = 0;
        public const int HeaderChannel = 1;
        public const int HeaderLength = 2;
        public const int HeaderCode = 3;
        public const int HeaderSize = 4;

#if UNITY_WEBGL && !UNITY_EDITOR
        [DllImport("__Internal")]
        public static extern void MirrorWT_Connect(string url, string certificateHash, int maxReliablePayload, int maxUnreliablePayload);

        [DllImport("__Internal")]
        public static extern void MirrorWT_Disconnect();

        /// <summary>0 disconnected, 1 connecting, 2 connected.</summary>
        [DllImport("__Internal")]
        public static extern int MirrorWT_State();

        [DllImport("__Internal")]
        public static extern int MirrorWT_Send(byte[] data, int offset, int length, int channel, int reliable);

        /// <summary>Writes one event into the caller buffers. Returns 1 or 0.</summary>
        [DllImport("__Internal")]
        public static extern int MirrorWT_Poll(int[] header, byte[] buffer, int capacity);

        /// <summary>What the browser reports as the largest sendable datagram, or 0.</summary>
        [DllImport("__Internal")]
        public static extern int MirrorWT_MaxDatagramSize();

        /// <summary>True when the browser exposes the WebTransport API at all.</summary>
        [DllImport("__Internal")]
        public static extern int MirrorWT_IsSupported();
#else
        const string NotWebGL = "the browser WebTransport client is only available in WebGL builds";

        public static void MirrorWT_Connect(string url, string certificateHash, int maxReliablePayload, int maxUnreliablePayload) => throw new NotSupportedException(NotWebGL);
        public static void MirrorWT_Disconnect() => throw new NotSupportedException(NotWebGL);
        public static int MirrorWT_State() => 0;
        public static int MirrorWT_Send(byte[] data, int offset, int length, int channel, int reliable) => 0;
        public static int MirrorWT_Poll(int[] header, byte[] buffer, int capacity) => 0;
        public static int MirrorWT_MaxDatagramSize() => 0;
        public static int MirrorWT_IsSupported() => 0;
#endif
    }
}

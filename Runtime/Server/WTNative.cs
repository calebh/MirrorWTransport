using System;
using System.Runtime.InteropServices;

namespace Mirror.WTransport
{
    /// <summary>
    /// P/Invoke surface of the native library built from <c>Native~/mirror-wtransport</c>.
    /// Everything is polling based: nothing here calls back into managed code, so
    /// no callback can arrive off the main thread.
    /// </summary>
    public static class WTNative
    {
        /// <summary>
        /// File name without prefix or extension: <c>mirror_wtransport.dll</c>,
        /// <c>libmirror_wtransport.so</c>, <c>libmirror_wtransport.dylib</c>.
        /// </summary>
        public const string Library = "mirror_wtransport";

        /// <summary>Must match <c>ABI_VERSION</c> in ffi.rs.</summary>
        public const uint ExpectedAbiVersion = 2;

        // Scratch buffer for the strings the native side hands back (addresses,
        // error messages, log lines). Only ever touched from the main thread.
        static readonly byte[] TextBuffer = new byte[2048];

#if UNITY_WEBGL && !UNITY_EDITOR
        // WebGL players have no native plugin at all: IL2CPP would fail to link
        // these imports, and the browser client is used instead.
        const string NotSupported = "the native WebTransport library is not available in WebGL builds";

        internal static uint mwt_abi_version() => 0;
        internal static int mwt_last_error(byte[] buffer, int capacity) => 0;
        internal static int mwt_poll_log(out int level, byte[] buffer, int capacity) { level = 0; return -1; }

        internal static int mwt_server_start(ref WTServerConfig config) => -1;
        internal static void mwt_server_stop() {}
        internal static int mwt_server_is_active() => 0;
        internal static int mwt_server_local_port() => -1;
        internal static int mwt_server_connection_count() => 0;
        internal static int mwt_server_certificate_hash(byte[] buffer, int capacity) => -1;
        internal static int mwt_server_client_address(uint connectionId, byte[] buffer, int capacity) => -1;
        internal static int mwt_server_poll(out WTEvent evt, byte[] buffer, int capacity) { evt = default; return 0; }
        internal static unsafe int mwt_server_send(uint connectionId, int channel, int reliable, byte* data, int length) => -1;
        internal static void mwt_server_disconnect(uint connectionId) {}

        internal static int mwt_client_connect(ref WTClientConfig config) => -1;
        internal static void mwt_client_disconnect() {}
        internal static void mwt_client_stop() {}
        internal static int mwt_client_state() => 0;
        internal static int mwt_client_poll(out WTEvent evt, byte[] buffer, int capacity) { evt = default; return 0; }
        internal static unsafe int mwt_client_send(int channel, int reliable, byte* data, int length) => -1;
#else
        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern uint mwt_abi_version();

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_last_error(byte[] buffer, int capacity);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_poll_log(out int level, byte[] buffer, int capacity);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_server_start(ref WTServerConfig config);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void mwt_server_stop();

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_server_is_active();

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_server_local_port();

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_server_connection_count();

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_server_certificate_hash(byte[] buffer, int capacity);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_server_client_address(uint connectionId, byte[] buffer, int capacity);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_server_poll(out WTEvent evt, byte[] buffer, int capacity);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern unsafe int mwt_server_send(uint connectionId, int channel, int reliable, byte* data, int length);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void mwt_server_disconnect(uint connectionId);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_client_connect(ref WTClientConfig config);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void mwt_client_disconnect();

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void mwt_client_stop();

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_client_state();

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int mwt_client_poll(out WTEvent evt, byte[] buffer, int capacity);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern unsafe int mwt_client_send(int channel, int reliable, byte* data, int length);
#endif

        // -------------------------------------------------------------------
        // Managed conveniences
        // -------------------------------------------------------------------

        static int available = -1;

        /// <summary>
        /// True when the native library loaded and reports the ABI this package
        /// was written against. The probe runs once and the result is cached,
        /// because a failed P/Invoke resolve is expensive.
        /// </summary>
        public static bool Available
        {
            get
            {
                if (available >= 0) return available == 1;

#if UNITY_WEBGL && !UNITY_EDITOR
                available = 0;
#else
                try
                {
                    uint abi = mwt_abi_version();
                    if (abi != ExpectedAbiVersion)
                    {
                        WTLog.Error($"the native library reports ABI {abi} but this package expects {ExpectedAbiVersion}. Rebuild it from Native~/mirror-wtransport.");
                        available = 0;
                    }
                    else
                    {
                        available = 1;
                    }
                }
                catch (DllNotFoundException)
                {
                    WTLog.Error($"could not load '{Library}'. Build it from Native~/mirror-wtransport and drop the result into Runtime/Plugins.");
                    available = 0;
                }
                catch (EntryPointNotFoundException e)
                {
                    WTLog.Error($"'{Library}' is missing an entry point ({e.Message}). It is probably an older build; rebuild it from Native~/mirror-wtransport.");
                    available = 0;
                }
#endif
                return available == 1;
            }
        }

        /// <summary>Message of the most recent native failure.</summary>
        public static string LastError()
        {
            int length = mwt_last_error(TextBuffer, TextBuffer.Length);
            return WTUtils.ReadUtf8(TextBuffer, length);
        }

        /// <summary>Drains the log lines queued by the native background threads.</summary>
        public static void DrainLogs()
        {
            const int MaxPerCall = 64;

            for (int i = 0; i < MaxPerCall; ++i)
            {
                int length = mwt_poll_log(out int level, TextBuffer, TextBuffer.Length);
                if (length < 0) return;
                WTLog.Write((WTLogLevel)level, WTUtils.ReadUtf8(TextBuffer, length));
            }
        }

        /// <summary>SHA-256 hash of the self signed certificate, or empty.</summary>
        public static string ServerCertificateHash()
        {
            int length = mwt_server_certificate_hash(TextBuffer, TextBuffer.Length);
            return length < 0 ? string.Empty : WTUtils.ReadUtf8(TextBuffer, length);
        }

        public static string ServerClientAddress(uint connectionId)
        {
            int length = mwt_server_client_address(connectionId, TextBuffer, TextBuffer.Length);
            return length < 0 ? string.Empty : WTUtils.ReadUtf8(TextBuffer, length);
        }
    }
}

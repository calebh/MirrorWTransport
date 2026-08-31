using System;
using System.Runtime.InteropServices;
using System.Text;

namespace Mirror.WTransport
{
    /// <summary>Shared helpers: url building, UTF-8 interop and error translation.</summary>
    public static class WTUtils
    {
        /// <summary>WebTransport always runs over HTTP/3, so the scheme is always https.</summary>
        public const string Scheme = "https";

        /// <summary>
        /// Extra bytes the native side accepts on top of the configured maximum
        /// message size. Mirror prefixes every batch with a timestamp, so the
        /// segment handed to the transport is slightly larger than the largest
        /// message Mirror validated.
        /// </summary>
        public const int SizeHeadroom = 64;

        // -------------------------------------------------------------------
        // Urls
        // -------------------------------------------------------------------

        /// <summary>
        /// Turns whatever Mirror hands to ClientConnect into an absolute
        /// WebTransport url. Accepts "host", "host:port" and a full
        /// "https://host:port/path", so a NetworkManager address field keeps
        /// working unchanged.
        /// </summary>
        public static string BuildUrl(string address, ushort defaultPort, string path)
        {
            if (string.IsNullOrEmpty(address))
                throw new ArgumentException("address is empty", nameof(address));

            address = address.Trim();

            // Already absolute: keep what was given, and only fill in the parts
            // that were left out.
            if (address.IndexOf("://", StringComparison.Ordinal) >= 0)
            {
                Uri parsed = new Uri(address);
                if (!parsed.Scheme.Equals(Scheme, StringComparison.OrdinalIgnoreCase))
                    throw new ArgumentException($"WebTransport requires the {Scheme} scheme, got {parsed.Scheme}://", nameof(address));

                UriBuilder absolute = new UriBuilder(parsed);
                if (parsed.IsDefaultPort) absolute.Port = defaultPort;
                if (string.IsNullOrEmpty(parsed.AbsolutePath) || parsed.AbsolutePath == "/")
                    absolute.Path = NormalizePath(path);

                return absolute.Uri.AbsoluteUri;
            }

            SplitHostAndPort(address, defaultPort, out string host, out ushort port);

            return new UriBuilder
            {
                Scheme = Scheme,
                Host = host,
                Port = port,
                Path = NormalizePath(path)
            }.Uri.AbsoluteUri;
        }

        /// <summary>
        /// Splits "host", "host:port", "[::1]:port" and a bare IPv6 literal.
        /// A bare literal is bracketed so it survives the UriBuilder.
        /// </summary>
        public static void SplitHostAndPort(string address, ushort defaultPort, out string host, out ushort port)
        {
            port = defaultPort;

            if (address.StartsWith("[", StringComparison.Ordinal))
            {
                int closing = address.IndexOf(']');
                if (closing < 0)
                {
                    host = address;
                    return;
                }

                host = address.Substring(0, closing + 1);
                if (closing + 1 < address.Length && address[closing + 1] == ':')
                    ushort.TryParse(address.Substring(closing + 2), out port);
                return;
            }

            int lastColon = address.LastIndexOf(':');
            int firstColon = address.IndexOf(':');

            // More than one colon and no brackets: a bare IPv6 literal.
            if (firstColon >= 0 && firstColon != lastColon)
            {
                host = "[" + address + "]";
                return;
            }

            if (lastColon > 0 && ushort.TryParse(address.Substring(lastColon + 1), out ushort parsed))
            {
                host = address.Substring(0, lastColon);
                port = parsed;
                return;
            }

            host = address;
        }

        public static string NormalizePath(string path)
        {
            if (string.IsNullOrEmpty(path)) return "/";
            return path.StartsWith("/", StringComparison.Ordinal) ? path : "/" + path;
        }

        // -------------------------------------------------------------------
        // Errors
        // -------------------------------------------------------------------

        public static TransportError ToTransportError(int nativeCode)
        {
            switch ((WTErrorCode)nativeCode)
            {
                case WTErrorCode.DnsResolve: return TransportError.DnsResolve;
                case WTErrorCode.Refused: return TransportError.Refused;
                case WTErrorCode.Timeout: return TransportError.Timeout;
                case WTErrorCode.Congestion: return TransportError.Congestion;
                case WTErrorCode.InvalidReceive: return TransportError.InvalidReceive;
                case WTErrorCode.InvalidSend: return TransportError.InvalidSend;
                case WTErrorCode.ConnectionClosed: return TransportError.ConnectionClosed;
                default: return TransportError.Unexpected;
            }
        }

        // -------------------------------------------------------------------
        // UTF-8 interop
        // -------------------------------------------------------------------

        /// <summary>
        /// Allocates a NUL terminated UTF-8 copy of <paramref name="text"/>.
        /// Returns <see cref="IntPtr.Zero"/> for null and empty, which the
        /// native side reads as "not set". Free with <see cref="FreeUtf8"/>.
        /// </summary>
        // Marshal.StringToCoTaskMemUTF8 would do this, but it is missing from
        // the .NET Standard 2.0 profile some projects still build against.
        public static IntPtr AllocUtf8(string text)
        {
            if (string.IsNullOrEmpty(text)) return IntPtr.Zero;

            byte[] bytes = Encoding.UTF8.GetBytes(text);
            IntPtr pointer = Marshal.AllocHGlobal(bytes.Length + 1);
            Marshal.Copy(bytes, 0, pointer, bytes.Length);
            Marshal.WriteByte(pointer, bytes.Length, 0);
            return pointer;
        }

        public static void FreeUtf8(IntPtr pointer)
        {
            if (pointer != IntPtr.Zero) Marshal.FreeHGlobal(pointer);
        }

        public static string ReadUtf8(byte[] buffer, int length)
        {
            if (buffer == null || length <= 0) return string.Empty;
            if (length > buffer.Length) length = buffer.Length;
            return Encoding.UTF8.GetString(buffer, 0, length);
        }
    }
}

using UnityEngine;

namespace Mirror.WTransport
{
    /// <summary>Shared logging so every message is tagged and info level can be muted.</summary>
    public static class WTLog
    {
        const string Prefix = "[WebTransport] ";

        /// <summary>Set from the transport inspector. Info lines are dropped when false.</summary>
        public static bool verbose;

        public static void Info(string message)
        {
            if (verbose) Debug.Log(Prefix + message);
        }

        public static void Warn(string message) => Debug.LogWarning(Prefix + message);

        public static void Error(string message) => Debug.LogError(Prefix + message);

        public static void Write(WTLogLevel level, string message)
        {
            switch (level)
            {
                case WTLogLevel.Error:
                    Error(message);
                    break;
                case WTLogLevel.Warn:
                    Warn(message);
                    break;
                default:
                    Info(message);
                    break;
            }
        }
    }
}

using UnityEditor;
using UnityEngine;

namespace Mirror.WTransport.EditorScripts
{
    /// <summary>
    /// Adds the bits of context the default inspector cannot show: whether the
    /// channel list is safe, whether the native library is actually loadable,
    /// and the certificate hash a browser needs in order to talk to a
    /// development server.
    /// </summary>
    [CustomEditor(typeof(WebTransportTransport))]
    public class WebTransportTransportEditor : Editor
    {
        public override void OnInspectorGUI()
        {
            DrawDefaultInspector();

            WebTransportTransport transport = (WebTransportTransport)target;

            EditorGUILayout.Space();

            DrawChannelValidation(transport);
            DrawNativeLibraryStatus();
            DrawCertificateHelp(transport);
        }

        /// <summary>
        /// The one channel setting that is never a trade-off. Everything else in
        /// the list is a judgement call the default array drawer handles fine.
        /// </summary>
        void DrawChannelValidation(WebTransportTransport transport)
        {
            if (!transport.ReliableChannelMisconfigured) return;

            EditorGUILayout.HelpBox(
                $"Channel {Channels.Reliable} is set to Unreliable. Mirror sends spawn, scene and " +
                "ownership messages on it, so this does not just make the game slower, it breaks it. " +
                "The first entry in Channels has to be Reliable.",
                MessageType.Error);
        }

        void DrawNativeLibraryStatus()
        {
            // Only meaningful for the editor and for standalone players: WebGL
            // builds use the browser API and never load the native library.
            if (WTNative.Available) return;

            EditorGUILayout.HelpBox(
                $"The native library '{WTNative.Library}' is not loaded, so this transport can neither host " +
                "a server nor connect from the editor. Build it with Native~/build.ps1 (or build.sh) and " +
                "make sure the result landed in Runtime/Plugins/x86_64.\n\n" +
                "WebGL builds are unaffected: they use the browser WebTransport API.",
                MessageType.Warning);
        }

        void DrawCertificateHelp(WebTransportTransport transport)
        {
            if (transport.certificateMode == WTCertificateMode.PemFiles)
            {
                EditorGUILayout.HelpBox(
                    "Clients connect normally as long as the certificate chain is trusted. Leave " +
                    "Client Certificate Hash empty in that case.\n\n" +
                    (transport.certificateRotationMinutes > 0
                        ? $"The certificate files are re-read every {Describe(transport.certificateRotationMinutes)}, " +
                          "so a renewal is picked up without restarting the server."
                        : "Certificate Rotation Minutes is 0, so a renewed certificate on disk is only " +
                          "picked up when the server restarts."),
                    MessageType.None);
                return;
            }

            DrawRotationWarning(transport);

            string hash = transport.ServerCertificateHash;

            if (string.IsNullOrEmpty(hash))
            {
                EditorGUILayout.HelpBox(
                    "A self signed certificate is generated on every server start, and it changes every " +
                    "time. Start the server, then copy the hash shown here into the Client Certificate " +
                    "Hash field of the client. Browsers reject self signed certificates unless the hash " +
                    "is pinned this way.\n\n" +
                    "Use PemFiles with a real certificate for anything you ship.",
                    MessageType.Info);
                return;
            }

            EditorGUILayout.LabelField("Running server certificate hash", EditorStyles.boldLabel);
            EditorGUILayout.SelectableLabel(hash, EditorStyles.textArea, GUILayout.Height(34));

            using (new EditorGUILayout.HorizontalScope())
            {
                if (GUILayout.Button("Copy hash"))
                    EditorGUIUtility.systemCopyBuffer = hash;

                if (GUILayout.Button("Copy into Client Certificate Hash"))
                {
                    Undo.RecordObject(transport, "Set client certificate hash");
                    transport.clientCertificateHash = hash;
                    EditorUtility.SetDirty(transport);
                }
            }

            // The hash changes on every restart and on every rotation, so the
            // inspector has to keep repainting while play mode is running.
            Repaint();
        }

        /// <summary>
        /// Rotation being off is the one setting whose consequence is invisible
        /// until it bites: the server keeps running and simply stops accepting
        /// new players once the certificate expires.
        /// </summary>
        void DrawRotationWarning(WebTransportTransport transport)
        {
            if (transport.certificateRotationMinutes > 0) return;

            EditorGUILayout.HelpBox(
                "Certificate rotation is disabled. This certificate expires after " +
                $"{Describe(transport.certificateValidityMinutes)}, and after that the server keeps " +
                "running and keeps everyone already connected, but every new connection fails. " +
                "Set Certificate Rotation Minutes above 0 for a server that stays up longer than that.",
                MessageType.Warning);
        }

        static string Describe(int minutes)
        {
            if (minutes >= 24 * 60) return $"{minutes / (24f * 60f):0.#} days";
            if (minutes >= 60) return $"{minutes / 60f:0.#} hours";
            return $"{minutes} minutes";
        }
    }
}

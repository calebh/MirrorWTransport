using UnityEditor;
using UnityEngine;

namespace Mirror.WTransport.EditorScripts
{
    /// <summary>
    /// Adds the bits of context the default inspector cannot show: whether the
    /// native library is actually loadable, and the certificate hash a browser
    /// needs in order to talk to a development server.
    /// </summary>
    [CustomEditor(typeof(WebTransportTransport))]
    public class WebTransportTransportEditor : Editor
    {
        public override void OnInspectorGUI()
        {
            DrawDefaultInspector();

            WebTransportTransport transport = (WebTransportTransport)target;

            EditorGUILayout.Space();

            DrawNativeLibraryStatus();
            DrawCertificateHelp(transport);
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
                    "Client Certificate Hash empty in that case.",
                    MessageType.None);
                return;
            }

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

            // The hash changes on every restart, so the inspector has to keep
            // repainting while play mode is running.
            Repaint();
        }
    }
}

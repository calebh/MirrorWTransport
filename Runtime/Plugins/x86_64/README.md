The native library goes here.

Build it with `Native~/build.ps1` (Windows) or `Native~/build.sh` (macOS, Linux);
both copy the result into this folder:

    mirror_wtransport.dll        Windows
    libmirror_wtransport.so      Linux
    libmirror_wtransport.dylib   macOS

Unity assigns the platform from the file extension and the 64 bit architecture
from this folder name. Open the Plugin Inspector once after the first import to
confirm, especially if you are shipping a dedicated server build.

WebGL builds do not use any of this: they talk to the browser WebTransport API
through `Runtime/Plugins/WebGL/MirrorWTransport.jslib`.

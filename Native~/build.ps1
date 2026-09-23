# Builds the native WebTransport library and drops it where Unity picks it up.
#
#   powershell -ExecutionPolicy Bypass -File Native~/build.ps1
#
# Requires a Rust toolchain (https://rustup.rs). Pass -Target to cross compile,
# for example:
#
#   ./build.ps1 -Target x86_64-unknown-linux-gnu
#
# -DebugBuild builds the unoptimised profile. It is not called -Debug because
# PowerShell reserves that name as a common parameter.
[CmdletBinding()]
param(
    [string]$Target = "",
    [switch]$DebugBuild
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$crate = Join-Path $root "mirror-wtransport"
$plugins = Join-Path (Split-Path -Parent $root) "Runtime/Plugins/x86_64"

$profileName = if ($DebugBuild) { "debug" } else { "release" }

$cargoArgs = @("build")
if (-not $DebugBuild) { $cargoArgs += "--release" }
if ($Target -ne "") { $cargoArgs += @("--target", $Target) }

Write-Host "building mirror-wtransport ($profileName)..."
Push-Location $crate
try {
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}

$outDir = if ($Target -ne "") {
    Join-Path $crate "target/$Target/$profileName"
} else {
    Join-Path $crate "target/$profileName"
}

# Only the artifact this invocation actually produces gets published. Copying
# whatever happens to be lying in the output directory silently republishes
# stale libraries: a Linux build run from WSL writes its .so into the very same
# target/release as a Windows build, and it would otherwise be copied out again
# long after it stopped matching the source.
$expected = if ($Target -ne "") {
    if ($Target -match "windows") { "mirror_wtransport.dll" }
    elseif ($Target -match "apple|darwin") { "libmirror_wtransport.dylib" }
    else { "libmirror_wtransport.so" }
}
elseif ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) { "mirror_wtransport.dll" }
elseif ($IsMacOS) { "libmirror_wtransport.dylib" }
else { "libmirror_wtransport.so" }

$built = Join-Path $outDir $expected
if (-not (Test-Path $built)) {
    throw "cargo reported success but $expected is not in $outDir"
}

New-Item -ItemType Directory -Force $plugins | Out-Null
Copy-Item $built (Join-Path $plugins $expected) -Force
Write-Host "copied $expected to Runtime/Plugins/x86_64"

# Point out the other platforms' libraries when they have fallen behind, since
# nothing else will notice until a dedicated server build fails the ABI check.
foreach ($other in @("mirror_wtransport.dll", "libmirror_wtransport.so", "libmirror_wtransport.dylib")) {
    if ($other -eq $expected) { continue }

    $published = Join-Path $plugins $other
    if ((Test-Path $published) -and (Get-Item $published).LastWriteTime -lt (Get-Item $built).LastWriteTime) {
        Write-Warning "$other in Runtime/Plugins/x86_64 is older than the library just built. Rebuild it for that platform before shipping."
    }
}

Write-Host "done. Unity will import it on the next domain reload."

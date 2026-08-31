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

# One of these exists depending on the host or the requested target.
$candidates = @(
    "mirror_wtransport.dll",
    "libmirror_wtransport.so",
    "libmirror_wtransport.dylib"
)

New-Item -ItemType Directory -Force $plugins | Out-Null

$copied = $false
foreach ($name in $candidates) {
    $built = Join-Path $outDir $name
    if (Test-Path $built) {
        Copy-Item $built (Join-Path $plugins $name) -Force
        Write-Host "copied $name to Runtime/Plugins/x86_64"
        $copied = $true
    }
}

if (-not $copied) {
    throw "no library was produced in $outDir"
}

Write-Host "done. Unity will import it on the next domain reload."

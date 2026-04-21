# Downloads a pinned Windows Chromium snapshot for bundling into the MSI.
#
# Usage:
#   .\scripts\fetch-chromium.ps1                # uses pinned revision
#   .\scripts\fetch-chromium.ps1 -Revision 1300000
#   .\scripts\fetch-chromium.ps1 -Dest chromium-bundle
#
# Layout after success:
#   <Dest>\chrome-win\chrome.exe
#   <Dest>\chrome-win\... (DLLs, locales, resources)
#
# The `chrome-win\` subfolder is the natural layout inside Chromium's zip;
# main.wxs and find_chrome() in src/main.rs depend on this exact shape.

[CmdletBinding()]
param(
    # Pinned Chromium snapshot revision.  Bump to a newer number when you want
    # to refresh; keep it pinned for reproducible builds.  See
    # https://commondatastorage.googleapis.com/chromium-browser-snapshots/Win_x64/
    # for the list of available revisions.
    [string]$Revision = '1381562',

    # Destination directory (relative to repo root).
    [string]$Dest = 'chromium-bundle'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference    = 'SilentlyContinue'  # massive speedup on Invoke-WebRequest

$url = "https://commondatastorage.googleapis.com/chromium-browser-snapshots/Win_x64/$Revision/chrome-win.zip"

Write-Host "fetch-chromium: revision = $Revision"
Write-Host "fetch-chromium: dest     = $Dest"
Write-Host "fetch-chromium: url      = $url"

# Skip if already extracted (GH Actions cache or local re-run).
if (Test-Path (Join-Path $Dest 'chrome-win\chrome.exe')) {
    Write-Host "fetch-chromium: chrome.exe already present, skipping download."
    exit 0
}

$tmp = Join-Path $env:TEMP "mdreader-chromium-$Revision.zip"

if (-not (Test-Path $tmp)) {
    Write-Host "fetch-chromium: downloading..."
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
} else {
    Write-Host "fetch-chromium: cached zip at $tmp"
}

if (-not (Test-Path $Dest)) {
    New-Item -ItemType Directory -Path $Dest -Force | Out-Null
}

Write-Host "fetch-chromium: extracting..."
Expand-Archive -Path $tmp -DestinationPath $Dest -Force

$chrome = Join-Path $Dest 'chrome-win\chrome.exe'
if (-not (Test-Path $chrome)) {
    Write-Error "fetch-chromium: chrome.exe not found after extraction at $chrome"
    exit 1
}

# Trim interactive / unused binaries to shrink installer (optional, ~20 MB off).
# Conservative list — only remove things a headless --print-to-pdf never touches.
$trim = @(
    'chrome_proxy.exe',
    'notification_helper.exe',
    'chromedriver.exe',
    'mojo_core.dll',  # already in chrome.dll; only used by mojo unit tests
    'interactive_ui_tests.exe'
)
foreach ($name in $trim) {
    $p = Join-Path $Dest "chrome-win\$name"
    if (Test-Path $p) { Remove-Item $p -Force -ErrorAction SilentlyContinue }
}

$size = (Get-ChildItem -Recurse (Join-Path $Dest 'chrome-win') | Measure-Object -Property Length -Sum).Sum
Write-Host ("fetch-chromium: done ({0:N1} MB staged)" -f ($size / 1MB))

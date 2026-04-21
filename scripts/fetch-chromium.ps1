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
    # to refresh; keep it pinned for reproducible builds.  Not every integer
    # in the history is a valid snapshot — the Win_x64 bucket only contains
    # revisions where a full build was uploaded.  If the pinned value is
    # missing the script falls back to the current LAST_CHANGE pointer.
    # See https://commondatastorage.googleapis.com/chromium-browser-snapshots/Win_x64/
    [string]$Revision = '1618066',

    # Destination directory (relative to repo root).
    [string]$Dest = 'chromium-bundle'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference    = 'SilentlyContinue'  # massive speedup on Invoke-WebRequest

function Test-ChromiumRevision([string]$rev) {
    $u = "https://commondatastorage.googleapis.com/chromium-browser-snapshots/Win_x64/$rev/chrome-win.zip"
    try {
        $r = Invoke-WebRequest -Uri $u -Method Head -UseBasicParsing -ErrorAction Stop
        return ($r.StatusCode -eq 200)
    } catch {
        return $false
    }
}

# Validate the pinned revision; fall back to the bucket's LAST_CHANGE pointer
# if the pin is missing.  This keeps builds reproducible most of the time but
# avoids the whole release failing when an old pin gets pruned.
if (-not (Test-ChromiumRevision $Revision)) {
    Write-Host "fetch-chromium: pinned revision $Revision not available, querying LAST_CHANGE..."
    $latest = (Invoke-WebRequest -Uri 'https://commondatastorage.googleapis.com/chromium-browser-snapshots/Win_x64/LAST_CHANGE' -UseBasicParsing).Content.Trim()
    if (-not $latest -or -not (Test-ChromiumRevision $latest)) {
        Write-Error "fetch-chromium: cannot resolve a valid Chromium revision (tried '$Revision' and LAST_CHANGE='$latest')"
        exit 1
    }
    Write-Host "fetch-chromium: using LAST_CHANGE revision $latest instead"
    $Revision = $latest
}

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

<#
.SYNOPSIS
    Fetches the official Web Platform Tests at a pinned revision, sparsely.

.DESCRIPTION
    Materializes ONLY the top-level trees the runner is permitted to execute
    (css/, dom/, html/, resources/) from a pinned revision of the official
    suite, into a gitignored cache under tools/wpt/.cache/.

    Everything here exists to keep a conformance number comparable:

      * The revision is pinned in this file, written next to the checkout, and
        echoed into the results file on every run. A conformance figure without
        a pinned suite revision is not comparable to anything.

      * The transport is the GitHub codeload tarball for that exact revision,
        not a branch or a tag. A tarball cannot drift between the moment it is
        named and the moment it is read.

      * Only the trees under test are extracted. The full suite is on the order
        of gigabytes and six other agents are building on this machine.

      * The script never touches the rENDER repository's git state. It does not
        invoke git at all on the primary path, and on the fallback path it is
        confined to the cache directory. The orchestrator owns history.

      * Network is required on first run. This is stated in the output rather
        than swallowed: a note in docs/wpt.md records that the suite was
        believed unobtainable for most of this project's history, and that
        belief has to be falsifiable.

      * An incomplete or unverified checkout throws instead of returning, so a
        later run cannot silently disagree with an earlier one.

.PARAMETER CacheDir
    Override the cache location. Defaults to <tools/wpt/.cache/wpt>.

.PARAMETER Revision
    Override the pinned revision, for reproducibility experiments only.

.PARAMETER Force
    Re-fetch even if the cache already matches the pin.

.EXAMPLE
    powershell -File tools/wpt/fetch-wpt.ps1
#>
[CmdletBinding()]
param(
    [string] $CacheDir,
    [string] $Revision,
    [switch] $Force
)

$ErrorPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Keep in sync with docs/wpt.md. Do not float this to a branch: an unpinned
# suite makes every recorded number incomparable to the next one.
$WptRevision = 'c7fdee80f3f17b4e9813964916afdfd57ace863f'
$WptRemote = 'https://github.com/web-platform-tests/wpt.git'
$WptArchiveBase = 'https://codeload.github.com/web-platform-tests/wpt/tar.gz/'

# The only trees the runner may execute. Anything outside this list is a
# deliberate scope decision, not an accident: canvas/, webgpu/, webaudio/,
# webrtc/, wasm/, network/, service-workers/ and storage/ need a browser this
# engine is not, and counting them as failures would manufacture a number out of
# missing capability.
# Shared trees, not test areas. `common/` holds `subset-tests-by-key.js`,
# `gc.js` and `get-host-info.sub.js`, and `fonts/ahem.css` is the font every
# CSS metrics test uses. Tests in css/ and html/ reference both with rooted URLs.
# Omitting them makes ~3,600 tests report a *missing fixture*, which is a true
# statement about the wrong thing: the fixture exists at the pinned revision and
# this fetch simply did not take it. A wrong "missing fixture" is worse than an
# admitted scope limit, because it reads as a suite defect.
$WptTrees = @('css', 'dom', 'html', 'resources', 'common', 'fonts')

if (-not [string]::IsNullOrWhiteSpace($Revision)) {
    $WptRevision = $Revision.Trim().ToLowerInvariant()
}

if ([string]::IsNullOrWhiteSpace($CacheDir)) {
    $CacheDir = Join-Path $PSScriptRoot '.cache\wpt'
}
$CacheDir = [IO.Path]::GetFullPath($CacheDir)
$DownloadDir = Join-Path (Split-Path $CacheDir -Parent) 'downloads'

function Write-Utf8NoNewline {
    param([Parameter(Mandatory)][string] $Path, [Parameter(Mandatory)][string] $Text)
    # Explicit [System.IO.File] UTF-8 without BOM. Set-Content is avoided on
    # purpose: it has written UTF-16 on some hosts and corrupted files in this
    # tree.
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

function Test-CacheIsCurrent {
    if ($Force) { return $false }
    $pinFile = Join-Path $CacheDir '.pinned-revision'
    if (-not (Test-Path -LiteralPath $pinFile -PathType Leaf)) { return $false }
    $recorded = [IO.File]::ReadAllText($pinFile).Trim().ToLowerInvariant()
    if ($recorded -ne $WptRevision) {
        throw "WPT cache '$CacheDir' is pinned to $recorded but this script pins $WptRevision. Delete the cache deliberately rather than mixing two suite revisions into one report."
    }
    foreach ($tree in $WptTrees) {
        if (-not (Test-Path -LiteralPath (Join-Path $CacheDir $tree) -PathType Container)) {
            return $false
        }
    }
    return $true
}

if (Test-CacheIsCurrent) {
    Write-Output "WPT cache already at $WptRevision ($CacheDir); nothing to do."
    return
}

if (Test-Path -LiteralPath $CacheDir -PathType Leaf) {
    throw "WPT cache path is a file: $CacheDir"
}

# A cache pinned to a different revision, or a half-extracted one, is removed
# rather than merged into. A half-extracted tree is how a denominator silently
# shrinks between two runs of the "same" suite.
if (Test-Path -LiteralPath $CacheDir -PathType Container) {
    $stale = $true
    $pinFile = Join-Path $CacheDir '.pinned-revision'
    if (Test-Path -LiteralPath $pinFile -PathType Leaf) {
        if ([IO.File]::ReadAllText($pinFile).Trim().ToLowerInvariant() -eq $WptRevision) { $stale = $false }
    }
    if ($stale) {
        Write-Output "Removing stale WPT cache at $CacheDir"
        Remove-Item -LiteralPath $CacheDir -Recurse -Force
    }
    else {
        # A cache whose *scope* changed - trees added or removed - must be rebuilt
        # even though the revision matches. `Test-CacheIsCurrent` only checks that
        # the expected trees are present, so a tree that was dropped from the list
        # is invisible to it and the stale copy would survive.
        foreach ($tree in $WptTrees) {
            if (Test-Path -LiteralPath (Join-Path (Join-Path $CacheDir $tree) $tree)) {
                Write-Output "Cache at $CacheDir has nested trees from a previous merge; rebuilding"
                $stale = $true
                break
            }
        }
        if ($stale) {
            Remove-Item -LiteralPath $CacheDir -Recurse -Force
        }
    }
}
New-Item -ItemType Directory -Path $CacheDir -Force | Out-Null
New-Item -ItemType Directory -Path $DownloadDir -Force | Out-Null

$archive = Join-Path $DownloadDir "wpt-$WptRevision.tar.gz"
$url = $WptArchiveBase + $WptRevision

Write-Output "Network required: fetching WPT $WptRevision from $url"
Write-Output "This is a one-time fetch of roughly 400 MB; it is cached and pinned afterwards."

if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
    $attempted = 0
    while ($true) {
        $attempted++
        try {
            # [Net.ServicePointManager] is required for TLS 1.2 on Windows
            # PowerShell 5.1 hosts; without it the download fails on modern
            # codeload endpoints.
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
            Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing -TimeoutSec 1800
            break
        } catch {
            if ($attempted -ge 3) {
                throw "could not download the WPT archive after $attempted attempts: $($_.Exception.Message)"
            }
            Write-Output ("  attempt {0} failed ({1}); retrying" -f $attempted, $_.Exception.Message)
            Start-Sleep -Seconds (5 * $attempted)
        }
    }
}

$archiveBytes = (Get-Item -LiteralPath $archive).Length
Write-Output ("  downloaded {0:N1} MB" -f ($archiveBytes / 1MB))
if ($archiveBytes -lt 1MB) {
    throw "WPT archive is implausibly small ($archiveBytes bytes); refusing to score against a truncated suite."
}

# The tarball's root directory is `wpt-<sha>`. Checking it means a substituted
# or mis-resolved archive fails here rather than producing numbers.
#
# The listing is read in full, once, and `$LASTEXITCODE` is checked against the
# *completed* read. Two reasons, both learned the hard way:
#   * `& tar ... | Select-Object -First 1` stops the pipe early, so bsdtar dies
#     on a broken pipe and $LASTEXITCODE comes back non-zero even when the
#     archive is perfect.
#   * A truncated download is the single most dangerous input to a conformance
#     run: it does not fail loudly, it silently shortens the denominator.
# A full listing is a complete decompress, so a clean exit is real evidence
# that every member is present.
$listing = & tar -tzf $archive
if ($LASTEXITCODE -ne 0) {
    throw "could not list the WPT archive (exit $LASTEXITCODE); it is truncated or corrupt. Delete $archive and re-run."
}
if ($listing.Count -lt 1000) {
    throw "WPT archive lists only $($listing.Count) members; refusing to score against a truncated suite."
}
$rootSegment = ($listing[0] -split '/')[0]
if ($rootSegment -ne "wpt-$WptRevision") {
    throw "WPT archive root is '$rootSegment', expected 'wpt-$WptRevision'. The archive does not match the pinned revision."
}
Write-Output ("  archive verified: {0} members, root {1}" -f $listing.Count, $rootSegment)

$staging = Join-Path $DownloadDir 'staging'
if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
New-Item -ItemType Directory -Path $staging -Force | Out-Null

Write-Output "  extracting only: $($WptTrees -join ', ')"
$patterns = @()
foreach ($tree in $WptTrees) { $patterns += "$rootSegment/$tree/*" }
# bsdtar treats bare arguments as member patterns, so this extracts four
# subtrees without materializing the rest of a multi-gigabyte suite.
& tar -xzf $archive -C $staging --no-same-owner --no-same-permissions @patterns 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { throw "tar extraction failed for $rootSegment" }

$extractedRoot = Join-Path $staging $rootSegment
foreach ($tree in $WptTrees) {
    $from = Join-Path $extractedRoot $tree
    if (-not (Test-Path -LiteralPath $from -PathType Container)) {
        throw "expected tree '$tree' is absent from the archive at the pinned revision"
    }
    $to = Join-Path $CacheDir $tree
    # Replace rather than merge. `Copy-Item -Recurse` onto an existing directory
    # copies *into* it, so a re-run against an existing cache silently nests
    # `css/css/`, `html/html/` and doubles every count in the census. That is
    # the worst kind of bug in a measurement tool: the suite still looks like a
    # suite, and every denominator is wrong by a factor of two.
    if (Test-Path -LiteralPath $to) {
        Remove-Item -LiteralPath $to -Recurse -Force
    }
    Copy-Item -LiteralPath $from -Destination $to -Recurse -Force
    if (Test-Path -LiteralPath (Join-Path $to $tree)) {
        throw "tree '$tree' was nested inside itself; the cache is corrupt. Remove $CacheDir and re-run."
    }
}

# Record the pin before deleting the archive, so an interrupted run leaves a
# cache that is either obviously stale or obviously complete.
Write-Utf8NoNewline -Path (Join-Path $CacheDir '.pinned-revision') -Text $WptRevision

Remove-Item -LiteralPath $staging -Recurse -Force
Remove-Item -LiteralPath $archive -Force

$counts = @()
foreach ($tree in @('html', 'css', 'dom')) {
    $n = @(Get-ChildItem -LiteralPath (Join-Path $CacheDir $tree) -Recurse -Filter '*.html' -File -ErrorAction SilentlyContinue).Count
    $counts += "$tree/ $n .html"
}

$totalBytes = (Get-ChildItem -LiteralPath $CacheDir -Recurse -File | Measure-Object -Property Length -Sum).Sum
Write-Output "WPT $WptRevision ready at $CacheDir ($([math]::Round($totalBytes/1MB,1)) MB on disk)"
Write-Output ("  {0}" -f ($counts -join ', '))

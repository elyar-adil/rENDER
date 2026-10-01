[CmdletBinding()]
param(
    # Where to place the cached suite. Defaults to tools/html5lib/.cache.
    [string] $Target
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# Pinned revisions.
#
# The tree-construction corpus has two homes and this script fetches both, each
# at an exact commit, because a conformance number is not comparable to anything
# - including to itself next month - without the revision it was measured on.
#
# 1. html5lib/html5lib-tests. This is the suite everyone means by "html5lib
#    tests". Its HEAD commit (224991ec10db04f056a89eed8b0bd8695fd2950e, 2026-06-26)
#    is titled "Tree construction tests have moved to WPT" and **deletes**
#    tree-construction/, tokenizer/, serializer/ and encoding/. Pinning HEAD
#    would fetch an empty corpus and report a vacuous 100%. The parent commit
#    below is the last revision that still contains the tree-construction tests.
#
# 2. web-platform-tests/wpt, html/syntax/parsing/resources. This is where the
#    tree-construction .dat files live now. It is the same revision this
#    repository already pins in tools/fetch-wpt.ps1, and it is a strict superset
#    of (1): four files the html5lib copy no longer carries
#    (processing-instructions.dat, scripted_adoption01.dat, scripted_ark.dat,
#    scripted_webkit01.dat) plus any edits to the shared files.
# ---------------------------------------------------------------------------
$Html5LibRevision = "9329e64694e7835d0dcff9811e22856ef6ad16f9"
$WptRevision = "c7fdee80f3f17b4e9813964916afdfd57ace863f"
$WptParsingPath = "html/syntax/parsing/resources"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if ([string]::IsNullOrWhiteSpace($Target)) {
    $Target = Join-Path $PSScriptRoot ".cache"
}
$Target = [IO.Path]::GetFullPath($Target)
if (-not (Test-Path -LiteralPath $Target)) {
    New-Item -ItemType Directory -Path $Target -Force | Out-Null
}

$staging = Join-Path ([IO.Path]::GetTempPath()) ("html5lib-fetch-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $staging -Force | Out-Null

try {
    # -- Source 1: html5lib-tests, tree-construction/ --------------------------
    $archive = Join-Path $staging "html5lib-tests.tar.gz"
    & curl.exe --fail --location --silent --show-error --retry 5 --retry-delay 1 `
        --retry-all-errors --max-time 600 `
        "https://codeload.github.com/html5lib/html5lib-tests/tar.gz/$Html5LibRevision" `
        --output $archive
    if ($LASTEXITCODE -ne 0) { throw "curl failed to download html5lib-tests $Html5LibRevision" }

    $unpacked = Join-Path $staging "html5lib-tests-$Html5LibRevision"
    & tar.exe -xzf $archive -C $staging "html5lib-tests-$Html5LibRevision/tree-construction"
    if ($LASTEXITCODE -ne 0) { throw "tar failed to extract tree-construction from the html5lib-tests archive" }

    $source = Join-Path $unpacked "tree-construction"
    $datCount = @(Get-ChildItem -LiteralPath $source -Filter *.dat -File).Count
    if ($datCount -lt 50) {
        throw "html5lib-tests $Html5LibRevision yielded only $datCount .dat files; expected the full tree-construction corpus"
    }
    Write-Output "html5lib-tests $Html5LibRevision : $datCount tree-construction .dat files"

    $html5libDir = Join-Path $Target "html5lib-tests-$Html5LibRevision"
    if (Test-Path -LiteralPath $html5libDir) { Remove-Item -LiteralPath $html5libDir -Recurse -Force }
    New-Item -ItemType Directory -Path $html5libDir -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination (Join-Path $html5libDir "tree-construction") -Recurse
    # Keep the licence and the format description next to the data.
    foreach ($extra in @("LICENSE", "AUTHORS.rst")) {
        $candidate = Join-Path $unpacked $extra
        if (Test-Path -LiteralPath $candidate) { Copy-Item -LiteralPath $candidate -Destination $html5libDir }
    }
    [IO.File]::WriteAllText((Join-Path $html5libDir ".render-revision"), $Html5LibRevision)

    # -- Source 2: WPT html/syntax/parsing/resources ---------------------------
    # Fetched as a repository tarball from codeload and extracted by path
    # prefix. The per-file alternative (raw.githubusercontent.com) is much
    # smaller, but that host is not reachable from every network this repository
    # is built on, and a fetch that half-works is worse than a slow one: the
    # script refuses to leave a partial suite behind.
    #
    # The tarball is about 100 MB and takes minutes; the corpus this runner reads
    # out of it is 61 small files. The extraction is what keeps the checked-out
    # result small, not the download.
    $wptArchive = Join-Path $staging "wpt.tar.gz"
    & curl.exe --fail --location --silent --show-error --retry 5 --retry-delay 2 `
        --retry-all-errors --max-time 1800 `
        "https://codeload.github.com/web-platform-tests/wpt/tar.gz/$WptRevision" `
        --output $wptArchive
    if ($LASTEXITCODE -ne 0) { throw "curl failed to download wpt $WptRevision" }

    $wptRoot = "wpt-$WptRevision"
    & tar.exe -xzf $wptArchive -C $staging "$wptRoot/$WptParsingPath"
    if ($LASTEXITCODE -ne 0) { throw "tar failed to extract $WptParsingPath from the wpt archive" }

    $extracted = Join-Path $staging "$wptRoot/$WptParsingPath"
    $datNames = @(Get-ChildItem -LiteralPath $extracted -Filter *.dat -File | ForEach-Object { $_.Name })
    if ($datNames.Count -lt 50) {
        throw "wpt $WptRevision yielded only $($datNames.Count) .dat files under $WptParsingPath"
    }
    $wptDir = Join-Path $Target "wpt-parsing-$WptRevision"
    if (Test-Path -LiteralPath $wptDir) { Remove-Item -LiteralPath $wptDir -Recurse -Force }
    New-Item -ItemType Directory -Path $wptDir -Force | Out-Null
    Copy-Item -LiteralPath $extracted -Destination (Join-Path $wptDir "resources") -Recurse
    Get-ChildItem -LiteralPath (Join-Path $wptDir "resources") -File |
        ForEach-Object { Move-Item -LiteralPath $_.FullName -Destination $wptDir -Force }
    Remove-Item -LiteralPath (Join-Path $wptDir "resources") -Recurse -Force
    Write-Output "wpt $WptRevision : $($datNames.Count) $WptParsingPath .dat files"
    [IO.File]::WriteAllText((Join-Path $wptDir ".render-revision"), $WptRevision)
}
finally {
    Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output ""
Write-Output "Suite cached under $Target. Run it with:"
Write-Output "  cargo run -p render-html --example html5lib"

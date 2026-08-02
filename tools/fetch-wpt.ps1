[CmdletBinding()]
param(
    [string] $Target
)

$ErrorActionPreference = "Stop"
$WptRevision = "c7fdee80f3f17b4e9813964916afdfd57ace863f"
$WptRemote = "https://github.com/web-platform-tests/wpt.git"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

if ([string]::IsNullOrWhiteSpace($Target)) {
    $Target = Join-Path (Split-Path $RepoRoot -Parent) "wpt"
}
$Target = [IO.Path]::GetFullPath($Target)

function Invoke-WptGit {
    param([string[]] $GitArguments)

    & git -C $Target @GitArguments
    if ($LASTEXITCODE -ne 0) {
        throw "git failed in '$Target': git $($GitArguments -join ' ')"
    }
}

if (Test-Path -LiteralPath $Target -PathType Leaf) {
    throw "WPT target is a file: $Target"
}

if (-not (Test-Path -LiteralPath $Target)) {
    New-Item -ItemType Directory -Path $Target -Force | Out-Null
}

$gitDir = Join-Path $Target ".git"
$head = $null
if (Test-Path -LiteralPath $gitDir) {
    $head = (& git -C $Target rev-parse --verify HEAD 2>$null | Select-Object -First 1)
}
if ($null -ne $head -and $LASTEXITCODE -eq 0) {
    $actual = $head.Trim().ToLowerInvariant()
    if ($actual -ne $WptRevision) {
        throw "Existing WPT checkout '$Target' is at $actual, expected $WptRevision. Choose another -Target or remove it deliberately."
    }
    $remote = (& git -C $Target remote get-url origin 2>$null | Select-Object -First 1)
    if ($LASTEXITCODE -ne 0 -or $remote.Trim() -ne $WptRemote) {
        throw "WPT checkout '$Target' has no expected origin '$WptRemote'."
    }
    $dirty = (& git -C $Target status --porcelain --untracked-files=all)
    if ($LASTEXITCODE -ne 0) {
        throw "cannot inspect WPT checkout '$Target' status"
    }
    if (-not [string]::IsNullOrWhiteSpace(($dirty -join "`n"))) {
        throw "WPT checkout '$Target' is dirty; clean it or choose another -Target."
    }
    Invoke-WptGit @("sparse-checkout", "disable")
    Set-Content -LiteralPath (Join-Path $Target ".render-revision") -Value $WptRevision -NoNewline -Encoding ascii
    Write-Output "WPT checkout already matches $WptRevision at $Target"
    exit 0
}

$existingEntries = @(Get-ChildItem -LiteralPath $Target -Force)
if ($existingEntries.Count -gt 0 -and -not (Test-Path -LiteralPath (Join-Path $Target ".git"))) {
    throw "WPT target '$Target' is not empty and is not a Git checkout. Choose another -Target."
}

if (-not (Test-Path -LiteralPath $gitDir)) {
    Invoke-WptGit @("init")
    Invoke-WptGit @("remote", "add", "origin", $WptRemote)
} else {
    $remote = (& git -C $Target remote get-url origin 2>$null | Select-Object -First 1)
    if ($LASTEXITCODE -ne 0 -or $remote.Trim() -ne $WptRemote) {
        throw "WPT checkout '$Target' has no expected origin '$WptRemote'."
    }
}

Invoke-WptGit @("fetch", "--depth", "1", "origin", $WptRevision)
Invoke-WptGit @("sparse-checkout", "disable")
Invoke-WptGit @("checkout", "--detach", $WptRevision)

$actual = (& git -C $Target rev-parse --verify HEAD).Trim().ToLowerInvariant()
if ($actual -ne $WptRevision) {
    throw "WPT checkout verification failed: got $actual, expected $WptRevision"
}
Set-Content -LiteralPath (Join-Path $Target ".render-revision") -Value $WptRevision -NoNewline -Encoding ascii
Write-Output "Fetched full official WPT revision $WptRevision into external checkout $Target"

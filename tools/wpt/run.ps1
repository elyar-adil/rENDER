<#
.SYNOPSIS
    Fetches (if needed) and runs the WPT conformance runner. One command.

.DESCRIPTION
    The single entry point for the rENDER WPT conformance work, in the order
    that makes the result trustworthy rather than merely fast:

      1. Run the harness self-check FIRST, and stop if it did not hold. A runner
         that cannot demonstrate its own correctness must not produce a number,
         and the number is the only thing this project would quote. Doing this
         first also means finding out before a 100 MB download.
      2. Fetch the pinned WPT revision if the cache is absent. Needs network on
         first run; later runs are fully offline.
      3. Census the suite to establish the denominator, then execute what is
         executable, then write results as data.

    Every percentage in the output carries its denominator. A run that evaluated
    nothing prints no percentage at all - not 0%.

    Only Windows PowerShell 5.1 is installed on this host; there is no `pwsh`.
    The script therefore avoids PowerShell 7 syntax, notably the ternary
    operator, which is a parse error here.

.PARAMETER Areas
    Comma-separated areas. Default: css,dom,html

.PARAMETER Limit
    Execute at most N tests per area. A partial run is labelled PARTIAL in the
    output and is never presented as a conformance rate.

.PARAMETER Census
    Census only: population, harness shapes and feasibility, no engine. This is
    the half of the work that does not depend on the engine being healthy, and
    therefore the half that can always be trusted.

.PARAMETER Probe
    Ask one question: can the engine load WPT's own testharness.js? Measured,
    not argued. Run this before trusting any conformance figure, because a
    runner that cannot drive the harness cannot produce a real one.

.PARAMETER NegativeControl
    Run the pipeline against adapters that must fail. A control that only
    exists as a test is a control nobody runs, and the failure it guards
    against - a 100% rate that looks healthy - survives review.

.PARAMETER Fixtures
    Account for every missing fixture by the WPT tree that would supply it.
    Answers "which trees do I add to fetch-wpt.ps1", with counts.

.PARAMETER SkipSelfTest
    Do not gate on the harness self-check. Exists for debugging the runner
    itself. Nothing that produces a reported figure should use it.

.EXAMPLE
    powershell -File tools/wpt/run.ps1
    powershell -File tools/wpt/run.ps1 -Areas dom -Probe
    powershell -File tools/wpt/run.ps1 -NegativeControl -Limit 300
    powershell -File tools/wpt/run.ps1 -Fixtures
    powershell -File tools/wpt/run.ps1 -Areas css -Census
#>
[CmdletBinding()]
param(
    [string] $Areas = 'css,dom,html',
    [int] $Limit = 0,
    [switch] $Census,
    [switch] $Probe,
    [switch] $NegativeControl,
    [switch] $Fixtures,
    [switch] $SkipSelfTest
)

$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$RunnerManifest = Join-Path $RepoRoot 'tests\wpt_runner\Cargo.toml'
$SuiteRoot = Join-Path $PSScriptRoot '.cache\wpt'
$OutDir = Join-Path $PSScriptRoot 'results'

if (-not (Test-Path -LiteralPath $RunnerManifest)) {
    Write-Error "runner manifest not found at $RunnerManifest"
    exit 2
}

if (-not $SkipSelfTest) {
    Write-Host '== harness self-check (gating: nothing is reported unless this holds)' -ForegroundColor Cyan
    # Deliberately built WITHOUT the engine feature. The self-check proves this
    # runner's own four-state logic against a stub, so depending on the engine
    # would make the gate weaker: an engine that fails to compile would take the
    # gate down with it, and the gate is the one thing that must keep working.
    & cargo run --release --quiet --manifest-path $RunnerManifest -- selftest
    if ($LASTEXITCODE -ne 0) {
        Write-Host ''
        Write-Error 'HARNESS SELF-CHECK FAILED. Refusing to report any result. See above.'
        exit 1
    }
}

if (-not (Test-Path -LiteralPath (Join-Path $SuiteRoot '.pinned-revision'))) {
    Write-Host ''
    Write-Host '== fetching the pinned suite (needs network on first run)' -ForegroundColor Cyan
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'fetch-wpt.ps1') -CacheDir $SuiteRoot
    if ($LASTEXITCODE -ne 0) {
        Write-Error 'fetch failed; see above'
        exit 1
    }
}
else {
    Write-Host ''
    Write-Host "== suite cache present at $SuiteRoot" -ForegroundColor Cyan
}

# PowerShell 5.1 has no ternary operator, so this is an if/else chain and not
# `?:`. Exactly one mode is honoured; the last matching switch wins, which is
# why the ordering below puts the narrowest first.
$subcommand = 'run'
if ($Census) { $subcommand = 'census' }
if ($Probe) { $subcommand = 'probe' }
if ($NegativeControl) { $subcommand = 'negative-control' }
if ($Fixtures) { $subcommand = 'fixtures' }

# The engine adapter is only built for the modes that need it. `census`,
# `selftest`, `probe` and `fixtures` all work against `std` alone, which is what
# keeps the trustworthy half of the work running when `render-js` is mid-edit by
# another agent - a situation this tree is in most days.
$features = @()
if ($subcommand -eq 'run' -or $subcommand -eq 'negative-control' -or $subcommand -eq 'probe') {
    $features = @('--features', 'engine')
}

$runArgs = @('run', '--release', '--quiet', '--manifest-path', $RunnerManifest) + $features +
    @('--', $subcommand, '--areas', $Areas, '--out', $OutDir)
if ($Limit -gt 0) {
    $runArgs += @('--limit', $Limit, '--allow-partial')
}

Write-Host ''
Write-Host "== running the suite (areas: $Areas)" -ForegroundColor Cyan
& cargo @runArgs
$runExit = $LASTEXITCODE

Write-Host ''
Write-Host '== results' -ForegroundColor Cyan
$summary = Join-Path $OutDir 'wpt-results.json'
$detail = Join-Path $OutDir 'wpt-results.jsonl'
if (Test-Path -LiteralPath $summary) {
    Write-Host "  $summary"
    Write-Host "  $detail"
    Write-Host ''
    Write-Host '  Read the figure out of the "population", "attempted" and "denominator_note"'
    Write-Host '  fields. There is no bare percentage in this output, by construction.'
}
else {
    Write-Warning "no results file was written to $OutDir"
}

Write-Host ''
if ($runExit -eq 0) {
    Write-Host 'A non-zero exit means test failures were recorded. Harness errors are reported' -ForegroundColor Yellow
    Write-Host 'but do NOT fail the command: they are not evidence about the engine.' -ForegroundColor Yellow
}
exit $runExit

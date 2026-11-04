<#
.SYNOPSIS
  Local pre-commit validation of KON's GitHub Actions workflows.  Run:  powershell -File tools\validate-ci.ps1

.DESCRIPTION
  1. YAML syntax + style  : yamllint (preset "relaxed", plus the one GitHub-Actions-specific `on:` exception).
  2. Workflow semantics   : actionlint (schema, expressions, runner labels, action inputs, needs/matrix, ...).
  3. Job simulation       : `act`, only if both `act` and a running Docker engine exist (otherwise reported as SKIPPED).

  Tools are bootstrapped once into %LOCALAPPDATA%\kon-ci-tools (a Python venv for yamllint, `go install` for
  actionlint). Nothing is installed globally and nothing in the repository is modified.
  Exit code: 0 = all executed checks passed, 1 = a check failed, 2 = a tool could not be set up.

  Not reproducible locally (see tools\validate-ci.md): GitHub-hosted runners, macOS runners, secrets/permissions,
  marketplace-action execution fidelity, the 1.1 GB corpus cache, and actionlint's shellcheck/pyflakes extras.
#>
param([switch]$Simulate)   # -Simulate: also try `act` (needs Docker); off by default because it is slow.

$ErrorActionPreference = 'Stop'
$repo  = Resolve-Path (Join-Path $PSScriptRoot '..')
$tools = Join-Path $env:LOCALAPPDATA 'kon-ci-tools'
$venv  = Join-Path $tools 'venv'
$bin   = Join-Path $tools 'bin'
New-Item -ItemType Directory -Force $tools, $bin | Out-Null

# ---- bootstrap ------------------------------------------------------------------------------------------
$yamllint = Join-Path $venv 'Scripts\yamllint.exe'
if (-not (Test-Path $yamllint)) {
    Write-Host 'Setting up yamllint (one-time)...'
    python -m venv $venv;                         if ($LASTEXITCODE) { Write-Error 'python venv failed'; exit 2 }
    & "$venv\Scripts\python.exe" -m pip install --quiet yamllint
    if ($LASTEXITCODE) { Write-Error 'pip install yamllint failed'; exit 2 }
}
$actionlint = Join-Path $bin 'actionlint.exe'
if (-not (Test-Path $actionlint)) {
    Write-Host 'Setting up actionlint (one-time, via go install)...'
    $env:GOBIN = $bin
    go install github.com/rhysd/actionlint/cmd/actionlint@latest
    if ($LASTEXITCODE -or -not (Test-Path $actionlint)) { Write-Error 'go install actionlint failed'; exit 2 }
}

# ---- discover -------------------------------------------------------------------------------------------
$files = Get-ChildItem -Path $repo -Recurse -Include *.yml, *.yaml -File |
    Where-Object { $_.FullName -notmatch '\\(node_modules|target|\.git|dist)\\' }
$workflows = $files | Where-Object { $_.FullName -match '\\\.github\\workflows\\' }
Write-Host "YAML files: $($files.Count)   workflows: $($workflows.Count)"
$files | ForEach-Object { Write-Host "  $($_.FullName.Substring($repo.Path.Length + 1))" }

$yamlCfg = "{extends: relaxed, rules: {truthy: {check-keys: false}}}"   # `on:` is a valid GH Actions key
$fail = $false
$report = [ordered]@{}

# ---- 1. yamllint ----------------------------------------------------------------------------------------
Write-Host "`n== yamllint ($(& $yamllint --version))"
& $yamllint -d $yamlCfg ($files.FullName)
$report['yamllint'] = if ($LASTEXITCODE -eq 0) { 'PASS' } else { $fail = $true; 'FAIL' }

# ---- 2. actionlint --------------------------------------------------------------------------------------
Write-Host "`n== actionlint ($((& $actionlint -version)[0]))"
if ($workflows) {
    Push-Location $repo
    try { & $actionlint -verbose ($workflows.FullName) } finally { Pop-Location }
    $report['actionlint'] = if ($LASTEXITCODE -eq 0) { 'PASS' } else { $fail = $true; 'FAIL' }
} else { $report['actionlint'] = 'SKIPPED (no workflows)' }

# ---- 3. act (optional) ----------------------------------------------------------------------------------
Write-Host "`n== act (job simulation)"
$act = Get-Command act -ErrorAction SilentlyContinue
$docker = $false
if (Get-Command docker -ErrorAction SilentlyContinue) { docker info *> $null; $docker = ($LASTEXITCODE -eq 0) }
if (-not $Simulate)        { $report['act'] = 'SKIPPED (run with -Simulate)' }
elseif (-not $act)         { $report['act'] = 'SKIPPED (act not installed)' }
elseif (-not $docker)      { $report['act'] = 'SKIPPED (no running Docker engine)' }
else {
    Push-Location $repo
    try { act -l; act --dryrun } finally { Pop-Location }
    $report['act'] = if ($LASTEXITCODE -eq 0) { 'PASS (dry run only; not GitHub-runner equivalent)' } else { $fail = $true; 'FAIL' }
}
Write-Host $report['act']

Write-Host "`n== Summary"
$report.GetEnumerator() | ForEach-Object { Write-Host ('  {0,-11} {1}' -f $_.Key, $_.Value) }
if ($fail) { Write-Host 'RESULT: FAILED'; exit 1 } else { Write-Host 'RESULT: OK (executed checks passed; see tools\validate-ci.md for what is not covered)'; exit 0 }

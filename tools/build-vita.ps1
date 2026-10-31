param(
    [ValidateSet('platform', 'gxm', 'image', 'effects', 'audio', 'pipeline', 'text', 'audio-mixer', 'game', 'movie', 'compressed', 'ui-color')]
    [string]$Mode = 'game',
    [ValidateSet('dev', 'release')][string]$BuildKind = 'release',
    [string]$Distro = '',
    [string]$Sdk = '',
    [switch]$Check,
    [switch]$Setup
)
$ErrorActionPreference = 'Stop'
if ($Check -and $Setup) { throw '-Check and -Setup are mutually exclusive' }
$launcher = Join-Path $PSScriptRoot 'build-vita.py'
$python = $null
$pythonPrefix = @()
foreach ($candidate in @('py', 'python3', 'python')) {
    $command = Get-Command $candidate -ErrorAction SilentlyContinue
    if (-not $command) { continue }
    $prefix = @()
    if ($candidate -eq 'py') { $prefix = @('-3') }
    try {
        & $command.Source @prefix -c 'import sys; sys.exit(0 if sys.version_info >= (3,9) else 1)' 2>$null
    } catch { continue }
    if ($LASTEXITCODE -eq 0) {
        $python = $command.Source
        $pythonPrefix = $prefix
        break
    }
}
if (-not $python) { throw 'Python 3.9+ is required (py -3, python3, or python on PATH)' }
$forward = @($launcher, $Mode, $BuildKind)
if ($Distro) { $forward += @('--wsl', $Distro) }
if ($Sdk) { $forward += @('--sdk', $Sdk) }
if ($Check) { $forward += '--check' }
if ($Setup) { $forward += '--setup' }
& $python @pythonPrefix @forward
exit $LASTEXITCODE

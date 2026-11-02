param(
    [switch]$PrepareAssets,
    [switch]$DebugTools,
    [string]$SourceAssets,
    [string]$Ffmpeg = '',
    [string]$Distro = '',
    [ValidateRange(1, 16)][int]$Jobs = 6
)

$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
$wslArgs = @()
if ($Distro) { $wslArgs += @('-d', $Distro) }
Push-Location -LiteralPath $taskRoot
try {
    if ($PrepareAssets) {
        if (-not $Ffmpeg) {
            $command = Get-Command ffmpeg -CommandType Application -ErrorAction SilentlyContinue
            if (-not $command) { throw 'ffmpeg was not found. Add it to PATH or pass -Ffmpeg with its executable path.' }
            $Ffmpeg = $command.Source
        } elseif (-not (Test-Path -LiteralPath $Ffmpeg -PathType Leaf)) {
            $command = Get-Command $Ffmpeg -CommandType Application -ErrorAction SilentlyContinue
            if (-not $command) { throw "ffmpeg was not found: $Ffmpeg" }
            $Ffmpeg = $command.Source
        }
        if (-not $SourceAssets) {
            $SourceAssets = Join-Path $taskRoot 'dist/Witch-on-the-Holy-Night-Chengkong-540p-x64/assets'
        }
        if (-not (Test-Path -LiteralPath $SourceAssets -PathType Container)) {
            throw 'Original conversion inputs are not retained in the final Vita project. Use -SourceAssets with separately prepared desktop inputs. To rebuild only the EXE, omit -PrepareAssets. For the current Vita assets, use python tools/generate_vita_asset_profile_540.py --check.'
        }
    }
    $buildArgs = $wslArgs + @('--', 'bash', './tools/build-compact.sh')
    if ($DebugTools) { $buildArgs += '--debug-tools' }
    & wsl.exe @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'Compact Windows engine build failed' }
    if ($PrepareAssets) {
        & wsl.exe @wslArgs -- bash ./tools/build-compact.sh --kdu
        if ($LASTEXITCODE -ne 0) { throw 'Compact texture / audio tool build failed' }
        $assetArgs = @('-u', 'tools/create_compact.py', '--jobs', $Jobs, '--ffmpeg', $Ffmpeg)
        if ($SourceAssets) { $assetArgs += @('--source', $SourceAssets) }
        & python @assetArgs
        if ($LASTEXITCODE -ne 0) { throw 'Compact asset conversion failed' }
        $output = Join-Path $taskRoot 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64'
        New-Item -ItemType Directory -Force -Path (Join-Path $taskRoot '.work') | Out-Null
        & ./tools/kdu-compact.exe --romfs (Join-Path $output 'assets') verify-streams .work/compact-audio-decoded.tsv
        if ($LASTEXITCODE -ne 0) { throw 'Ogg full decode validation failed' }
        $checkArgs = @('-u', 'tools/verify_compact_assets.py', '--ffmpeg', $Ffmpeg)
        if ($SourceAssets) { $checkArgs += @('--source', $SourceAssets) }
        & python @checkArgs
        if ($LASTEXITCODE -ne 0) { throw 'Compact asset validation failed' }
    }
} finally {
    Pop-Location
}

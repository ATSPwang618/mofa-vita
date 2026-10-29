param(
    [switch]$PrepareAssets,
    [switch]$DebugTools,
    [string]$SourceAssets,
    [string]$Ffmpeg = 'C:/Program Files/ffmpeg/bin/ffmpeg.exe',
    [string]$Distro = 'pocket-ubuntu',
    [ValidateRange(1, 16)][int]$Jobs = 6
)

$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $taskRoot
try {
    if ($PrepareAssets) {
        if (-not $SourceAssets) {
            $SourceAssets = Join-Path $taskRoot 'dist/Witch-on-the-Holy-Night-Chengkong-540p-x64/assets'
        }
        if (-not (Test-Path -LiteralPath $SourceAssets -PathType Container)) {
            throw 'Original conversion inputs were removed from the final 540p project. Use -SourceAssets with a separately prepared input directory to regenerate assets. To rebuild only the EXE, omit -PrepareAssets. To verify retained files, run python tools/verify_current_compact.py.'
        }
    }
    $buildArgs = @('-d', $Distro, '--', 'bash', './tools/build-compact.sh')
    if ($DebugTools) { $buildArgs += '--debug-tools' }
    & wsl @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'Compact Windows engine build failed' }
    if ($PrepareAssets) {
        & wsl -d $Distro -- bash ./tools/build-compact.sh --kdu
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

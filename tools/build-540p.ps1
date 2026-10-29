param(
    [switch]$PrepareAssets,
    [switch]$DebugTools,
    [string]$SourceAssets,
    [string]$ModRoot,
    [ValidateRange(1, 32)][int]$Jobs = 8
)

$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $taskRoot
try {
    $buildArgs = @('-d', 'pocket-ubuntu', '--', 'bash', './tools/build-windows.sh')
    if ($DebugTools) { $buildArgs += '--debug-tools' }
    & wsl @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'Windows engine build failed' }
    if ($PrepareAssets) {
        & wsl -d pocket-ubuntu -- bash ./tools/build-windows.sh --kdu
        if ($LASTEXITCODE -ne 0) { throw 'Texture converter build failed' }
        $assetArgs = @('-u', 'tools/create_540p.py', '--jobs', $Jobs)
        if ($SourceAssets) { $assetArgs += @('--source', $SourceAssets) }
        if ($ModRoot) { $assetArgs += @('--mod', $ModRoot) }
        & python @assetArgs
        if ($LASTEXITCODE -ne 0) { throw '540p asset conversion failed' }
        $checkArgs = @('-u', 'tools/verify_540p_assets.py')
        if ($SourceAssets) { $checkArgs += @('--source', $SourceAssets) }
        & python @checkArgs
        if ($LASTEXITCODE -ne 0) { throw '540p asset verification failed' }
    }
} finally {
    Pop-Location
}

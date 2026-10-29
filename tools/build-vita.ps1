param([string]$Distro = 'pocket-ubuntu', [ValidateSet('platform', 'gxm', 'image', 'effects', 'audio', 'pipeline', 'text', 'audio-mixer', 'game', 'movie', 'compressed', 'ui-color')][string]$Mode = 'platform')
$ErrorActionPreference = 'Stop'
$scriptPath = (Join-Path $PSScriptRoot 'build-vita.sh').Replace('\', '/')
$resolved = & wsl -d $Distro -- wslpath -u $scriptPath
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve build script path in WSL' }
$wslPath = $resolved.Trim()
& wsl -d $Distro -- env PATH=/root/.cargo/bin:/opt/vitasdk/bin:/usr/local/bin:/usr/bin:/bin bash $wslPath $Mode
if ($LASTEXITCODE -ne 0) { throw "Vita build failed: exit $LASTEXITCODE" }

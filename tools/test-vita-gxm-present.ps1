[CmdletBinding()]
param([string]$Distro = 'pocket-ubuntu')
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$projectPosix = (& wsl.exe -d $Distro -- wslpath -u $projectRoot.Replace('\', '/')).Trim()
if ($LASTEXITCODE -ne 0 -or !$projectPosix.StartsWith('/')) { throw 'WSL path resolution failed' }
& wsl.exe -d $Distro -- env PATH=/opt/vitasdk/bin:/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin `
    bash "$projectPosix/tools/test-vita-gxm-present.sh"
if ($LASTEXITCODE -ne 0) {
    throw 'Host GXM present regression failed; inspect doc/logs/vita/gxm-present-host.log'
}
Write-Output "Verified host submission states; receipt: $(Join-Path $projectRoot 'doc/vita-gxm-present-host-tests.json')"

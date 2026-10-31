[CmdletBinding()]
param([string]$Distro = $env:KNIFE_WSL_DISTRO)
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$wslArgs = @()
if ($Distro) { $wslArgs = @('-d', $Distro) }
$projectPosix = (& wsl.exe @wslArgs -- wslpath -u $projectRoot.Replace('\', '/')).Trim()
if ($LASTEXITCODE -ne 0 -or !$projectPosix.StartsWith('/')) { throw 'WSL path resolution failed' }
& wsl.exe @wslArgs -- `
    bash "$projectPosix/tools/test-vita-gxm-present.sh"
if ($LASTEXITCODE -ne 0) {
    throw 'Host GXM present regression failed; inspect doc/logs/vita/gxm-present-host.log'
}
Write-Output "Verified host submission states; receipt: $(Join-Path $projectRoot 'doc/vita-gxm-present-host-tests.json')"

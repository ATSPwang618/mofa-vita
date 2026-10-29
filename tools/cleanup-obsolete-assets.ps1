[CmdletBinding(SupportsShouldProcess)]
param([switch]$Apply)

# Default is a read-only preview. -Apply is intended for manual use by the owner.
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$receiptPath = Join-Path $projectRoot 'doc/compact-storage-cleanup.json'
$receipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding utf8 | ConvertFrom-Json
$runtime = Join-Path $projectRoot 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64'
$protected = @($runtime, (Join-Path $projectRoot 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-image-source'))
$expectedEngine = (Get-Content -LiteralPath (Join-Path $projectRoot 'doc/compact-verification.json') -Raw -Encoding utf8 | ConvertFrom-Json).production_executable.sha256
if ((Get-FileHash -LiteralPath (Join-Path $runtime 'knife.exe') -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedEngine) { throw 'Current engine identity changed; inspect before cleanup.' }
foreach ($save in $receipt.save_backups) {
    $backup = [IO.Path]::GetFullPath($save.backup)
    if (-not $backup.StartsWith($runtime + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Backup is outside the current runtime.' }
    if ((Get-FileHash -LiteralPath $backup -Algorithm SHA256).Hash.ToLowerInvariant() -ne $save.sha256) { throw 'Save backup checksum failed.' }
}
$targets = @()
$records = @($receipt.selected)
foreach ($extra in @('dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-project-094-staging.zip', '.work/package-094-final-review')) {
    if ($records.path -notcontains $extra) { $records += [pscustomobject]@{ path=$extra; bytes=0; files=0 } }
}
foreach ($record in $records) {
    $target = [IO.Path]::GetFullPath((Join-Path $projectRoot $record.path))
    if (-not $target.StartsWith($projectRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Target leaves the project.' }
    if ($protected | Where-Object { $target.Equals($_, [StringComparison]::OrdinalIgnoreCase) -or $target.StartsWith($_ + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) }) { throw 'Protected final 540p files selected.' }
    $rel = $record.path.Replace('\', '/')
    $allowed = $rel -match '^dist/Witch-on-the-Holy-Night-(Windows-x64|Chengkong-540p-x64|Chengkong-540p-project\.zip|Chengkong-540p-Compact-project-094-staging\.zip)$' -or
               $rel -match '^\.work/(nca|portable-bntx|compact-package-review|package-review|compact-pilot|compact-extract|control-1\.02|pilot-540p|package-094-review|package-094-final-review|540p-assets-validation-partial\.json|build-compact\.sh|red-artifacts\.jsonl|test-red\.sh)$' -or
               $rel -match '^\.work/qa/(540p|boot-save|interactive-save|scenario-save|knife-540p-qa\.exe|knife-qa\.exe|red-core\.exe|interactive-process\.json|active-case\.json|tests/[^/]+\.exe)$' -or
               $rel -match '^doc/logs/(pe-(540p[^/]*|compact---[^/]*|kdu|knife[^/]*)\.txt|build-compact---[^/]*\.log)$' -or
               $rel -match '^doc/screenshots/(540p|backlog|boot|config|load|story|title)$'
    if (-not $allowed) { throw "Unapproved target: $rel" }
    if ($rel.StartsWith('.work/qa/tests/')) {
        $keep = Get-Content -LiteralPath (Join-Path $projectRoot '.work/qa/tests/manifest.json') -Raw -Encoding utf8 | ConvertFrom-Json
        if ($keep.exe -contains [IO.Path]::GetFileName($target)) { throw 'A current test executable was selected.' }
    }
    if (Test-Path -LiteralPath $target) {
        $item = Get-Item -LiteralPath $target -Force
        $nodes = @($item)
        if ($item.PSIsContainer) { $nodes += @(Get-ChildItem -LiteralPath $target -Force -Recurse) }
        if ($nodes | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }) { throw "Reparse point found: $target" }
        $regularFiles = @($nodes | Where-Object { -not $_.PSIsContainer })
        $actualBytes = ($regularFiles | Measure-Object -Property Length -Sum).Sum
        $targets += [pscustomobject]@{ Path=$record.path; Absolute=$target; Bytes=[long]$actualBytes; Files=$regularFiles.Count }
    }
}
$targets | Select-Object Path,Bytes,Files | Format-Table -AutoSize
Write-Host 'Logical sizes include hard links and are not a promise of freed disk space.'
if (-not $Apply) { Write-Host 'Preview only. Run this script manually with -Apply after reviewing the paths.'; return }
if ($WhatIfPreference) { foreach ($target in $targets) { [void]$PSCmdlet.ShouldProcess($target.Absolute, 'Remove obsolete project files') }; return }
$driveName = [IO.Path]::GetPathRoot($projectRoot).TrimEnd('\').TrimEnd(':')
$freeBefore = (Get-PSDrive -Name $driveName).Free
$removed = @()
foreach ($target in $targets) {
    if (-not $PSCmdlet.ShouldProcess($target.Absolute, 'Remove obsolete project files')) { throw 'Cleanup was skipped; completion is not recorded.' }
    Remove-Item -LiteralPath $target.Absolute -Recurse -Force
    if (Test-Path -LiteralPath $target.Absolute) { throw "Removal did not finish: $($target.Absolute)" }
    $removed += $target.Path
}
Push-Location -LiteralPath $projectRoot
try {
    python -X utf8 tools/verify_current_compact.py
    if ($LASTEXITCODE -ne 0) { throw 'Post-cleanup full asset verification failed.' }
    python -X utf8 tools/verify_windows.py --variant compact --production-smoke
    if ($LASTEXITCODE -ne 0) { throw 'Post-cleanup runtime smoke check failed.' }
} finally { Pop-Location }
if ((Get-FileHash -LiteralPath $receipt.current_personal_save.path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $receipt.current_personal_save.sha256) { throw 'Personal save changed; inspect before finalizing.' }
$receipt.status = 'completed_manually'
$receipt.complete = $true
$receipt.removed = $removed
$receipt.manual_cleanup_executed = $true
$receipt | Add-Member -NotePropertyName manual_completed_at -NotePropertyValue ([DateTimeOffset]::Now.ToString('o')) -Force
$receipt | Add-Member -NotePropertyName manual_disk_free_before -NotePropertyValue $freeBefore -Force
$receipt | Add-Member -NotePropertyName manual_disk_free_after -NotePropertyValue ((Get-PSDrive -Name $driveName).Free) -Force
$receipt | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $receiptPath -Encoding utf8
Write-Host 'Selected obsolete files removed; retained assets, engine and personal save verified.'

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
Push-Location $projectRoot
try {
    & cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    $metadataText = & cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Dependency metadata failed' }
    $metadata = $metadataText | ConvertFrom-Json
    $distPath = Join-Path $projectRoot 'dist'
    New-Item -ItemType Directory -Path $distPath -Force | Out-Null
    Copy-Item -LiteralPath 'target\release\lite-recorder.exe' -Destination $distPath
    # The archive contains first-run instructions, never paths from the build machine.
    # The EXE generates the full command reference on the destination desktop.
    $firstRunText = [System.IO.File]::ReadAllText((Join-Path $projectRoot 'README.txt'))
    [System.IO.File]::WriteAllText((Join-Path $distPath 'README.txt'), $firstRunText, [System.Text.UTF8Encoding]::new($true))
    $notices = [System.Text.StringBuilder]::new()
    [void]$notices.AppendLine('LiteRecorder third-party software notices')
    foreach ($package in ($metadata.packages | Where-Object { $_.name -ne 'lite-recorder' } | Sort-Object name)) {
        [void]$notices.AppendLine("`n=== $($package.name) $($package.version) ===")
        [void]$notices.AppendLine("License: $($package.license)")
        [void]$notices.AppendLine("Repository: $($package.repository)")
        $crateRoot = Split-Path $package.manifest_path -Parent
        $licenses = Get-ChildItem -LiteralPath $crateRoot -File | Where-Object { $_.Name -match '^(LICENSE|COPYING|COPYRIGHT|NOTICE)' }
        foreach ($licenseFile in $licenses) {
            [void]$notices.AppendLine("`n--- $($licenseFile.Name) ---")
            [void]$notices.AppendLine([System.IO.File]::ReadAllText($licenseFile.FullName))
        }
    }
    [System.IO.File]::WriteAllText((Join-Path $distPath 'THIRD-PARTY-NOTICES.txt'), $notices.ToString(), [System.Text.UTF8Encoding]::new($false))
    $exeHash = (Get-FileHash -LiteralPath (Join-Path $distPath 'lite-recorder.exe') -Algorithm SHA256).Hash
    [System.IO.File]::WriteAllText((Join-Path $distPath 'SHA256.txt'), "$exeHash  lite-recorder.exe`n")
    Compress-Archive -LiteralPath @((Join-Path $distPath 'lite-recorder.exe'), (Join-Path $distPath 'README.txt'), (Join-Path $distPath 'THIRD-PARTY-NOTICES.txt'), (Join-Path $distPath 'SHA256.txt')) -DestinationPath (Join-Path $distPath 'LiteRecorder-windows-x64.zip') -Force
    Get-Item -LiteralPath (Join-Path $distPath 'lite-recorder.exe'), (Join-Path $distPath 'LiteRecorder-windows-x64.zip') | Select-Object Name, Length
}
finally { Pop-Location }

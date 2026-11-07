# Windows packages: installer (Inno Setup) and portable zip.
#
#   cargo build --release
#   pwsh packaging/windows/package.ps1 -Version v4.0.0 -OutDir dist
#
# Run from the repository root. Inno Setup 6 is needed for the installer
# (`choco install innosetup`); without it only the portable zip is made.
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$OutDir = "dist"
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")

$bin = Resolve-Path "target\release"
foreach ($exe in "lunii.exe", "lunii-cli.exe") {
    if (-not (Test-Path (Join-Path $bin $exe))) { throw "target\release\$exe missing: cargo build --release" }
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$out = Resolve-Path $OutDir

# --- portable zip ------------------------------------------------------------
$stage = Join-Path ([System.IO.Path]::GetTempPath()) "lunii-portable-$PID"
$dir = Join-Path $stage "Lunii"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item (Join-Path $bin "lunii.exe"), (Join-Path $bin "lunii-cli.exe") $dir
Copy-Item "LICENSE" (Join-Path $dir "LICENSE.txt")
$zip = Join-Path $out "Lunii-$Version-win64_portable.zip"
Compress-Archive -Path $dir -DestinationPath $zip -CompressionLevel Optimal -Force
Remove-Item -Recurse -Force $stage
Write-Output $zip

# --- installer ---------------------------------------------------------------
$iscc = @(
    (Get-Command "ISCC.exe" -ErrorAction SilentlyContinue).Source,
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) {
    Write-Warning "Inno Setup not found: installer skipped"
    exit 0
}
& $iscc /Q "/DAppVersion=$Version" "/DSourceDir=$bin" "/DOutputDir=$out" "packaging\windows\installer.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
Write-Output (Join-Path $out "Lunii-$Version-win64_setup.exe")

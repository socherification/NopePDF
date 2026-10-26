# Downloads the pinned PDFium build (see pdfium.lock) into vendor\pdfium and
# verifies its checksum. Run once after cloning; build.ps1 calls it when needed.
param([switch]$Force)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$dest = Join-Path $root "vendor\pdfium"
if ((Test-Path (Join-Path $dest "bin\pdfium.dll")) -and -not $Force) {
    Write-Host "PDFium already present in $dest (use -Force to re-download)"
    return
}

$lock = Get-Content (Join-Path $PSScriptRoot "pdfium.lock") | Where-Object { $_ -and -not $_.StartsWith("#") }
$version = ($lock | Where-Object { $_ -like "version *" }).Split(" ")[1]
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
$asset = "pdfium-win-$arch.tgz"
$expected = (($lock | Where-Object { $_ -like "sha256 $asset *" }) -split " ")[2]
if (-not $expected) { throw "No checksum for $asset in pdfium.lock" }

$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$([uri]::EscapeDataString($version))/$asset"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "nopepdf-$asset"
Write-Host "Downloading $url"
$ProgressPreference = "SilentlyContinue"
Invoke-WebRequest $url -OutFile $tmp -UseBasicParsing
$actual = (Get-FileHash $tmp -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $expected) { Remove-Item $tmp; throw "Checksum mismatch for ${asset}: $actual" }

if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
New-Item -ItemType Directory -Force $dest | Out-Null
tar -xzf $tmp -C $dest
if ($LASTEXITCODE -ne 0) { throw "tar failed" }
Remove-Item $tmp
Write-Host "PDFium $version extracted to $dest"

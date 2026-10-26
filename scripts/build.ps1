# Builds a release, assembles a portable folder dist\NopePDF\{nopepdf.exe, pdfium.dll}
# and, when Inno Setup is installed, the installer dist\NopePDF-setup-<version>.exe.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent

# Rust (GNU toolchain) needs MinGW binutils; WinLibs is installed via winget.
$mingw = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Directory -Filter "BrechtSanders.WinLibs*" -ErrorAction SilentlyContinue |
    ForEach-Object { Join-Path $_.FullName "mingw64\bin" } | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($mingw) { $env:PATH = "$mingw;$env:PATH" }
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

Push-Location $root
try {
    & (Join-Path $PSScriptRoot "fetch-pdfium.ps1")
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    $dist = Join-Path $root "dist\NopePDF"
    if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
    New-Item -ItemType Directory -Force (Join-Path $dist "licenses") | Out-Null
    Copy-Item "target\release\nopepdf.exe" $dist
    Copy-Item "vendor\pdfium\bin\pdfium.dll" $dist
    Copy-Item "LICENSE" (Join-Path $dist "LICENSE.txt")
    # PDFium and the libraries bundled into it require their notices to ship with the binary.
    Copy-Item "vendor\pdfium\licenses\*" (Join-Path $dist "licenses")
    Copy-Item "vendor\pdfium\LICENSE" (Join-Path $dist "licenses\pdfium-binaries.txt")
    Write-Host "Portable: $dist"

    $iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if ($iscc) {
        $version = (Select-String -Path "Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
        & $iscc /Q "/DMyAppVersion=$version" "installer\nopepdf.iss"
        if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
        Write-Host "Installer: $(Join-Path $root "dist\NopePDF-setup-$version.exe")"
    } else {
        Write-Host "Inno Setup not found - skipping installer (winget install JRSoftware.InnoSetup)"
    }
} finally {
    Pop-Location
}

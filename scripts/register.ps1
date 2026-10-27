# Registers NopePDF (current user only, no admin) as a PDF viewer so it shows up in
# "Open with" and in Settings > Default apps. Windows does not let programs set
# themselves as the default; the last step opens Settings so you can pick it.
param([string]$Exe = (Join-Path (Split-Path $PSScriptRoot -Parent) "dist\NopePDF\nopepdf.exe"))
$ErrorActionPreference = "Stop"
$Exe = (Resolve-Path $Exe).Path
$cmd = "`"$Exe`" `"%1`""
$classes = "HKCU:\Software\Classes"

# Creates a registry key only when it is missing. New-Item -Force on an existing key
# would wipe its values, which matters for shared keys such as RegisteredApplications.
function Ensure-Key([string]$Path) {
    if (-not (Test-Path $Path)) { New-Item $Path -Force | Out-Null }
}

Ensure-Key "$classes\NopePDF.Document\shell\open\command"
Set-ItemProperty "$classes\NopePDF.Document" "(default)" "Dokument PDF"
Set-ItemProperty "$classes\NopePDF.Document\shell\open\command" "(default)" $cmd
Ensure-Key "$classes\NopePDF.Document\DefaultIcon"
Set-ItemProperty "$classes\NopePDF.Document\DefaultIcon" "(default)" "`"$Exe`",0"

Ensure-Key "$classes\.pdf\OpenWithProgids"
New-ItemProperty "$classes\.pdf\OpenWithProgids" "NopePDF.Document" -Value "" -PropertyType String -Force | Out-Null

Ensure-Key "$classes\Applications\nopepdf.exe\shell\open\command"
Set-ItemProperty "$classes\Applications\nopepdf.exe\shell\open\command" "(default)" $cmd
Ensure-Key "$classes\Applications\nopepdf.exe\SupportedTypes"
New-ItemProperty "$classes\Applications\nopepdf.exe\SupportedTypes" ".pdf" -Value "" -PropertyType String -Force | Out-Null

# Capabilities so it is listed under Default apps.
$cap = "HKCU:\Software\NopePDF\Capabilities"
Ensure-Key "$cap\FileAssociations"
Set-ItemProperty $cap "ApplicationName" "NopePDF"
Set-ItemProperty $cap "ApplicationDescription" "Lekki czytnik PDF"
Set-ItemProperty "$cap\FileAssociations" ".pdf" "NopePDF.Document"
Ensure-Key "HKCU:\Software\RegisteredApplications"
Set-ItemProperty "HKCU:\Software\RegisteredApplications" "NopePDF" "Software\NopePDF\Capabilities"

Write-Host "Registered: $Exe"
Write-Host "Now pick NopePDF for .pdf in the Settings window that opens."
Start-Process "ms-settings:defaultapps"

# Registers NopePDF (current user only, no admin) as a PDF viewer so it shows up in
# "Open with" and in Settings > Default apps. Windows does not let programs set
# themselves as the default; the last step opens Settings so you can pick it.
param([string]$Exe = (Join-Path (Split-Path $PSScriptRoot -Parent) "dist\NopePDF\nopepdf.exe"))
$ErrorActionPreference = "Stop"
$Exe = (Resolve-Path $Exe).Path
$cmd = "`"$Exe`" `"%1`""
$classes = "HKCU:\Software\Classes"

New-Item "$classes\NopePDF.Document\shell\open\command" -Force | Out-Null
Set-ItemProperty "$classes\NopePDF.Document" "(default)" "Dokument PDF"
Set-ItemProperty "$classes\NopePDF.Document\shell\open\command" "(default)" $cmd
New-Item "$classes\NopePDF.Document\DefaultIcon" -Force | Out-Null
Set-ItemProperty "$classes\NopePDF.Document\DefaultIcon" "(default)" "`"$Exe`",0"

New-Item "$classes\.pdf\OpenWithProgids" -Force | Out-Null
New-ItemProperty "$classes\.pdf\OpenWithProgids" "NopePDF.Document" -Value "" -PropertyType String -Force | Out-Null

New-Item "$classes\Applications\nopepdf.exe\shell\open\command" -Force | Out-Null
Set-ItemProperty "$classes\Applications\nopepdf.exe\shell\open\command" "(default)" $cmd
New-Item "$classes\Applications\nopepdf.exe\SupportedTypes" -Force | Out-Null
New-ItemProperty "$classes\Applications\nopepdf.exe\SupportedTypes" ".pdf" -Value "" -PropertyType String -Force | Out-Null

# Capabilities so it is listed under Default apps.
$cap = "HKCU:\Software\NopePDF\Capabilities"
New-Item "$cap\FileAssociations" -Force | Out-Null
Set-ItemProperty $cap "ApplicationName" "NopePDF"
Set-ItemProperty $cap "ApplicationDescription" "Lekki czytnik PDF"
Set-ItemProperty "$cap\FileAssociations" ".pdf" "NopePDF.Document"
New-Item "HKCU:\Software\RegisteredApplications" -Force | Out-Null
Set-ItemProperty "HKCU:\Software\RegisteredApplications" "NopePDF" "Software\NopePDF\Capabilities"

Write-Host "Registered: $Exe"
Write-Host "Now pick NopePDF for .pdf in the Settings window that opens."
Start-Process "ms-settings:defaultapps"

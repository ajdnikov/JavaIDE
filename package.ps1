# Builds a portable, offline-ready Java IDE folder in dist\JavaIDE:
#   JavaIDE.exe   the IDE
#   jdk\          a full JDK 25 (javac, java, jshell ...) - no install needed
#   jdtls\        the Eclipse JDT Language Server (completion, imports, live problems)
#   workspace\    a sample project that opens on first launch
#
# Usage:  .\package.ps1                      (auto-detects a JDK 25 and the language server)
#         .\package.ps1 -Jdk "D:\jdks\jdk-25" (use a specific JDK)
#         .\package.ps1 -Jdtls "D:\jdtls"     (use a specific language server folder)
#         .\package.ps1 -Zip                  (also create dist\JavaIDE.zip)
param(
    [string]$Jdk = "",
    [string]$Jdtls = "",
    [switch]$Zip
)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

function Get-JavaMajor([string]$jdkHome) {
    $java = Join-Path $jdkHome "bin\java.exe"
    if (-not (Test-Path $java)) { return 0 }
    $out = (& cmd /c "`"$java`" -version 2>&1") -join "`n"
    if ($out -match '"(\d+)') { return [int]$Matches[1] }
    return 0
}

if (-not $Jdk) {
    $candidates = @()
    if ($env:JAVA_HOME) { $candidates += $env:JAVA_HOME }
    foreach ($root in @("C:\Program Files\Java", "C:\Program Files\Eclipse Adoptium", "C:\Program Files\Microsoft",
                        "C:\Program Files\Zulu", "$env:LOCALAPPDATA\Programs\Eclipse Adoptium")) {
        if (Test-Path $root) { $candidates += Get-ChildItem $root -Directory | ForEach-Object FullName }
    }
    $Jdk = $candidates | Where-Object { (Test-Path (Join-Path $_ "bin\javac.exe")) -and (Get-JavaMajor $_) -ge 25 } |
        Select-Object -First 1
    if (-not $Jdk) { throw "No JDK 25+ found. Pass one with -Jdk <path>." }
}
$major = Get-JavaMajor $Jdk
if ($major -lt 25) { throw "$Jdk is Java $major; Java 25 or newer is required." }
Write-Host "Using JDK $major at $Jdk"

if (-not $Jdtls) {
    # The server inside VS Code's Java extension (newest installed version).
    $ext = @("$env:USERPROFILE\.vscode\extensions", "$env:USERPROFILE\.antigravity-ide\extensions") |
        Where-Object { Test-Path $_ } |
        ForEach-Object { Get-ChildItem $_ -Directory -Filter "redhat.java-*-win32-x64" } |
        Where-Object { Test-Path (Join-Path $_.FullName "server\plugins") } |
        Sort-Object { [version]($_.Name -replace '^redhat\.java-(\d+\.\d+\.\d+).*$', '$1') } -Descending |
        Select-Object -First 1
    if (-not $ext) { throw "No language server found. Install VS Code's Java extension or pass -Jdtls <path>." }
    $Jdtls = Join-Path $ext.FullName "server"
}
if (-not (Get-ChildItem (Join-Path $Jdtls "plugins") -Filter "org.eclipse.equinox.launcher_*.jar" -ErrorAction SilentlyContinue)) {
    throw "$Jdtls is not a JDT Language Server folder."
}
Write-Host "Using language server at $Jdtls"

Write-Host "Building release..."
# cargo prints progress on stderr; don't let PowerShell 5.1 treat that as an error
$ErrorActionPreference = "Continue"
cargo build --release
$ErrorActionPreference = "Stop"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$dist = Join-Path $PSScriptRoot "dist\JavaIDE"
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Force $dist | Out-Null
Copy-Item "target\release\JavaIDE.exe" $dist

Write-Host "Copying JDK (this takes a moment)..."
robocopy $Jdk (Join-Path $dist "jdk") /E /NFL /NDL /NJH /NJS /NP | Out-Null
if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE)" }

Write-Host "Copying language server..."
foreach ($part in "plugins", "features", "config_win") {
    robocopy (Join-Path $Jdtls $part) (Join-Path $dist "jdtls\$part") /E /NFL /NDL /NJH /NJS /NP | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy failed for $part ($LASTEXITCODE)" }
}
$license = Join-Path (Split-Path $Jdtls -Parent) "LICENSE.txt"
if (Test-Path $license) { Copy-Item $license (Join-Path $dist "jdtls\LICENSE.txt") }

Copy-Item -Recurse "workspace" (Join-Path $dist "workspace")
Copy-Item "README.md" $dist

if ($Zip) {
    $zipPath = Join-Path $PSScriptRoot "dist\JavaIDE.zip"
    if (Test-Path $zipPath) { Remove-Item $zipPath }
    Compress-Archive -Path $dist -DestinationPath $zipPath
    Write-Host "Created $zipPath"
}
$size = "{0:N0} MB" -f ((Get-ChildItem $dist -Recurse | Measure-Object Length -Sum).Sum / 1MB)
Write-Host "Done: $dist ($size). Copy that folder anywhere (USB stick, another PC) and run JavaIDE.exe."


exit 0

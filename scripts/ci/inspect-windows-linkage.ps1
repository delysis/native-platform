# Read-only diagnostics for a native test executable that fails before its harness starts.
$ErrorActionPreference = 'Stop'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$dumpbin = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC/Tools/MSVC/**/bin/Hostx64/x64/dumpbin.exe' | Select-Object -First 1
if (-not $dumpbin) { throw 'MSVC dumpbin was not found' }

$profile = Join-Path $PWD 'target/debug'
$deps = Join-Path $profile 'deps'
$executables = @(Get-ChildItem $deps -Filter 'tauri_plugin_loom-*.exe' -ErrorAction SilentlyContinue)
$libraries = @(
    Get-ChildItem $profile -Filter '*.dll' -ErrorAction SilentlyContinue
    Get-ChildItem $deps -Filter '*.dll' -ErrorAction SilentlyContinue
    Get-Item (Join-Path $env:SystemRoot 'System32/onnxruntime.dll') -ErrorAction SilentlyContinue
    Get-Item (Join-Path $env:SystemRoot 'System32/DirectML.dll') -ErrorAction SilentlyContinue
)

foreach ($file in $executables + $libraries) {
    Write-Output "::group::$($file.FullName)"
    Write-Output "Bytes: $($file.Length); version: $($file.VersionInfo.FileVersion)"
    Write-Output "Link type: $($file.LinkType); target: $($file.LinkTarget)"
    try {
        Get-FileHash $file.FullName -Algorithm SHA256 | Format-List
        & $dumpbin /nologo /imports $file.FullName
        if ($file.Extension -eq '.dll') { & $dumpbin /nologo /exports $file.FullName }
    } catch {
        Write-Warning "Could not inspect $($file.FullName): $_"
    } finally {
        Write-Output '::endgroup::'
    }
}

param(
    [Parameter(Mandatory)][string]$Version,
    [string]$MakeNsis = "$env:LOCALAPPDATA/tauri/NSIS/Bin/makensis.exe"
)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$') { throw 'Invalid release version' }
$repo = (Resolve-Path "$PSScriptRoot/../..").Path
$work = Join-Path $repo "src-tauri/target/installer-download-tests/$Version"
New-Item -ItemType Directory -Force "$work/downloads", "$work/app" | Out-Null
$template = Get-Content "$repo/src-tauri/nsis/template.nsi" -Raw
# Execute the production download/checksum/extraction code, without process
# termination, uninstall, registry writes, shortcuts, or UI pages.
$start = $template.IndexOf('  !addplugindir', $template.IndexOf('Section Install'))
$end = $template.IndexOf('  ; Create uninstaller', $start)
if ($start -lt 0 -or $end -lt 0) { throw 'Installer test boundaries not found' }
$body = $template.Substring($start, $end - $start)
$body = $body.Replace('..\..\..\..\nsis\plugins\x86-unicode', "$repo\src-tauri\nsis\plugins\x86-unicode")
$body = $body.Replace('$TEMP', '$EXEDIR\downloads')
$body = [regex]::Replace($body, '(?m)^(\s*)DetailPrint "(.*)"\r?$', '$1FileWrite $TestLog "$2$\r$\n"')
$script = @'
Unicode true
OutFile "download-test.exe"
RequestExecutionLevel user
SilentInstall silent
!include LogicLib.nsh
!include FileFunc.nsh
!include x64.nsh
!include StrFunc.nsh
${StrCase}
${StrLoc}
!include "@REPO@\src-tauri\nsis\checksum.nsh"
!addplugindir "@PLUGINS@"
!define VERSION "@VERSION@"
!define MAINBINARYNAME "VRCT"
Var TargetVersion
Var TestLog
Function .onInstFailed
  FileWrite $TestLog "FAIL installer aborted$\r$\n"
  FileClose $TestLog
  SetErrorLevel 1
FunctionEnd
Function CleanupFailedInstall
FunctionEnd
Section
  StrCpy $INSTDIR "$EXEDIR\app"
  StrCpy $TargetVersion "@VERSION@"
  FileOpen $TestLog "$EXEDIR\download.log" w
@BODY@
  FileWrite $TestLog "PASS downloaded, SHA-256 verified, extracted VRCT.exe$\r$\n"
  FileClose $TestLog
  SetErrorLevel 0
SectionEnd
'@
$plugins = "$env:LOCALAPPDATA\tauri\NSIS\Plugins\x86-unicode\additional"
$script = $script.Replace('@REPO@', $repo).Replace('@VERSION@', $Version).Replace('@PLUGINS@', $plugins).Replace('@BODY@', $body)
[IO.File]::WriteAllText((Join-Path $work 'download-test.nsi'), $script, [Text.UTF8Encoding]::new($false))
Push-Location $work
try {
    & $MakeNsis /V2 download-test.nsi
    if ($LASTEXITCODE -ne 0) { throw 'Installer download test compilation failed' }
    $process = Start-Process -FilePath ./download-test.exe -ArgumentList /S -WindowStyle Hidden -PassThru -Wait
    Get-Content download.log
    if ($process.ExitCode -ne 0) { throw "Installer download test failed: $($process.ExitCode)" }
    # The production section removes the ZIP after successful extraction.
    # Check the extracted payload against every entry of its integrity manifest.
    $manifest = Get-Content app/native-package-manifest.json -Raw | ConvertFrom-Json
    if ($manifest.version -ne $Version) { throw 'Extracted app version mismatch' }
    foreach ($file in $manifest.files.PSObject.Properties) {
        $path = Join-Path "$work/app" $file.Name
        $actual = Get-FileHash -LiteralPath $path -Algorithm SHA256
        if ($actual.Hash.ToLowerInvariant() -ne $file.Value.sha256 -or (Get-Item -LiteralPath $path).Length -ne $file.Value.bytes) {
            throw "Extracted payload mismatch: $($file.Name)"
        }
    }
    Write-Host "Verified all $(@($manifest.files.PSObject.Properties).Count) extracted file hashes"
} finally { Pop-Location }

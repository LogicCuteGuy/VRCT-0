param([string]$MakeNsis = "$env:LOCALAPPDATA/tauri/NSIS/Bin/makensis.exe")
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path "$PSScriptRoot/../..").Path
$work = Join-Path $repo 'src-tauri/target/installer-checksum-tests'
New-Item -ItemType Directory -Force $work | Out-Null
$payload = Join-Path $work 'payload.bin'
[IO.File]::WriteAllText($payload, 'checksum regression fixture', [Text.Encoding]::ASCII)
$sha256 = [Security.Cryptography.SHA256]::Create()
try {
    $digest = [BitConverter]::ToString($sha256.ComputeHash([IO.File]::ReadAllBytes($payload))).Replace('-', '').ToLowerInvariant()
} finally { $sha256.Dispose() }
$cases = @(
    @{ Name = 'bare'; Text = $digest; Expected = $digest },
    @{ Name = 'sha256sum'; Text = "$digest  VRCT-0.zip`n"; Expected = $digest },
    @{ Name = 'crlf-upper'; Text = "$($digest.ToUpperInvariant())`r`n"; Expected = $digest },
    @{ Name = 'whitespace'; Text = " `t$digest`t*VRCT-0.zip`r`n"; Expected = $digest },
    @{ Name = 'empty'; Text = ''; Expected = '' },
    @{ Name = 'short'; Text = $digest.Substring(1); Expected = '' },
    @{ Name = 'long'; Text = "${digest}a"; Expected = '' },
    @{ Name = 'nonhex'; Text = ('z' * 64); Expected = '' },
    @{ Name = 'html'; Text = '<html>404</html>'; Expected = '' }
)
$script = @'
Unicode true
OutFile "checksum-tests.exe"
RequestExecutionLevel user
SilentInstall silent
!include LogicLib.nsh
!include StrFunc.nsh
${StrCase}
${StrLoc}
!include "@REPO@\src-tauri\nsis\checksum.nsh"
!addplugindir "@REPO@\src-tauri\nsis\plugins\x86-unicode"
Section
  FileOpen $9 "$EXEDIR\results.txt" w
'@
$script = $script.Replace('@REPO@', $repo)
foreach ($case in $cases) {
    [IO.File]::WriteAllText((Join-Path $work "$($case.Name).sha256"), $case.Text, [Text.Encoding]::ASCII)
    $block = @'
  FileOpen $0 "$EXEDIR\@NAME@.sha256" r
  StrCpy $1 ""
  FileRead $0 $1
  FileClose $0
  Push $1
  Call ParsePackageChecksum
  Pop $1
  ${If} $1 != "@EXPECTED@"
    FileWrite $9 "FAIL @NAME@: $1$\r$\n"
    FileClose $9
    SetErrorLevel 1
    Quit
  ${EndIf}
  FileWrite $9 "PASS @NAME@$\r$\n"
'@
    $script += "`n" + $block.Replace('@NAME@', $case.Name).Replace('@EXPECTED@', $case.Expected)
}
$script += "`n" + @'
  NScurl::sha256 -file "$EXEDIR\payload.bin"
  Pop $1
  ${If} $1 != "@DIGEST@"
    FileWrite $9 "FAIL NScurl file hash: $1$\r$\n"
    FileClose $9
    SetErrorLevel 2
    Quit
  ${EndIf}
  FileWrite $9 "PASS real file digest$\r$\n"
  FileOpen $0 "$EXEDIR\payload.bin" a
  FileSeek $0 0 END
  FileWrite $0 "altered"
  FileClose $0
  NScurl::sha256 -file "$EXEDIR\payload.bin"
  Pop $1
  ${If} $1 == "@DIGEST@"
    FileWrite $9 "FAIL modified file accepted$\r$\n"
    FileClose $9
    SetErrorLevel 3
    Quit
  ${EndIf}
  FileWrite $9 "PASS modified file rejected$\r$\n"
  FileClose $9
  SetErrorLevel 0
SectionEnd
'@.Replace('@DIGEST@', $digest)
[IO.File]::WriteAllText((Join-Path $work 'checksum-tests.nsi'), $script, [Text.UTF8Encoding]::new($false))
Push-Location $work
try {
    & $MakeNsis /V2 checksum-tests.nsi
    if ($LASTEXITCODE -ne 0) { throw 'NSIS checksum test compilation failed' }
    $process = Start-Process -FilePath ./checksum-tests.exe -ArgumentList /S -WindowStyle Hidden -PassThru -Wait
    Get-Content results.txt
    if ($process.ExitCode -ne 0) { throw "NSIS checksum tests failed: $($process.ExitCode)" }
} finally { Pop-Location }

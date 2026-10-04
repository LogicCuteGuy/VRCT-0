$ErrorActionPreference = 'Stop'
if ($args.Count -eq 0) { throw 'windows_native requires a command.' }
$Program = $args[0]
$ProgramArgs = @($args | Select-Object -Skip 1)

if (-not $env:VCINSTALLDIR) {
    $vswhere = Join-Path ([Environment]::GetEnvironmentVariable('ProgramFiles(x86)')) 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Install Visual Studio C++ build tools or use Developer PowerShell.' }
    $installation = (& $vswhere -utf8 -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
    if (-not $installation) { throw 'Visual Studio x64 C++ build tools were not found.' }
    $vcvars = Join-Path $installation 'VC/Auxiliary/Build/vcvars64.bat'
    if (-not (Test-Path -LiteralPath $vcvars)) { throw "MSVC environment script is missing: $vcvars" }
    $environment = & cmd.exe /d /c "call `"$vcvars`" >nul && set"
    if ($LASTEXITCODE -ne 0) { throw 'Could not initialize the MSVC environment.' }
    foreach ($line in $environment) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
        }
    }
}
if (-not $env:LIBCLANG_PATH -or -not (Test-Path -LiteralPath (Join-Path $env:LIBCLANG_PATH 'libclang.dll'))) {
    $repository = Split-Path $PSScriptRoot
    $candidates = @(
        (Join-Path $repository 'src-tauri/target/native-cache/libclang-22.1.8/runtimes/win-x64/native'),
        (Join-Path $env:ProgramFiles 'LLVM/bin')
    )
    $env:LIBCLANG_PATH = $candidates | Where-Object { Test-Path -LiteralPath (Join-Path $_ 'libclang.dll') } | Select-Object -First 1
}
if (-not $env:LIBCLANG_PATH -or -not (Test-Path -LiteralPath (Join-Path $env:LIBCLANG_PATH 'libclang.dll'))) {
    throw 'ASIO builds require libclang. Install LLVM and set LIBCLANG_PATH to its bin folder (see docs/windows_audio.md).'
}
if (-not $env:CPAL_ASIO_DIR) {
    $cachedSdk = Join-Path (Split-Path $PSScriptRoot) 'src-tauri/target/tmp/asio_sdk'
    if (Test-Path -LiteralPath (Join-Path $cachedSdk 'common/asio.h')) { $env:CPAL_ASIO_DIR = $cachedSdk }
}
& $Program @ProgramArgs
exit $LASTEXITCODE

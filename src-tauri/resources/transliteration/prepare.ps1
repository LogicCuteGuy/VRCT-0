# Fetch the native dictionary without Python. The 360 MB payload is not stored
# in Git. Run before building a native installer; existing verified files reuse.
[CmdletBinding()]
param(
    [string]$ResourceDirectory = $PSScriptRoot,
    [string]$ArchivePath
)
$ErrorActionPreference = 'Stop'
$taskManifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'manifest.json') -Raw | ConvertFrom-Json
$taskOutputDir = [IO.Path]::GetFullPath($ResourceDirectory)
[IO.Directory]::CreateDirectory($taskOutputDir) | Out-Null
$taskDictionary = Join-Path $taskOutputDir 'system.dic'
function Test-TaskDictionary([string]$Path) {
    if (!(Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    if ((Get-Item -LiteralPath $Path).Length -ne $taskManifest.dictionary_bytes) { return $false }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() -eq $taskManifest.dictionary_sha256
}
function Copy-TaskResources {
    foreach ($taskName in @('sudachi.json', 'char.def', 'rewrite.def', 'unk.def', 'LEGAL', 'LICENSE-2.0.txt', 'manifest.json')) {
        $taskSourcePath = Join-Path $PSScriptRoot $taskName
        $taskDestinationPath = Join-Path $taskOutputDir $taskName
        if ([IO.Path]::GetFullPath($taskSourcePath) -ne [IO.Path]::GetFullPath($taskDestinationPath)) {
            Copy-Item -LiteralPath $taskSourcePath -Destination $taskDestinationPath -Force
        }
    }
}
if (Test-TaskDictionary $taskDictionary) {
    Copy-TaskResources
    Write-Output "Sudachi full $($taskManifest.version): verified $taskDictionary"
    return
}
$taskDownload = $null
$taskTemporaryDictionary = Join-Path $taskOutputDir ('.system-' + [guid]::NewGuid().ToString('N') + '.tmp')
$taskZip = $null
try {
    if (!$ArchivePath) {
        $taskDownload = Join-Path ([IO.Path]::GetTempPath()) ('vrct-sudachi-' + [guid]::NewGuid().ToString('N') + '.zip')
        Invoke-WebRequest -Uri $taskManifest.archive_url -OutFile $taskDownload
        $ArchivePath = $taskDownload
    }
    $taskArchive = [IO.Path]::GetFullPath($ArchivePath)
    if ((Get-Item -LiteralPath $taskArchive).Length -ne $taskManifest.archive_bytes -or
        (Get-FileHash -LiteralPath $taskArchive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $taskManifest.archive_sha256) {
        throw 'Sudachi archive checksum/size mismatch'
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $taskZip = [IO.Compression.ZipFile]::OpenRead($taskArchive)
    $taskEntry = $taskZip.GetEntry($taskManifest.dictionary_entry)
    if (!$taskEntry -or $taskEntry.Length -ne $taskManifest.dictionary_bytes) { throw 'Sudachi dictionary entry missing/wrong size' }
    # Extract this exact entry only; no archive-controlled filesystem paths.
    $taskSource = $taskEntry.Open()
    $taskDestination = [IO.File]::Create($taskTemporaryDictionary)
    try { $taskSource.CopyTo($taskDestination) }
    finally { $taskDestination.Dispose(); $taskSource.Dispose() }
    if (!(Test-TaskDictionary $taskTemporaryDictionary)) { throw 'Sudachi dictionary checksum mismatch' }
    Move-Item -LiteralPath $taskTemporaryDictionary -Destination $taskDictionary -Force
    Copy-TaskResources
    Write-Output "Sudachi full $($taskManifest.version): prepared and verified $taskDictionary"
}
finally {
    if ($taskZip) { $taskZip.Dispose() }
    # Remove only the exact random files created by this invocation.
    if (Test-Path -LiteralPath $taskTemporaryDictionary) { Remove-Item -LiteralPath $taskTemporaryDictionary -Force }
    if ($taskDownload -and (Test-Path -LiteralPath $taskDownload)) { Remove-Item -LiteralPath $taskDownload -Force }
}

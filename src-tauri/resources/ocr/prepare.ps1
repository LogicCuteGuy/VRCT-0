param([string]$Destination = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
$ocrManifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'models.json') -Raw | ConvertFrom-Json
New-Item -ItemType Directory -Path $Destination -Force | Out-Null
foreach ($ocrModel in $ocrManifest.models) {
    $ocrFinal = Join-Path $Destination $ocrModel.file
    if ((Test-Path -LiteralPath $ocrFinal) -and (Get-FileHash -LiteralPath $ocrFinal -Algorithm SHA256).Hash.ToLowerInvariant() -eq $ocrModel.sha256) { continue }
    $ocrPartial = "$ocrFinal.$PID.partial"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $ocrModel.url -OutFile $ocrPartial -TimeoutSec 180
        if ((Get-FileHash -LiteralPath $ocrPartial -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ocrModel.sha256) { throw "OCR model checksum mismatch: $($ocrModel.file)" }
        Move-Item -LiteralPath $ocrPartial -Destination $ocrFinal -Force
    } finally {
        if (Test-Path -LiteralPath $ocrPartial) { Remove-Item -LiteralPath $ocrPartial }
    }
    Write-Output "Prepared $($ocrModel.file)"
}

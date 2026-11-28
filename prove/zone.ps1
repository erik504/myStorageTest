# Lettura sequenziale a inizio, metà e fine del disco senza amministratore: legge un file da 2 GB al
# posto del disco (le velocità quindi non contano: si provano tratti, tempi, grafici e fumetti).
# Fa solo quella prova, con -SecondiPunto secondi per punto, poi schermate dei fumetti di velocità,
# latenza e del riquadro della fine. Quello che misura lo scrive in zone.txt, accanto alle schermate.
#
#   .\prove\zone.ps1
#   .\prove\zone.ps1 -SecondiPunto 10
param([string]$Exe = '', [string]$Vol = 'C', [int]$SecondiPunto = 3, [string]$Size = '1500,950', [int]$Secondi = 180)
. (Join-Path $PSScriptRoot 'comune.ps1')
if (-not $Exe) { $Exe = $ExePredefinito }
$file = Join-Path $Radice 'target\superficie-di-prova.bin'
$shots = Join-Path $Radice 'target\shots\zone'

if (-not (Test-Path $file)) {
    $f = [IO.File]::Create($file); $f.SetLength(2GB); $f.Close()
}
Clear-VariabiliProva
Clear-DatiProva
Remove-Item $shots -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $shots | Out-Null
Set-IstanzaProva
$env:MYSTORAGETEST_VOL = $Vol
$env:MYSTORAGETEST_AUTOTEST = 'zone'
$env:MYSTORAGETEST_ZONE_SECS = "$SecondiPunto"
$env:MYSTORAGETEST_SHOT = $shots
$env:MYSTORAGETEST_SIZE = $Size
$env:MYSTORAGETEST_SURFACE_FILE = $file

$codice = Invoke-Prova $Exe @() $Secondi
"codice di uscita: $codice"
Get-Content (Join-Path $shots 'zone.txt') -Encoding UTF8 -ErrorAction SilentlyContinue
Get-ChildItem $shots -Filter *.png | ForEach-Object { "  $($_.Name)" }

Clear-VariabiliProva
Clear-DatiProva
Remove-Item $file -Force -ErrorAction SilentlyContinue
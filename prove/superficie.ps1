# Scansione della superficie senza amministratore: legge adagio un file da 2 GB al posto del volume.
# Due avvii: il primo ferma la scansione a metà e si chiude; il secondo verifica il punto ricordato,
# riprende, prova zoom, tratto parziale, scelta di un tratto trascinando e «Riscandisci».
# Quello che osserva lo scrive in superficie.txt, accanto alle schermate.
#
#   .\prove\superficie.ps1
param([string]$Exe = '', [string]$Vol = 'C', [int]$Secondi = 120)
. (Join-Path $PSScriptRoot 'comune.ps1')
if (-not $Exe) { $Exe = $ExePredefinito }
$file = Join-Path $Radice 'target\superficie-di-prova.bin'
$shots = Join-Path $Radice 'target\shots\superficie'

if (-not (Test-Path $file)) {
    $f = [IO.File]::Create($file); $f.SetLength(2GB); $f.Close()
}
Clear-VariabiliProva
Clear-DatiProva
Remove-Item $shots -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $shots | Out-Null
Set-IstanzaProva
$env:MYSTORAGETEST_VOL = $Vol
$env:MYSTORAGETEST_AUTOTEST = 'superficie'
$env:MYSTORAGETEST_SHOT = $shots
$env:MYSTORAGETEST_SURFACE_FILE = $file

foreach ($fase in '1', '') {
    if ($fase) { $env:MYSTORAGETEST_SURFACE_PHASE = $fase } else { Remove-Item Env:MYSTORAGETEST_SURFACE_PHASE -ErrorAction SilentlyContinue }
    $codice = Invoke-Prova $Exe @() $Secondi
    "fase '$fase' finita, codice $codice"
}
Get-Content (Join-Path $shots 'superficie.txt') -Encoding UTF8 -ErrorAction SilentlyContinue
Get-ChildItem $shots -Filter *.png | ForEach-Object { "  $($_.Name)" }

Clear-VariabiliProva
Clear-DatiProva
Remove-Item $file -Force -ErrorAction SilentlyContinue

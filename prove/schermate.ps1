# Schermate delle schede senza fare prove sui dischi: istanza isolata, finestra fuori dallo schermo e
# senza fuoco, sequenza guidata con il puntatore simulato (grafo, fumetti, zoom, trascinamento, Info,
# SMART, eventi, temperatura, monitoraggio), poi chiusura.
#
#   .\prove\schermate.ps1 -Vol C                      grafo dei soli dischi
#   .\prove\schermate.ps1 -Vol C -Grafo completo      con la spunta «Grafo completo»
#   .\prove\schermate.ps1 -Vol 9                      un disco senza lettera, con il suo numero
param(
    [string]$Exe = '',
    [Parameter(Mandatory)][string]$Vol,
    [ValidateSet('dischi', 'completo')][string]$Grafo = 'dischi',
    [string]$Dir = '',
    [string]$Size = '1500,950',
    [int]$Secondi = 180
)
. (Join-Path $PSScriptRoot 'comune.ps1')
if (-not $Exe) { $Exe = $ExePredefinito }
if (-not $Dir) { $Dir = Join-Path $Radice "target\shots\schermate-$Grafo" }

Clear-VariabiliProva
Clear-DatiProva
# La modalità del grafo si sceglie con le impostazioni dell'istanza di prova.
Set-ImpostazioniProva $(if ($Grafo -eq 'completo') { '{"graph_full": true}' } else { '{"graph_full": false}' })
Remove-Item $Dir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Dir | Out-Null
Set-IstanzaProva
$env:MYSTORAGETEST_SHOT = $Dir
$env:MYSTORAGETEST_SIZE = $Size
$env:MYSTORAGETEST_VOL = $Vol

$codice = Invoke-Prova $Exe @() $Secondi
"codice di uscita: $codice"
Get-ChildItem $Dir -Filter *.png -ErrorAction SilentlyContinue | ForEach-Object { "  $($_.Name)" }

Clear-VariabiliProva
Clear-DatiProva

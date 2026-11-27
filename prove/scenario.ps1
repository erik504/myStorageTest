# Scenario completo di verifica dell'interfaccia: una prova vera sul volume scelto (e, con -Vol2, una
# seconda insieme su un altro volume), fumetti dei grafici con il puntatore simulato, tutte le schede,
# schermate PNG nella cartella -Dir, poi chiusura da solo. Dura un paio di minuti.
#
#   .\prove\scenario.ps1 -Vol J
#   .\prove\scenario.ps1 -Vol C -Vol2 E
#
# Riuscita = codice di uscita 0, una ventina di schermate, nessun file di prova rimasto sui volumi.
# Le schermate vanno guardate: i problemi di impaginazione si vedono solo lì.
param(
    [string]$Exe = '',
    [Parameter(Mandatory)][string]$Vol,
    [string]$Vol2 = '',
    [string]$Dir = '',
    [string]$Size = '1500,1150',
    [int]$Secondi = 400
)
. (Join-Path $PSScriptRoot 'comune.ps1')
if (-not $Exe) { $Exe = $ExePredefinito }
if (-not $Dir) { $Dir = Join-Path $Radice 'target\shots\scenario' }

Clear-VariabiliProva
Clear-DatiProva
Remove-Item $Dir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Dir | Out-Null
Set-IstanzaProva
$env:MYSTORAGETEST_SHOT = $Dir
$env:MYSTORAGETEST_SIZE = $Size
$env:MYSTORAGETEST_AUTOTEST = '1'
$env:MYSTORAGETEST_VOL = $Vol
if ($Vol2) { $env:MYSTORAGETEST_VOL2 = $Vol2 }

$tempo = [Diagnostics.Stopwatch]::StartNew()
$codice = Invoke-Prova $Exe @() $Secondi
"codice di uscita: $codice, in $([int]$tempo.Elapsed.TotalSeconds) s"
$foto = @(Get-ChildItem $Dir -Filter *.png -ErrorAction SilentlyContinue)
"schermate: $($foto.Count)"
$foto | ForEach-Object { "  $($_.Name)" }

Clear-VariabiliProva
Clear-DatiProva
$lettere = @($Vol, $Vol2) | Where-Object { $_ -match '^[A-Za-z]$' }
$rimasti = @(Get-FileRimasti $lettere)
"file di prova rimasti: " + $(if ($rimasti.Count) { $rimasti -join ', ' } else { 'nessuno' })

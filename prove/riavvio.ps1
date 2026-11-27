# Prova di «Riavvia come amministratore» senza elevazione vera: con MYSTORAGETEST_RESTART_TEST il
# rilancio usa "open" al posto di "runas". La prima istanza si rilancia da sola e si chiude.
# Riuscita = la prima è uscita e ne resta viva un'altra, con la sua finestra.
# Le istanze già aperte dall'utente vengono ignorate e non vengono toccate.
#
#   .\prove\riavvio.ps1
param([string]$Exe = '')
. (Join-Path $PSScriptRoot 'comune.ps1')
if (-not $Exe) { $Exe = $ExePredefinito }

$mine = @((Get-Process mystoragetest -ErrorAction SilentlyContinue).Id)
"istanze dell'utente (ignorate): " + ($mine -join ',')
function Get-Test { @(Get-Process mystoragetest -ErrorAction SilentlyContinue | Where-Object { $mine -notcontains $_.Id }) }

Clear-VariabiliProva
Clear-DatiProva
Set-IstanzaProva
$env:MYSTORAGETEST_RESTART_TEST = '1'
$first = Start-Process -FilePath $Exe -PassThru
"prima istanza: pid $($first.Id)"

$deadline = (Get-Date).AddSeconds(20)
while (-not $first.HasExited -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
"prima istanza uscita: $($first.HasExited)"
Start-Sleep -Seconds 4

$left = Get-Test
if ($left.Count -eq 0) {
    "ESITO: nessuna istanza rimasta (il riavvio NON ha aperto il programma)"
} else {
    foreach ($p in $left) {
        $cmd = (Get-CimInstance Win32_Process -Filter "ProcessId=$($p.Id)").CommandLine
        "rimasta: pid $($p.Id)  finestra=$($p.MainWindowHandle)  titolo='$($p.MainWindowTitle)'  cmd=$cmd"
    }
    "ESITO: istanze rimaste = $($left.Count)"
}

# Pulizia delle sole istanze di prova: --quit con lo stesso nome d'istanza chiude solo quella.
Remove-Item Env:MYSTORAGETEST_RESTART_TEST
Start-Process -FilePath $Exe -ArgumentList '--quit' -Wait
Start-Sleep -Seconds 3
$left = Get-Test
if ($left.Count -gt 0) { "ancora vive dopo --quit: " + ($left.Id -join ','); $left | Stop-Process -Force }
Clear-VariabiliProva
Clear-DatiProva
"pulizia fatta; istanze dell'utente ancora vive: " + (@((Get-Process mystoragetest -ErrorAction SilentlyContinue).Id) -join ',')

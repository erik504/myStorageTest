# Prova dell'avvio diretto da amministratore senza elevazione vera: con MYSTORAGETEST_RESTART_TEST il
# rilancio usa "open" al posto di "runas". Tre casi: argomento --admin, impostazione
# «Avvia sempre come amministratore», nessuna richiesta.
# Riuscita = nei primi due casi il primo processo esce subito e ne resta uno solo, rilanciato con
# --after <pid> e gli stessi argomenti (senza --admin); nel terzo nessun rilancio.
# Le istanze dell'utente vengono ignorate e non vengono toccate.
#
#   .\prove\amministratore.ps1 -Vol C
param([string]$Exe = '', [string]$Vol = 'C')
. (Join-Path $PSScriptRoot 'comune.ps1')
if (-not $Exe) { $Exe = $ExePredefinito }

$mine = @((Get-Process mystoragetest -ErrorAction SilentlyContinue).Id)
"istanze dell'utente (ignorate): " + ($mine -join ',')
function Get-Test { @(Get-Process mystoragetest -ErrorAction SilentlyContinue | Where-Object { $mine -notcontains $_.Id }) }
function Stop-Test {
    Remove-Item Env:MYSTORAGETEST_RESTART_TEST -ErrorAction SilentlyContinue
    Start-Process -FilePath $Exe -ArgumentList '--quit' -Wait
    Start-Sleep -Seconds 3
    $left = Get-Test
    if ($left.Count -gt 0) { "ancora vive dopo --quit: " + ($left.Id -join ','); $left | Stop-Process -Force }
}

Clear-VariabiliProva
Set-IstanzaProva
foreach ($case in 'argomento', 'impostazione', 'spenta') {
    Clear-DatiProva
    $list = @('--tab', 'smart', '--vol', $Vol)
    if ($case -eq 'argomento') { $list = @('--admin') + $list }
    if ($case -eq 'impostazione') { Set-ImpostazioniProva '{ "always_admin": true }' }
    $env:MYSTORAGETEST_RESTART_TEST = '1'
    $first = Start-Process -FilePath $Exe -ArgumentList $list -PassThru
    Start-Sleep -Milliseconds 1200
    $early = $first.HasExited
    Start-Sleep -Seconds 4
    $left = Get-Test
    $cmds = $left | ForEach-Object { (Get-CimInstance Win32_Process -Filter "ProcessId=$($_.Id)").CommandLine }
    "[$case] primo processo uscito subito: $early · rimaste: $($left.Count)"
    $cmds | ForEach-Object { "    $_" }
    Stop-Test
}
Clear-VariabiliProva
Clear-DatiProva
"pulizia fatta; istanze dell'utente ancora vive: " + (@((Get-Process mystoragetest -ErrorAction SilentlyContinue).Id) -join ',')

# Parti comuni agli script di prova. Si includono con: . (Join-Path $PSScriptRoot 'comune.ps1')
#
# Le istanze di prova hanno un nome a parte (cartella dati e finestra proprie), non prendono il fuoco
# e stanno fuori dallo schermo: l'istanza aperta dall'utente non viene mai toccata.

$Radice = Split-Path $PSScriptRoot -Parent
$ExePredefinito = Join-Path $Radice 'target\release\mystoragetest.exe'

# Cartelle dati dell'istanza di prova. In alcune sessioni la shell vede una AppData virtualizzata:
# la seconda strada porta alla cartella vera (se non c'è virtualizzazione è la stessa cartella).
function Get-CartelleProva {
    $locale = Join-Path $env:LOCALAPPDATA 'myStorageTest-prova'
    $vera = '\\localhost\' + $env:LOCALAPPDATA.Substring(0, 1) + '$' + $env:LOCALAPPDATA.Substring(2) + '\myStorageTest-prova'
    @($locale, $vera) | Select-Object -Unique
}

function Clear-DatiProva {
    foreach ($d in Get-CartelleProva) { Remove-Item $d -Recurse -Force -ErrorAction SilentlyContinue }
}

# Scrive le impostazioni iniziali dell'istanza di prova, es. '{"graph_full": true}'.
function Set-ImpostazioniProva([string]$Json) {
    foreach ($d in Get-CartelleProva) {
        New-Item -ItemType Directory -Force $d -ErrorAction SilentlyContinue | Out-Null
        Set-Content (Join-Path $d 'settings.json') $Json -Encoding UTF8 -ErrorAction SilentlyContinue
    }
}

function Set-IstanzaProva {
    $env:MYSTORAGETEST_INSTANCE = 'prova'
    $env:MYSTORAGETEST_NOACTIVATE = '1'
    $env:MYSTORAGETEST_POS = '-6000,300'
}

function Clear-VariabiliProva {
    Get-ChildItem Env: | Where-Object { $_.Name -like 'MYSTORAGETEST_*' } | ForEach-Object { Remove-Item "Env:$($_.Name)" }
}

# Avvia l'exe e aspetta che finisca; restituisce il codice di uscita (o $null se è stato fermato).
function Invoke-Prova([string]$Exe, [string[]]$Argomenti = @(), [int]$Secondi = 240) {
    $p = if ($Argomenti.Count) { Start-Process -FilePath $Exe -ArgumentList $Argomenti -PassThru }
         else { Start-Process -FilePath $Exe -PassThru }
    $null = $p.Handle   # senza, ExitCode resta vuoto dopo WaitForExit
    if (-not $p.WaitForExit($Secondi * 1000)) {
        Write-Host "non ha finito in $Secondi s: lo fermo"
        $p | Stop-Process -Force
        return $null
    }
    $p.ExitCode
}

# File di prova rimasti nella radice dei volumi indicati (dovrebbero essere zero).
function Get-FileRimasti([string[]]$Lettere) {
    foreach ($l in $Lettere) {
        Get-ChildItem "$($l):\" -Force -Filter '.mystoragetest-*' -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName }
    }
}

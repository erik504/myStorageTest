# Installa la build di release come myStorageTest.exe nella radice del repository (escluso da git) e
# pulisce i dati delle istanze di prova. Se l'exe è in uso (l'utente ha il programma aperto), quello
# vecchio viene rinominato myStorageTest.old.exe: l'istanza aperta resta alla versione vecchia finché
# l'utente non la chiude e riapre.
#
#   .\prove\installa.ps1
param([string]$Exe = '')
. (Join-Path $PSScriptRoot 'comune.ps1')
if (-not $Exe) { $Exe = $ExePredefinito }
$dst = Join-Path $Radice 'myStorageTest.exe'
$old = Join-Path $Radice 'myStorageTest.old.exe'

# La copia vecchia si toglie solo se nessuno la sta usando.
Remove-Item $old -Force -ErrorAction SilentlyContinue
try {
    Copy-Item $Exe $dst -Force -ErrorAction Stop
    "installato"
} catch {
    Rename-Item $dst $old -ErrorAction Stop
    Copy-Item $Exe $dst -Force
    "installato (quello in uso rinominato in myStorageTest.old.exe)"
}
$uguali = (Get-FileHash $Exe).Hash -eq (Get-FileHash $dst).Hash
"stesso contenuto della build: $uguali"
Clear-DatiProva

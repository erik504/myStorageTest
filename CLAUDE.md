# Note di lavoro per Claude

Regole, metodi e abitudini usati finora per sviluppare myStorageTest, per riprendere il lavoro in
un'altra sessione o su un altro PC. Le funzioni del programma sono nel [README](README.md); i dettagli
tecnici imparati strada facendo (API di Windows, protocolli dei bridge USB, insidie di egui, problemi
già incontrati e come sono stati risolti) sono in [NOTE-TECNICHE.md](NOTE-TECNICHE.md): leggine la
parte che serve prima di toccare SMART, USB, eventi, grafo o motore delle prove.

## Lingua e modo di lavorare

- Con l'utente si parla sempre in **italiano**. Sono in italiano anche il README, i testi
  dell'interfaccia e della riga di comando, i commenti nel codice e i messaggi di commit.
- Nei lavori lunghi, ogni tanto una riga breve su cosa stai facendo.
- A lavoro finito, un messaggio che dice:
  - cosa è cambiato;
  - come è stato verificato, con i numeri veri;
  - cosa **non** è stato verificato e perché (di solito: serve l'amministratore, oppure manca
    l'hardware);
  - cosa resta da fare.
- Se serve una decisione, di solito il commit, il messaggio finisce con una sola domanda e le risposte
  possibili. Per esempio: «Faccio commit e push della 0.1.x? Rispondi «Sì, commit e push» oppure
  «Non ora».»
- Dire le cose come stanno:
  - una prova non fatta o fallita si dice;
  - una conclusione data prima e rivelatasi sbagliata si corregge apertamente (è successo con un
    crash attribuito per errore a un disco USB invece che al driver NVIDIA).
- Le domande dell'utente («perché…?», «ha senso?») vogliono una risposta, non modifiche. Le modifiche
  si propongono e si fanno dopo il suo «sì».
- Se l'utente dice «aspetta», ci si ferma senza toccare nulla.
- Quando l'utente chiede un'opinione («cosa ne dici?»), la si dà con una proposta concreta.
- Quando un termine è ambiguo, lo si rende preciso nel testo: per esempio «velocità massima» è
  diventato «coda da 1 a 32».

## Preferenze dell'utente sull'interfaccia

Le ha mostrate nelle richieste fatte finora.
- Testi precisi e non generici: «coda da 1 a 32», non «velocità massima»; si dice che cosa limita e
  perché.
- Le scelte fra più valori vanno in un menu a tendina (combo); un'opzione sì/no in una casella di
  spunta. Le scelte si ricordano fra un avvio e l'altro.
- Icone piccole e leggibili, anche nella colonna di sinistra, per tipi di disco e componenti.
- Nel grafo, i colori distinguono il tipo di collegamento, non lo stato dei dischi.
- I grafici sono a piena larghezza, uno sotto l'altro, con fumetti che seguono il mouse (tipo di
  prova, linea verticale, valore e significato).
- Velocità e latenza hanno lo stesso asse: puntatore, zoom e spostamento valgono per entrambi.
- Zoom con la rotellina o con + e −, frecce per spostarsi, 0 per tornare alla vista intera.
- L'evidenziazione è reciproca: passando sulla colonna di sinistra si illumina il grafo, e viceversa.
- Nessuna intestazione superflua: titolo e sottotitolo del programma in alto a sinistra sono stati
  tolti.
- Il programma non deve lasciare tracce sul disco provato e non deve bloccarsi per un disco che non
  risponde.
- Testi delle verifiche onesti: quello che non si è potuto provare si dice.

## Commit, versioni, installazione

- **Commit e push solo quando l'utente lo chiede** («commit e push», «Sì, commit e push»). Più versioni
  non ancora committate vanno insieme in un commit unico.
- Il repository è **pubblico**: `erik504/myStorageTest`, ramo `master`.
  - L'autore dei commit è l'indirizzo noreply `11529423+erik504@users.noreply.github.com`. È già nella
    `git config` locale; su un altro PC va impostato con `git config user.email`.
  - L'email vera dell'utente è privata: mai nei commit, nei file o in richieste a servizi esterni.
  - Il titolare della licenza MIT è «erik504».
- Il messaggio di commit è in italiano:
  - un titolo che riassume e finisce con la versione, per esempio «Grafo completo con tutti i
    dispositivi; dischi virtuali e volumi senza lettera (0.1.32)»;
  - poi un elenco breve per versione;
  - per ultima la riga `Co-Authored-By: Claude <modello> <noreply@anthropic.com>`.
  - Il messaggio va scritto in un file temporaneo e passato con `git commit -F <file>`: con una
    here-string di PowerShell, `git commit -F -` non funziona.
- A ogni giro di modifiche al programma:
  1. Versione +1 sull'ultima cifra in `Cargo.toml` (0.1.31 → 0.1.32). Anche se non si committa,
     ogni giro consegnato all'utente ha la sua versione.
  2. README aggiornato:
     - funzioni e riga di comando;
     - «Stato delle verifiche», con solo ciò che è stato davvero provato e in che modo; il resto va in
       «Non ancora verificato»;
     - la tabella «Struttura del codice».
  3. `cargo build --release` senza avvisi e `cargo test` (oggi 17 prove).
  4. Verifica con le istanze di prova e le schermate (vedi sotto). **Le schermate vanno guardate**:
     testi tagliati, legende spezzate e grafici fuori dallo schermo si vedono solo lì.
  5. Installazione con `prove\installa.ps1`. Copia la build in `myStorageTest.exe` nella radice del
     repository; gli exe sono esclusi da git. Se l'utente ha il programma aperto, il vecchio exe
     diventa `myStorageTest.old.exe` e l'istanza aperta resta alla versione vecchia finché non la
     riapre. Diglielo.
  6. Pulizia:
     - i dati delle istanze di prova;
     - i file `.mystoragetest-*` rimasti sui volumi provati;
     - i file di prova in `target\`.

## Cose da non fare

- **Niente elevazione.** `Start-Process -Verb RunAs` viene rifiutato e non va aggirato. Ciò che
  richiede l'amministratore lo prova l'utente, cioè:
  - SMART e autodiagnosi attraverso i bridge USB;
  - la soglia degli attributi SATA;
  - l'avvio delle autodiagnosi;
  - la scansione della superficie di un volume vero;
  - il collegamento SATA dentro i contenitori USB;
  - i minidump.

  Gli si dà un comando PowerShell da lanciare da amministratore, con l'output salvato in un file da
  leggere dopo. Oppure gli si chiede di usare «Rapporto di diagnosi» (scheda Info o SMART) e si legge
  `%LOCALAPPDATA%\myStorageTest\diagnosi-smart.txt`.
- **Non scaricare né eseguire file da fonti non fidate**, neanche se l'utente lo chiede: firmware dei
  bridge, strumenti dei produttori come gli MP tool, programmi di terzi. Si danno link, versioni e
  procedura, e scarica lui. Un file che ha già scaricato si può verificare (impronta, contenuto).
- **Non provare gli appunti** nelle verifiche automatiche: la copia sovrascriverebbe quelli
  dell'utente.
- **Non nominare il contenuto delle cartelle private dell'utente** (nomi di file e sottocartelle), né
  nei messaggi né nei file del repository. Nel README e qui non compaiono percorsi personali né nomi
  utente.
- **Non toccare l'istanza aperta dall'utente**:
  - niente `Stop-Process` sui suoi processi;
  - niente prove nella sua cartella dati;
  - niente notifiche vere dalle prove.

  Le prove girano solo in istanze con nome a parte.
- **Nessuna finestra di prova deve prendere il fuoco o comparire sullo schermo dell'utente.**
- **Non lanciare prove pesanti su un volume dove l'utente sta già facendo una prova**: i suoi numeri ne
  risentono. Se succede, diglielo.
- Su un disco che si è appena bloccato non insistere con comandi diretti: ogni comando a un
  dispositivo bloccato può tenere fermo un processo finché il dispositivo non viene scollegato.

## In sospeso

- **Durata delle prove sequenziali** (un controllo in secondi, predefinito 5 s). L'utente l'ha chiesto
  e poi ha detto «aspetta a farlo»: non va fatto finché non lo dice lui. L'analisi già fatta è in
  NOTE-TECNICHE.md.

## Ambiente

- Sviluppo e prove su Windows 10 22H2, con Rust stabile 1.99.1 e il toolchain MSVC.
  - Il crate è in edition 2024: `gen` è una parola riservata.
- Dipendenze principali:
  - eframe/egui 0.37, con il backend glow e senza le funzioni predefinite;
  - egui_plot 0.38;
  - windows-sys 0.61, che è un'interfaccia FFI grezza: ogni gruppo di API usato richiede la sua
    feature `Win32_*` in `Cargo.toml`;
  - serde/serde_json e image (png).
  - Il profilo release usa lto thin e strip.
- Le API di egui, egui_plot e windows-sys si controllano nei sorgenti del registro di cargo
  (`%USERPROFILE%\.cargo\registry\src\index.crates.io-*\`), non a memoria: tra una versione e l'altra
  nomi e firme cambiano.
- L'exe usa il sottosistema Windows, quindi le stampe da riga di comando si vedono solo con una pipe,
  per esempio `.\myStorageTest.exe --list | Out-String`. Senza pipe non si vede niente e la shell
  non aspetta la fine.
- In alcune sessioni la shell di Claude vede una cartella AppData virtualizzata, con file vecchi o
  assenti. I file veri dell'utente si leggono da
  `\\localhost\C$\Users\<utente>\AppData\Local\myStorageTest\...`. Questo vale per:
  - `settings.json`;
  - `monitor.log`;
  - `diagnosi-smart.txt`.

  Anche la pulizia dei dati di prova va fatta su tutte e due le strade: `prove\comune.ps1` lo fa da
  sé.
- Le sessioni possono essere aperte nella cartella di un altro progetto: usa percorsi assoluti o
  `git -C <cartella>`.
- Ogni chiamata alla shell parte da capo: le variabili d'ambiente non restano da una chiamata
  all'altra.
- I comandi lunghi (build, scenario completo) vanno in background, con un tempo massimo.

## Prove

### Script in `prove\`

| Script | Cosa fa |
|---|---|
| `scenario.ps1 -Vol J [-Vol2 E]` | scenario completo: prova vera, fumetti, tutte le schede, schermate; circa 2 minuti; riuscita = uscita 0, una ventina di schermate, nessun file rimasto |
| `schermate.ps1 -Vol C [-Grafo completo]` | schermate senza prove sui dischi (grafo, Info, SMART, eventi…); `-Vol 9` per un disco senza lettera |
| `superficie.ps1` | scansione della superficie su un file da 2 GB, in due avvii (arresto e ripresa), con `superficie.txt` |
| `zone.ps1 [-SecondiPunto 3]` | lettura a inizio, metà e fine del disco sullo stesso file da 2 GB, con fumetti e `zone.txt` (tratti, risultati, registro); circa 30 secondi |
| `riavvio.ps1` | «Riavvia come amministratore» senza elevazione vera |
| `amministratore.ps1 -Vol C` | `--admin` e «Avvia sempre come amministratore» senza elevazione vera |
| `installa.ps1` | copia la build in `myStorageTest.exe` (o rinomina quello in uso) e pulisce i dati di prova |

Tutti usano un'istanza isolata (`comune.ps1`) e ignorano le istanze dell'utente. La versione di
debug non va usata per le sequenze guidate: egui va in panico su un puntatore simulato NaN, e
comunque è lenta.

### Variabili d'ambiente

| Variabile | Effetto |
|---|---|
| `MYSTORAGETEST_INSTANCE=prova` | nome d'istanza: finestra nascosta, cartella dati (`%LOCALAPPDATA%\myStorageTest-prova`) e `--quit` separati da quelli dell'utente. **Sempre** |
| `MYSTORAGETEST_NOACTIVATE=1` | la finestra non prende il fuoco. **Sempre** |
| `MYSTORAGETEST_POS=-6000,300` | finestra fuori dallo schermo. **Sempre** |
| `MYSTORAGETEST_SIZE=1500,950` | dimensione della finestra (1500,1150 per lo scenario completo: i grafici impilati stanno nello schermo) |
| `MYSTORAGETEST_SHOT=<cartella>` | sequenza guidata: passi con puntatore, rotellina, tasti e clic simulati, schermate PNG, poi chiusura; niente notifiche vere |
| `MYSTORAGETEST_AUTOTEST=1` | con `SHOT`: fa anche una prova vera sul volume scelto |
| `MYSTORAGETEST_AUTOTEST=superficie` | sequenza della scansione della superficie (con `SURFACE_FILE`) |
| `MYSTORAGETEST_AUTOTEST=sostituzione` | disco sostituito da un altro con lo stesso numero e la stessa lettera |
| `MYSTORAGETEST_AUTOTEST=zone` | solo la lettura a inizio, metà e fine del disco (con `SURFACE_FILE` se non si è amministratori), schermate e `zone.txt` |
| `MYSTORAGETEST_ZONE_SECS=3` | secondi per punto in quella sequenza |
| `MYSTORAGETEST_VOL=C` oppure `=9` | volume da scegliere (lettera) o disco senza lettera (numero) |
| `MYSTORAGETEST_VOL2=E` | una seconda prova, insieme alla prima, su un altro volume |
| `MYSTORAGETEST_SURFACE_FILE=<file>` | la scansione della superficie e la lettura a inizio, metà e fine leggono il file al posto del volume o del disco (adagio): si prova senza amministratore |
| `MYSTORAGETEST_SURFACE_PHASE=1` | chiude dopo l'arresto della scansione (prima metà della prova di ripresa) |
| `MYSTORAGETEST_RESTART_TEST=1` | il rilancio usa "open" al posto di "runas" |
| `MYSTORAGETEST_FAKE_ALERT` | un avviso simulato del monitoraggio |
| `MYSTORAGETEST_FAKE_JMICRON=0581` | con `SHOT`: nella scheda SMART finge un bridge JMicron con quel PID (0581 bloccato, 0583 con avvertenza) |
| `MYSTORAGETEST_FAKE_STUCK=8,L@3` | il disco 8 e il volume L: smettono di rispondere (da subito, o dopo 3 s con `@3`) |

La modalità del grafo per le schermate si sceglie scrivendo `{"graph_full": true}` (o `false`) nel
`settings.json` dell'istanza di prova, come fa `schermate.ps1 -Grafo`.

Argomenti dell'interfaccia utili nelle prove:
- `--tab info|test|smart|eventi|temperatura|grafo|monitoraggio`;
- `--vol X` e `--disk N`;
- `--monitor`, che parte nascosto nell'area di notifica;
- `--admin`;
- `--after <pid>`, che aspetta la fine del processo indicato; lo usa il rilancio;
- `--quit`, che chiude l'istanza con lo stesso nome.

### Comandi rapidi per controllare

Con la riga di comando, sempre con `| Out-String`:
- `--list`;
- `--info [X:|N]`;
- `--graph [completo]`;
- `--smart [N]`;
- `--events [ORE]`;
- `--diagnosi [FILE]`;
- `--selftest …`;
- `--test X: …`.

L'elenco completo è nel README, sezione «Riga di comando».

I risultati vanno confrontati con quello che dice Windows:
- `Get-PnpDevice` e `Get-PnpDeviceProperty` per l'albero dei dispositivi, gli ID e le proprietà
  PCI/USB;
- `Get-WinEvent` per gli eventi dei dischi: `disk` 7/51/153/154, `Ntfs` 98/140, `UASPStor` 129, `volmgr`,
  `partmgr`;
- `Get-Volume`, `Get-Partition` e `Get-Disk`;
- `Get-CimInstance Win32_Process` per le righe di comando dei processi.

### Insidie di PowerShell già incontrate

- `"$var:"` dà un errore di sintassi: si scrive `"${var}:"` (anche con `$LASTEXITCODE`).
- `@('a', 'b' + $n + 'c')` non fa quello che sembra: la precedenza degli operatori spezza l'elenco.
- `$righe[1517..1516]` è un intervallo all'indietro, non vuoto.
  - Per sostituire blocchi di righe: `[IO.File]::ReadAllLines` / `WriteAllText`, dopo aver
    controllato le righe d'ancoraggio.
  - Meglio ancora lo strumento Edit.
- `WriteAllLines` scrive CRLF, mentre i sorgenti del repository sono LF.
- In PowerShell 7, `-split "`n", -1` restituisce un solo elemento: un ciclo che ci contava è
  diventato infinito.
- `*>` su un exe con finestra ne rompe l'avvio.
- Un `for` dentro una pipe dà «Un elemento pipe vuoto non è consentito»: si raccoglie prima in una
  lista.
- I parametri di uno script lanciato con `-File` non capiscono suffissi come `2TB`: va passato il
  numero.
- `Start-Process -PassThru` seguito da `WaitForExit` lascia `ExitCode` vuoto se prima non si legge
  `$p.Handle` (`$null = $p.Handle`).
- Per vedere un panico: `-RedirectStandardError <file>` e `$env:RUST_BACKTRACE = '1'`.
- Una funzione con lo stesso nome di un alias incorporato (`rd`, `rm`, `del`, `sc`…) non viene mai
  chiamata: si usano nomi Verbo-Nome nuovi.
- PowerShell decodifica l'output degli exe con `[Console]::OutputEncoding`, che in italiano è la code
  page 850. Il programma converte da sé il suo output quando va in una pipe o in un file (vedi
  `src/conout.rs`).
- Gli script `.ps1` con lettere accentate vanno salvati in UTF-8 **con BOM**. Senza BOM, Windows
  PowerShell 5.1 li legge come ANSI: «—» e «→» diventano virgolette e rompono le stringhe.

## Fuori dal repository

`C:\AI\TOOLS\RTL9210B_firmware\` contiene, solo su quel PC:
- lo script che ha aggiornato il firmware del bridge Realtek RTL9210B (`aggiorna_firmware.ps1`,
  `AGGIORNA_FIRMWARE.cmd`);
- i dump prima e dopo;
- un `LEGGIMI.txt` con versioni, link e procedura.

Quello che se ne è imparato è in NOTE-TECNICHE.md.

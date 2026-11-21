# myStorageTest

Test, diagnosi e monitoraggio dei dischi per Windows 10 e 11, in un solo eseguibile scritto in Rust.

Nasce da un caso reale: un SSD in un contenitore USB che per ore ha risposto con errori hardware a
ogni scrittura senza che nessuno se ne accorgesse. Il programma serve a due cose: provare un disco
quando si ha un dubbio, e accorgersi subito quando un disco comincia a dare errori.

## Cosa fa

La colonna a sinistra elenca i dischi, ognuno con la temperatura e il pallino dello stato SMART; in cima
c'è la voce **Monitoraggio**, con la casella che lo accende e lo spegne. Ogni disco ha cinque schede.

**Info**, la prima: tutti i dati del volume e dei dischi su cui sta. Del volume lo spazio occupato e
libero, il file system con le sue funzioni, l'unità di allocazione, il numero di serie e la partizione che
lo ospita; di ogni disco modello, numero di serie, firmware, collegamento, dimensione dei settori, TRIM,
cache in scrittura, tabella delle partizioni e stato SMART. Per ogni disco ci sono anche l'interfaccia, cioè
di che cosa è capace, e il collegamento attuale, cioè a che cosa lavora davvero:

- NVMe: versione dello standard e collegamento PCIe, generazione e linee (per esempio «NVMe 1.4 su PCIe
  4.0 x4», collegato a «PCIe 4.0 x1»), con la banda massima di ognuno;
- SATA: revisione dello standard, velocità massima del disco e velocità a cui si è accordato con il
  controller (1,5, 3 o 6 Gbit/s), dai dati di identificazione che Windows conserva;
- USB: velocità di cui il dispositivo è capace e quella a cui lavora, chiesta all'hub, con la porta, se
  è USB 2 o USB 3 e se il connettore è USB-C; da amministratore, per un disco SATA in un contenitore
  USB, anche il collegamento SATA fra il bridge e il disco.

Quando il collegamento è più lento di quanto il disco saprebbe fare la riga è evidenziata, con le cause
possibili (un NVMe x4 che usa una sola linea, un disco USB 3 su una porta USB 2, un SATA a 3 Gbit/s). In alto
«Esplora» apre il volume in Esplora risorse e, per i dispositivi USB e i supporti rimovibili, «Espelli»
chiede a Windows la rimozione sicura; di una scheda in un lettore con più alloggiamenti espelle solo il
supporto. In fondo c'è il rapporto di diagnosi del disco: i comandi inviati per leggerne lo SMART e le
risposte ricevute, già pronti dalla lettura fatta all'avvio; «Rifai» li ripete riprovando tutte le strade.
Le unità ottiche (CD e DVD) non sono fra i dischi che il programma elenca.

**Test** sul volume scelto, con grafici in tempo reale di velocità, latenza e distribuzione delle latenze
(passando il mouse su un grafico un fumetto dice a quale prova appartiene il punto, il valore e che cosa
significa). La distribuzione raccoglie le latenze in intervalli larghi circa il 10 % l'uno e si può
guardare su scala logaritmica, che mostra insieme microsecondi e secondi, oppure lineare, che allarga
il grosso delle risposte e lascia fuori lo 0,5 % più lento:

- scrittura e lettura sequenziale, con verifica che i dati riletti siano identici a quelli scritti;
- lettura e scrittura casuale a blocchi da 4 KiB (operazioni al secondo, latenza media, percentili, massimo);
- flush su disco, misurato operazione per operazione;
- rilettura di verifica dei blocchi scritti dalla scrittura casuale e dal flush: è una fase a sé
  («Verifica»), visibile nello stato, nel registro delle attività e nei grafici;
- lettura dei file già presenti sul volume, in sola lettura, per trovare settori illeggibili;
- lettura grezza della superficie del volume (solo da amministratore), su due grafici a parte, uno sopra
  l'altro e con lo stesso asse: velocità e latenza (media e lettura più lenta) lungo il disco, con i
  blocchi illeggibili segnati, mentre la scansione gira e alla fine. Si può scandire tutta o solo un
  tratto («dal… al…»). Ogni volume ricorda dove si è fermata l'ultima scansione rimasta a metà e lo
  ripropone come punto di partenza, anche dopo aver chiuso il programma; ripartendo, i tratti già letti
  nella sessione restano nel grafico. I grafici si ingrandiscono insieme con la rotellina o con + e −,
  si spostano trascinando con il tasto destro, e 0 rimostra tutto il disco. Trascinando con il tasto
  sinistro si sceglie un tratto: «Riscandisci» rilegge solo quello, lasciando nel grafico il resto e
  senza toccare il punto ricordato per la ripresa; un clic toglie la scelta;
- controllo degli errori di disco comparsi nel registro di sistema durante la prova.

Ogni volume ha la sua pagina Test: i risultati restano lì dopo la fine della prova e quando si guarda un
altro disco, fino alla prova successiva o alla chiusura del programma. Le prove su volumi diversi possono
girare insieme.

Ogni errore di I/O viene classificato: supporto danneggiato (CRC, settore non trovato), errore hardware
del dispositivo, collegamento (reset del bus), timeout, disco scollegato, file system danneggiato, dati
corrotti (rilettura diversa dalla scrittura), spazio esaurito, sola lettura. Un'operazione che resta
bloccata viene segnalata mentre è ancora in corso, e «Ferma» la annulla.

**SMART** di ogni disco, con gli attributi in chiaro e i valori anomali evidenziati, e l'**autodiagnosi**
(self-test) del disco: prova in corso con l'avanzamento, prove registrate dal disco con il loro esito,
avvio della prova breve o estesa e interruzione. Un'autodiagnosi fallita rende critico lo stato del disco.

**Eventi di sistema**: gli errori e gli avvisi di dischi, controller e file system registrati da Windows
nelle ultime 24 ore, 7 giorni o 30 giorni, con una spiegazione per i codici più comuni. «Aggiorna»
aggiunge solo gli eventi nuovi.

**Temperatura**: lo storico della temperatura del disco da quando il programma è aperto. Tutti i dischi
che la dichiarano vengono letti allo stesso ritmo, ogni 30 secondi (da 10 secondi a 15 minuti, a scelta
nella scheda). Un disco fermo non viene svegliato per leggerlo: prima della lettura periodica il programma
chiede a Windows se lo ha spento e, da amministratore, al disco stesso se sta girando (CHECK POWER MODE,
anche attraverso i bridge USB che riportano la risposta); la scheda lo mostra come «fermo» e le letture
riprendono quando torna a girare. Su certi dischi a piatti una lettura frequente può però rimandare la
messa a riposo: in quel caso conviene allungare l'intervallo.

**Monitoraggio**: un'icona nell'area di notifica sorveglia in tempo reale gli errori di disco nel registro
di sistema e rilegge periodicamente lo SMART di tutti i dischi, autodiagnosi comprese. Se compare un
problema l'icona cambia colore e arriva una notifica sul desktop con i dettagli; ogni avviso finisce anche
in `%LOCALAPPDATA%\myStorageTest\monitor.log`.

## Nessuna traccia sul disco provato

Le prove in scrittura usano un solo file temporaneo nascosto, aperto con `FILE_FLAG_DELETE_ON_CLOSE`:
Windows lo elimina quando l'handle si chiude, anche se il programma viene terminato a metà. A fine prova
il programma controlla che il file non esista più e lo dichiara.

La lettura dei file esistenti non modifica nulla, nemmeno la data di ultimo accesso, e salta i segnaposto
dei servizi cloud per non farli scaricare. Impostazioni e registro degli avvisi stanno sul disco di
sistema, mai sui dischi provati.

Resta inevitabile ciò che il file system annota da sé quando un file viene creato e cancellato (voci del
journal NTFS). L'autodiagnosi la esegue il disco e il suo esito resta nel registro interno del disco.

## Cosa serve l'amministratore

| Funzione | Utente normale | Amministratore |
|---|---|---|
| Test su file, lettura dei file esistenti | sì | sì |
| Eventi di sistema e monitoraggio degli errori | sì | sì |
| SMART dei dischi NVMe interni | sì | sì |
| SMART dei dischi SATA interni | sì, senza soglie | sì, con soglie |
| SMART dei dischi USB | no | sì |
| Autodiagnosi: stato e prove registrate (NVMe interni) | sì | sì |
| Autodiagnosi: stato ed esito dell'ultima prova (SATA interni) | sì | sì, con il registro completo |
| Autodiagnosi: avvio e interruzione | no | sì |
| Scansione della superficie | no | sì |

«Riavvia come amministratore» in basso a sinistra rilancia il programma con i privilegi necessari. Per
partire direttamente così c'è la casella «Avvia sempre come amministratore», subito sotto: a ogni avvio,
anche con Windows, il programma chiede i privilegi e Windows mostra la sua richiesta di conferma; se la
si rifiuta il programma si apre lo stesso, senza privilegi. Da un collegamento o dalla riga di comando si
ottiene lo stesso con `mystoragetest --admin`.

### Dischi USB

Per i dischi USB i comandi passano attraverso il bridge, e ogni bridge ha il suo modo di inoltrarli. Alla
prima lettura il programma cerca quello giusto, in quest'ordine, e lo ricorda per le letture successive:

1. lo standard **SAT** (ATA PASS-THROUGH a 12 e a 16 byte), che parlano quasi tutti i bridge USB-SATA; se
   il disco risponde ma ha lo SMART spento, il programma lo attiva;
2. i protocolli dei vecchi bridge USB-SATA **JMicron** (JM2033x), **Cypress**, **Sunplus** e **Prolific**,
   inviati solo ai bridge di quel produttore;
3. i protocolli dei bridge USB-NVMe **JMicron** (JMS583/JMS581/JMS586), **Realtek** (RTL9210/9211/9220) e
   **ASMedia** (ASM236x/ASM246x). Molti contenitori e SSD portatili montano un bridge ASMedia o Realtek
   sotto il nome del proprio produttore: a un dispositivo di marca sconosciuta che si dichiara un SSD
   vengono quindi provate anche queste due letture, e la risposta è accettata solo se ha esattamente la
   forma di un registro NVMe;
4. le pagine di registro **SCSI** standard (temperatura e previsione di guasto), per i dispositivi che non
   parlano né ATA né NVMe.

Alle chiavette e alle schede di memoria (supporti rimovibili) vanno solo i comandi SAT: di norma non hanno
dati SMART e il programma lo dice, senza riprovare a ogni lettura. Se un disco non risponde in tempo la
ricerca si ferma subito, invece di insistere con altri comandi. Fra i bridge USB-NVMe solo JMicron inoltra
il comando di avvio dell'autodiagnosi; Realtek e ASMedia inoltrano soltanto le letture.

«Rapporto di diagnosi», nella scheda SMART, scrive in `%LOCALAPPDATA%\myStorageTest\diagnosi-smart.txt` i
comandi inviati a ogni disco e le risposte ricevute, e lo apre: serve a capire perché un disco o un bridge
non restituisce i dati (`mystoragetest --diagnosi` fa lo stesso dalla riga di comando). Il rapporto del
solo disco scelto si legge in fondo alla scheda Info.

I formati dei comandi proprietari dei bridge sono quelli documentati dal progetto
[smartmontools](https://www.smartmontools.org/).

L'autodiagnosi degli NVMe interni passa dal driver di Windows, che accetta il comando da Windows 10 1903.

## Riga di comando

```
mystoragetest                     apre l'interfaccia
mystoragetest --admin             apre l'interfaccia chiedendo a Windows i privilegi di amministratore
mystoragetest --monitor           avvia nascosto nell'area di notifica, con il monitoraggio attivo
mystoragetest --quit              chiude l'istanza in esecuzione
mystoragetest --list              elenca volumi e dischi
mystoragetest --info [X:|N]       tutti i dati del volume X: o del disco N (senza argomento: di tutti)
mystoragetest --smart [N]         stato SMART di tutti i dischi o del disco N
mystoragetest --diagnosi [FILE]   comandi inviati a ogni disco per leggere lo SMART e risposte ricevute
mystoragetest --events [ORE]      errori di disco nel registro di sistema (predefinito: 24 ore)
mystoragetest --selftest [N]      autodiagnosi dei dischi: stato e prove registrate
mystoragetest --selftest N breve|estesa|stop
                                  avvia o interrompe l'autodiagnosi del disco N (da amministratore)
mystoragetest --check-events [N]  confronta la lettura veloce degli eventi con i testi di Windows
mystoragetest --export-icon FILE  salva in un PNG l'icona, in tutte le dimensioni e nei tre stati
mystoragetest --test X: [opzioni] esegue le prove sul volume X:
    --size MB    dimensione del file di prova (predefinito 256)
    --secs N     durata delle prove casuali (predefinito 5)
    --scan MB    legge anche i file esistenti fino a MB (0 = tutti)
    --surface    scansione della superficie (solo da amministratore)
    --from P     inizio della scansione della superficie, in percentuale (predefinito 0)
    --to P       fine della scansione della superficie, in percentuale (predefinito 100)
    --no-write   salta le prove in scrittura
```

`--test` esce con 0 se non ci sono errori, 1 con avvisi, 2 con errori, 3 se interrotto.

«Avvia con Windows» nella pagina Monitoraggio registra `mystoragetest --monitor` per l'utente corrente.
L'avvio automatico parte senza privilegi di amministratore, a meno che sia spuntata «Avvia sempre come
amministratore» (e allora all'accesso compare la richiesta di conferma di Windows): senza privilegi lo
SMART dei dischi USB non viene sorvegliato e per quei dischi restano gli errori del registro di sistema.
Il monitoraggio non sveglia i dischi fermi: per loro resta valida l'ultima lettura.

## Lettura veloce degli eventi

Far comporre a Windows il testo di ogni evento costa circa 0,3 ms l'uno, e un disco guasto ne scrive
decine di migliaia. Il programma chiede quindi a Windows il testo una sola volta per tipo di evento, con
dei segnaposto al posto dei dati, e poi inserisce i dati da sé. Il modello vale solo dopo aver riprodotto
esattamente il testo di Windows su eventi veri; quando Windows traduce un dato (per esempio un codice di
stato nella sua descrizione) si torna a chiedere il testo a Windows, ricordando quelli già composti.

Sul PC di sviluppo 20.000 eventi si leggono in 0,9 secondi invece di 6,2, e i 39.067 degli ultimi 30
giorni in 2 secondi invece di 12. `--check-events` ripete il confronto: 78.134 testi, nessuno diverso.

## Compilare

Serve Rust stabile con il toolchain MSVC.

```
cargo build --release
```

L'eseguibile è `target\release\mystoragetest.exe`, senza altre dipendenze da installare.

## Stato delle verifiche

Provato su Windows 10 22H2 con dischi NVMe e SATA interni e con dischi USB:

- test completi su un NVMe interno e su un SSD NVMe in contenitore USB, con controllo che il file di prova sparisca;
- due prove contemporanee su due volumi, ognuna con la sua pagina, che resta dopo aver cambiato disco;
- SMART senza privilegi su sei NVMe e due SATA interni;
- autodiagnosi senza privilegi: disponibilità, durate dichiarate e prove registrate su sei NVMe; disponibilità ed esito dell'ultima prova su due SATA;
- lettura degli eventi di sistema, compresa una raffica reale di circa 38.000 errori di un disco USB, con i testi confrontati uno a uno con quelli di Windows;
- storico delle temperature degli SSD interni;
- scansione della superficie eseguita su un file al posto del volume: arresto a metà, punto ricordato e riproposto anche dopo la chiusura del programma, ripresa fino in fondo, tratto parziale, grafici di velocità e latenza, zoom con rotellina e tasti, scelta di un tratto trascinando (con un puntatore simulato) e sua rilettura con «Riscandisci»;
- monitoraggio: avvio nascosto, icona, notifica (con un avviso simulato), consumo nullo a riposo, chiusura con `--quit`;
- fumetti dei grafici (linea verticale, prova, valore e spiegazione) con un puntatore simulato;
- «Riavvia come amministratore» e l'avvio diretto da amministratore (`--admin` e «Avvia sempre come amministratore»): la sequenza di rilancio, provata senza elevazione;
- SMART da amministratore di un SSD NVMe in contenitore USB con bridge JMicron (152D:0581): la lettura riesce (prova fatta a mano dall'interfaccia);
- SMART da amministratore, dal «Rapporto di diagnosi»: due SATA interni con soglie e registro completo delle autodiagnosi; due dischi a piatti Seagate su USB (0BC2:3322 e 0BC2:61B7, UASP) con SAT a 12 byte, soglie, registro delle autodiagnosi e risposta alla domanda «il disco gira?»; lo stesso bridge JMicron senza un disco riconosciuto, che risponde a ogni comando con un errore dell'hardware e viene riportato come tale;
- comandi diretti a due chiavette USB (una su UASP, una no), che Windows lascia passare anche senza privilegi: i comandi SAT partono, vengono rifiutati come previsto e le chiavette risultano senza SMART, senza altri tentativi;
- scheda Info e `--info` su tredici dischi (NVMe e SATA interni, un volume distribuito su due dischi, dischi USB, chiavette, un disco virtuale) e sui volumi senza un disco fisico: partizioni GPT e MBR con le lettere al loro posto, settori, TRIM, cache in scrittura; velocità USB coerente con gli hub attraversati (dischi a 5 Gbit/s dietro un hub USB 3, una chiavetta USB 3 riconosciuta come limitata a 480 Mbit/s dietro un hub USB 2); per ogni disco USB il dispositivo a cui «Espelli» chiederebbe la rimozione, confrontato con l'albero dei dispositivi di Windows;
- interfaccia e collegamento attuale senza privilegi: sei NVMe (versione NVMe e PCIe confrontate con quelle che Windows riporta per il dispositivo; uno PCIe 4.0 x4 collegato a x1 viene segnalato), due SATA interni a 6 Gbit/s con la revisione SATA 3.3, cinque dispositivi USB con la velocità e il tipo di porta (USB 2 o 3, USB-C).

Non ancora verificato:

- **collegamento SATA dentro un contenitore USB**: si legge solo da amministratore, attraverso il bridge;
- **«Espelli»**: la richiesta a Windows è quella documentata per la rimozione sicura e il dispositivo a cui va è stato controllato, ma l'espulsione vera non è stata eseguita, per non scollegare i dischi in uso; l'espulsione del solo supporto (lettori con più alloggiamenti, supporti rimovibili non USB) non ha avuto un dispositivo su cui girare;
- **avvio e interruzione dell'autodiagnosi**, su qualunque disco: richiedono l'amministratore; i comandi sono scritti secondo le specifiche NVMe e ATA e la documentazione di Windows, ma non sono stati eseguiti;
- **SMART dei dischi USB con bridge Realtek e ASMedia, con i vecchi bridge JMicron, Cypress, Sunplus e Prolific** e **registri SCSI**: i comandi sono scritti secondo i protocolli documentati ma non c'era un dispositivo su cui eseguirli («Rapporto di diagnosi» serve proprio a controllarli);
- **disco fermo lasciato stare**: la domanda al disco funziona, ma un disco davvero fermo durante una lettura periodica non è ancora stato osservato;
- **scansione della superficie di un volume vero**: richiede l'amministratore; tutto il resto della scansione è stato provato leggendo un file al posto del volume;
- avviso del monitoraggio per un'autodiagnosi fallita: non c'era un disco con una prova fallita;
- storico delle temperature dei dischi USB;
- riapertura della finestra dall'icona e chiusura nell'area di notifica;
- riapertura di un'istanza elevata avviando di nuovo il programma senza privilegi;
- comportamento davanti a errori di I/O veri: sui dischi sani a disposizione non si sono presentati;
- Windows 11: le API usate sono le stesse, ma il programma non vi è stato eseguito.

I numeri dei dischi fisici cambiano dopo un riavvio o un ricollegamento: per questo gli eventi di sistema
precedenti all'ultimo avvio vengono attribuiti a un disco solo tramite la lettera del volume.

## Struttura del codice

| File | Contenuto |
|---|---|
| `src/disks.rs` | elenco di volumi e dischi fisici, bridge USB |
| `src/info.rs` | dati di dettaglio di volumi e dischi (partizioni, settori, collegamenti PCIe, SATA e USB) ed espulsione |
| `src/tester.rs` | motore delle prove di I/O |
| `src/errors.rs` | classificazione dei codici d'errore |
| `src/smart.rs` | SMART e autodiagnosi (NVMe, ATA, pass-through USB) |
| `src/eventlog.rs` | registro eventi di Windows: query, sottoscrizione, testi veloci |
| `src/monitor.rs` | monitoraggio continuo e avvisi |
| `src/tray.rs` | icona nell'area di notifica e notifiche |
| `src/icon.rs` | disegno dell'icona: pila di dischi a strati, colorata secondo lo stato |
| `build.rs` | mette la stessa icona nel file eseguibile, senza file .ico né compilatore di risorse |
| `src/ui.rs` | interfaccia (egui) e grafici |
| `src/cli.rs` | modalità a riga di comando |

Le variabili d'ambiente `MYSTORAGETEST_SHOT`, `MYSTORAGETEST_AUTOTEST`, `MYSTORAGETEST_VOL`,
`MYSTORAGETEST_VOL2`, `MYSTORAGETEST_NOACTIVATE`, `MYSTORAGETEST_POS` e `MYSTORAGETEST_FAKE_ALERT` servono
alle verifiche automatiche dell'interfaccia: eseguono una prova (con `VOL2` anche una seconda, insieme,
su un altro volume), salvano le schermate di ogni scheda (comprese quelle dei fumetti dei grafici, con un
puntatore simulato, e quella del grafico della superficie, con dati inventati) e chiudono.
`MYSTORAGETEST_INSTANCE` dà alle istanze di prova un nome, impostazioni e registro separati, così non
interferiscono con quella in uso; `MYSTORAGETEST_RESTART_TEST` prova il riavvio del programma senza la
richiesta di elevazione.

`MYSTORAGETEST_SURFACE_FILE` fa leggere alla scansione della superficie un file al posto del volume
(adagio, per avere il tempo di fermarla), così si prova senza privilegi. Con `MYSTORAGETEST_AUTOTEST=superficie`
la sequenza ferma la scansione, verifica il punto ricordato, la riprende, prova lo zoom e un tratto
parziale, sceglie un tratto trascinando sul grafico e lo fa rileggere, e scrive ciò che osserva in
`superficie.txt`; `MYSTORAGETEST_SURFACE_PHASE=1` la chiude dopo
l'arresto, per verificare al lancio successivo che la memoria sia rimasta.

## Licenza

[MIT](LICENSE).

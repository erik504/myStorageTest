# myStorageTest

Test, diagnosi e monitoraggio dei dischi per Windows 10 e 11, in un solo eseguibile scritto in Rust.

Nasce da un caso reale: un SSD in un contenitore USB che per ore ha risposto con errori hardware a
ogni scrittura senza che nessuno se ne accorgesse. Il programma serve a due cose: provare un disco
quando si ha un dubbio, e accorgersi subito quando un disco comincia a dare errori.

## Cosa fa

La colonna a sinistra elenca i dischi, ognuno con un'icona del tipo (NVMe, SSD, disco meccanico,
chiavetta, scheda SD, disco esterno, disco virtuale, volume su più dischi, volume cifrato, cloud) nel colore
del suo collegamento, la temperatura e il pallino dello stato SMART; in cima ci sono «Aggiorna», che
rilegge dischi, volumi, i dati della scheda Info e il Grafo Storage, la voce **Monitoraggio**, con la
casella che lo accende e lo spegne, e la voce **Grafo Storage**.

**Grafo Storage**, la pagina che si apre all'avvio: come sono collegati i dischi e gli altri dispositivi
PCIe. Da sinistra a destra il processore, il chipset (sulle schede AMD è uno switch PCIe collegato a una
porta del processore), i controller SATA e USB, gli hub USB in cascata, i bridge dei contenitori USB (tutti
in una colonna, subito prima dei dischi), e in una colonna i dischi, ognuno con dimensione, lettere, collegamento (NVMe su PCIe 4.0 x4, SATA 6 Gbit/s, i
giri al minuto dei dischi meccanici quando li dichiarano), salute SMART e temperatura. Il riquadro del
bridge dice il chip (per esempio Realtek RTL9210B-CG o JMicron JMS581, anche quando il contenitore ha
l'identificativo USB del suo produttore), se collega un disco NVMe o SATA, l'identificativo USB, la velocità
USB e il protocollo UAS o BOT; il fumetto aggiunge come passano i comandi SMART e se l'autodiagnosi si può
avviare. La linea fra bridge e disco ha il colore del collegamento dentro il contenitore (NVMe o SATA).
Chiavette e schede di memoria non hanno un bridge a sé. Ci sono anche gli altri canali PCIe: scheda grafica (con l'audio HDMI), schede di rete, audio,
controller senza dischi, porte libere, e le funzioni interne al processore e al chipset. Ogni riquadro ha
un'icona (processore, chipset, controller, hub, scheda grafica, rete, tipo di disco…) e il colore del tipo
di collegamento: PCIe e NVMe, SATA, USB, software (volumi e dischi virtuali, tratteggiati). Lo spessore
delle linee segue la banda teorica del collegamento; passando sopra a una linea un fumetto la dice e
spiega da dove viene (per esempio «PCIe 4.0 x4: 16 GT/s per linea × 4 linee, codifica 128b/130b → 7,9 GB/s
teorici», o che dietro il chipset tutto divide il suo collegamento con il processore). In giallo i
collegamenti più lenti di quanto il dispositivo potrebbe (un NVMe x4 su una sola linea, una scheda grafica
x16 che lavora a x8). A destra dei dischi i volumi che ne usano più d'uno, collegati a ciascuno, con il
modo in cui li usano (a strisce, esteso, in mirroring, chiesto a Windows), e i contenitori cifrati
VeraCrypt o TrueCrypt, collegati al disco o al volume che contiene il loro file, una colonna più a destra
di chi li ospita (un contenitore su un volume a strisce sta a destra di quel volume). In fondo i dischi virtuali
e i volumi senza un disco fisico, ciascuno con il driver che lo crea (per esempio Google Drive con il suo
driver di file system): Windows dice se un driver sta sul percorso delle richieste di un volume, e lo si
chiede per ogni driver caricato. Il grafo sta in un riquadro suo e all'inizio vi entra tutto (non sotto
il 45 %); la rotellina lo ingrandisce o lo rimpicciolisce attorno al puntatore, trascinandolo con il mouse
lo si sposta, + e − ingrandiscono e rimpiccioliscono, le frecce lo spostano (con Maiuscolo a passi più
lunghi) e 0 torna alla vista iniziale; sotto il grafo c'è la scala. Passando sopra a un riquadro si
vedono i nomi completi e gli identificativi, e si accendono il percorso fino al processore e la voce
corrispondente nella colonna a sinistra; passando sopra a una voce della colonna si accendono i suoi
dischi nel grafo. Gli identificativi di un riquadro: ID istanza di Windows, indirizzo PCI
(bus:dispositivo.funzione), percorsi di posizione (`PCIROOT(0)#PCI(0301)#PCI(0000)` e quello ACPI, con il
nome della porta nel firmware), il numero dello slot quando il firmware lo dichiara; per i dischi anche
`\\.\PhysicalDriveN`, la posizione, il numero di serie e gli identificativi del controller NVMe o del
dispositivo USB che li contiene; per i controller USB l'hub radice; per hub e dispositivi USB la porta e il
percorso fino a lì (`…#USBROOT(0)#USB(8)#USB(1)`); per i volumi il nome `\\?\Volume{…}\`, il dispositivo
di Windows e i driver. Sulle linee il canale: la porta PCIe da cui passa il collegamento (indirizzo, ID
istanza, percorsi), la porta dell'hub USB o quella del controller SATA, il file di un contenitore. Il clic
destro su un riquadro o su una linea copia gli identificativi; anche `--graph` li stampa.
Un clic su un disco ne apre la pagina. Il grafo viene dall'albero dei dispositivi di Windows: per ogni
disco e per ogni dispositivo PCI si risale fino al processore, senza privilegi di amministratore e senza
svegliare i dischi fermi, e si ricostruisce quando i dischi cambiano.

Ogni disco ha cinque schede.

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
possibili (un NVMe x4 che usa una sola linea, un disco USB 3 su una porta USB 2, un SATA a 3 Gbit/s).
«Esplora», a destra nella riga delle schede e quindi disponibile da tutte, apre il volume in Esplora
risorse. In alto nella scheda, per i dispositivi USB e i supporti rimovibili, «Espelli» chiede a Windows
la rimozione sicura; di una scheda in un lettore con più alloggiamenti espelle solo il
supporto. In fondo c'è il rapporto di diagnosi del disco: i comandi inviati per leggerne lo SMART e le
risposte ricevute, già pronti dalla lettura fatta all'avvio; «Rifai» li ripete riprovando tutte le strade.
Le unità ottiche (CD e DVD) non sono fra i dischi che il programma elenca.

**Test** sul volume scelto, con grafici in tempo reale di velocità, latenza e distribuzione delle latenze
(passando il mouse su un grafico un fumetto dice a quale prova appartiene il punto, il valore, per le
prove con coda la misura in corso in quell'istante, per esempio «Coda Q8 · blocchi da 256K in sequenza»,
e che cosa significa). I riquadri occupano tutta la larghezza, uno sotto l'altro: velocità e latenza,
con lo stesso asse del tempo (il puntatore indica lo stesso istante su entrambe, Ctrl + rotellina le
ingrandisce insieme, trascinando ci si sposta insieme e il doppio clic torna a tutta la prova), poi la
distribuzione della latenza e il registro delle attività. La distribuzione raccoglie le latenze in intervalli larghi circa il 10 % l'uno e si può
guardare su scala logaritmica, che mostra insieme microsecondi e secondi, oppure lineare, che allarga
il grosso delle risposte e lascia fuori lo 0,5 % più lento:

- scrittura e lettura sequenziale, con verifica che i dati riletti siano identici a quelli scritti;
- lettura e scrittura con coda da 1 a 32, in tre gruppi che si scelgono uno per uno: 4K (in punti a
  caso), 256K e 1M (in sequenza e in punti a caso). Ogni gruppo è una prova a sé, in lettura e in
  scrittura, con 1, 2, 4, 8, 16 e 32 richieste in volo insieme (la profondità della coda, Q), tutte da
  un solo thread, come nei programmi di benchmark. La durata di ogni misura si sceglie fra 0,5, 1, 2, 5
  e 10 secondi (1 s di partenza, ricordata): con tutti i gruppi le misure sono 60, circa un minuto a
  1 s. La tabella «velocità per blocco e profondità della coda» le raccoglie per gruppo: per ogni riga
  il valore a Q1 e il migliore fra Q1 e Q32 con la Q a cui è arrivato; passando sopra a un valore si
  vedono tutte le Q, con la latenza media. Il riquadro «Lettura Q1–32» riporta la lettura in sequenza
  più veloce. Le prove con una richiesta alla volta su NVMe e RAID restano a metà o meno di quello che
  il disco fa con più richieste insieme: a Q1 il disco resta fermo fra una richiesta e la successiva,
  mentre la risposta risale e la nuova richiesta scende. Le letture girano subito dopo la rilettura
  sequenziale, sul file appena scritto in ordine (dopo le scritture casuali i blocchi sono
  sparpagliati dentro l'SSD e le letture rallentano); le scritture per ultime. I dati letti non vengono
  controllati e quelli scritti non vengono verificati; con misure lunghe, sui dischi veloci la prova in
  scrittura scrive molti GB (circa 6 GB al secondo di misura su un NVMe PCIe 4.0) e mostra anche il calo
  quando finisce la cache veloce dell'SSD. Con blocchi da 4K e code lunghe un solo thread arriva a
  circa 100.000 operazioni al secondo su un NVMe PCIe 4.0: sui dischi più veloci quel limite può
  essere il programma;
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
Nei contenitori USB-NVMe la prova si avvia solo attraverso i bridge JMicron, con un loro protocollo.
Sui **JMS581** (152D:0581 e 1581) è bloccata, con la spiegazione: un contenitore con questo bridge
accetta il comando ma non lo esegue, nemmeno in 60 secondi; Windows lo reimposta e lo scollega, e il
bridge riparte solo togliendo l'alimentazione. Per gli altri JMicron (JMS583, JMS586) la sezione avvisa
che il bridge può bloccarsi allo stesso modo, dice quale bridge ha il contenitore e consiglia di chiudere
prima i file aperti sul disco. Se la prova non parte, la nota dice a quale passo del protocollo si è
fermata e perché (bridge che non risponde o rifiuta, oppure lo stato NVMe con cui risponde il disco).
Da riga di comando `--selftest` legge prima lo SMART dei dischi USB, come fa la finestra, per sapere quale
protocollo usare con il bridge.
Gli attributi 202 e 206 hanno un significato da dischi a piatti (errori «Data Address Mark», altezza di
volo delle testine) che negli SSD cambia con il produttore, come nella tabella di smartmontools: negli
SSD Crucial e Micron il 202 è la vita consumata in percentuale, con la vita residua nel dettaglio e un
avviso dal 90 % come l'usura degli NVMe, e il 206 è il tasso di errori di scrittura; negli altri SSD
(Samsung e SK hynix usano il 202 per lo stato di eccezione, Intel per la fine vita, Lexar il 206 per il
minimo dei cicli di cancellazione…) compaiono come «Attributo del produttore».
Se al posto di un disco ne compare un altro con lo stesso numero e la stessa lettera (per esempio
staccando un contenitore USB e collegandone un altro), la finestra se ne accorge alla rilettura
dell'elenco dei dischi, ogni 4 secondi: toglie lo SMART e le note del disco di prima e legge subito il
nuovo. Con la finestra nascosta nell'area di notifica l'elenco si rilegge prima di ogni lettura periodica.
Un disco o un volume che non risponde (per esempio un contenitore USB con il bridge bloccato) non ferma il
programma: nella lettura dell'elenco ogni disco e ogni volume si interroga per conto suo, con al massimo
2,5 secondi per rispondere e una sola domanda in sospeso alla volta; chi tace resta com'era alla lettura
precedente e dopo 8 secondi è segnato «non risponde» nella colonna a sinistra, nel grafo e in `--list`. A
un disco così non si mandano comandi SMART, la scheda Info dice solo che non risponde, il monitoraggio lo
segnala con un avviso, e alla chiusura la finestra non aspetta il lavoro rimasto fermo su di lui. Windows
però non lascia terminare un programma che ha una richiesta in sospeso verso un dispositivo: finché il
disco non viene scollegato il processo può restare nel Task Manager, anche dopo «Termina attività».

**Eventi di sistema**: gli errori e gli avvisi di dischi, controller e file system registrati da Windows
nelle ultime 24 ore, 7 giorni o 30 giorni, con una spiegazione per i codici più comuni. «Aggiorna»
aggiunge solo gli eventi nuovi. Per le operazioni di I/O ritentate (153) o fallite (154) il programma
legge anche i dati binari dell'evento e dice quale comando il disco ha ricevuto e cosa ha risposto, per
esempio «scrittura (WRITE 10): errore hardware, guasto interno del dispositivo [4/44/81]» o «lettura
(READ 10): scaduto il tempo: il disco non ha risposto [SRB 09]»; fra le segnalazioni più frequenti il
comando è contato a parte, così si vede subito se falliscono le letture, le scritture o tutto.

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

Il file va nella radice del volume. Se la radice non accetta file, va nella cartella temporanea o in quella
dell'utente quando stanno sullo stesso volume (è il caso del disco di sistema). Se nemmeno questo è
possibile, perché la radice accetta solo cartelle (i permessi del disco di sistema, che si ritrovano su
dischi preparati allo stesso modo), il file va in una cartella nascosta creata lì per la prova, aperta
anche lei perché Windows la elimini alla chiusura; a fine prova si controlla che sia sparita anche la
cartella. Se il programma viene terminato, Windows chiude gli handle in ordine di numero: quello della
cartella viene aperto dopo quelli del file, così si chiude per ultimo, quando la cartella è già vuota.
Se una cartella vuota restasse comunque, la prova successiva sullo stesso volume la elimina. Se le
prove sul file non possono partire (spazio insufficiente, permessi), la prova finisce «Completato con
avvisi» e non «Nessun errore».

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
il comando di avvio dell'autodiagnosi, tranne il JMS581 che si blocca. Per Realtek e ASMedia è noto solo un
comando proprietario di lettura (E4h e E6h, gli stessi di smartmontools), che porta il codice del comando
NVMe ma non il namespace e prevede sempre dati in arrivo: l'avvio dell'autodiagnosi non ha una strada
documentata. `--selftest X: breve --realtek` prova comunque a mandarlo attraverso il comando dei Realtek,
prima senza dati e poi come lettura, e controlla nel registro del disco se la prova è partita. Un RTL9210B
(0BDA:9210, firmware 1.00) e un RTL9210C (3535:9210, firmware 1.01) lo rifiutano in entrambe le forme come
«comando non valido» (codice 20/00), subito e senza conseguenze: il bridge controlla il codice del comando
NVMe e quello dell'autodiagnosi non lo accetta. Molti contenitori Realtek hanno l'identificativo USB del
produttore del contenitore invece di quello di Realtek (0BDA), come quel RTL9210C: se il dispositivo si
dichiara un SSD, il programma prova comunque le letture ASMedia e Realtek e tiene quella che risponde.

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
mystoragetest --graph             il Grafo Storage come testo: collegamenti, bande teoriche, driver
mystoragetest --smart [N]         stato SMART di tutti i dischi o del disco N
mystoragetest --diagnosi [FILE]   comandi inviati a ogni disco per leggere lo SMART e risposte ricevute
mystoragetest --events [ORE]      errori di disco nel registro di sistema (predefinito: 24 ore), con il riepilogo per tipo
mystoragetest --selftest [X:|N]   autodiagnosi dei dischi: stato e prove registrate
mystoragetest --selftest X:|N breve|estesa|stop
                                  avvia o interrompe l'autodiagnosi del disco del volume X: o del
                                  disco N (da amministratore), con i comandi inviati e i loro tempi
mystoragetest --selftest X:|N breve|estesa|stop --realtek
                                  prova sperimentale: lo stesso attraverso un bridge USB-NVMe Realtek,
                                  con un comando non documentato per questo uso
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
    --queue LISTA      gruppi della tabella con coda Q1-Q32, per esempio 4K,256K,1M (predefinito: tutti)
    --queue-secs S     durata di ogni misura della tabella con coda, in secondi (predefinito 1)
    --no-queue         salta la tabella per blocco e profondità della coda
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
- test completo su una chiavetta USB la cui radice accetta solo cartelle: il file di prova va in una cartella nascosta creata per la prova, e alla fine non restano né il file né la cartella, anche chiudendo a forza il programma durante la scrittura e durante le letture con coda; su un volume senza spazio per il file la prova finisce «Completato con avvisi»;
- tabella per blocco e profondità della coda su un NVMe PCIe 4.0 (lettura in sequenza 7,35 GB/s e scrittura 6,4 GB/s, come da specifica, contro 4,7 e 4,9 a Q1), su un volume distribuito su due NVMe (8,9 GB/s in lettura), su un volume cifrato VeraCrypt (circa 2 GB/s a ogni Q), su un disco a piatti USB (4K casuale da 174 operazioni al secondo a Q1 a 663 a Q32) e su una chiavetta USB: nessuno stallo falso con le code lunghe sui dischi lenti, file di prova eliminato; riquadri, tabella e fumetti con la finestra alla dimensione normale e a quella minima (dove i riquadri vanno su due righe);
- due prove contemporanee su due volumi, ognuna con la sua pagina, che resta dopo aver cambiato disco;
- SMART senza privilegi su sei NVMe e due SATA interni; sul Crucial MX500 l'attributo 202 (valore grezzo 24, attuale 76) letto come vita consumata 24 % e vita residua 76 %, coerente con i 373 cicli di cancellazione medi, e il 206 come tasso di errori di scrittura, confrontati con la voce «Crucial/Micron Client SSDs» della tabella di smartmontools; il riconoscimento dei modelli Crucial e Micron ha una prova automatica;
- autodiagnosi senza privilegi: disponibilità, durate dichiarate e prove registrate su sei NVMe; disponibilità ed esito dell'ultima prova su due SATA;
- lettura degli eventi di sistema, compresa una raffica reale di circa 38.000 errori di un disco USB, con i testi confrontati uno a uno con quelli di Windows;
- autodiagnosi attraverso un bridge JMicron JMS581 (152D:0581), da amministratore, con `--selftest H: breve`: le letture dello SMART attraverso il bridge rispondono in circa 200 ms, il comando di avvio viene accettato, ma la sua esecuzione non finisce, né in 8 né in 60 secondi; Windows reimposta il bridge, che va scollegato e ricollegato (sette tentativi, a 10 e a 5 Gbit/s). Da qui il blocco della prova sui JMS581;
- autodiagnosi attraverso un bridge Realtek RTL9210B (0BDA:9210, firmware 1.00), da amministratore, con `--selftest H: breve --realtek`: le letture attraverso il bridge (registro SMART, Identify, registro delle prove) rispondono in 2–3 ms; il comando di autodiagnosi dentro il comando E4h viene rifiutato come «comando non valido» (20/00) sia senza dati sia come lettura, in 1–2 ms, e il disco resta collegato;
- SMART e autodiagnosi attraverso un bridge Realtek RTL9210C con l'identificativo USB di un altro produttore (3535:9210, firmware 1.01, USB 3.2 Gen 2 con UAS), da amministratore: SAT e lettura ASMedia vengono rifiutate subito, la lettura Realtek risponde in 1–9 ms con SMART, Identify e registro delle prove di un SSD Kingston; il comando di autodiagnosi viene rifiutato come sull'RTL9210B, in 0 ms, senza eventi di errore nel registro di sistema;
- disco sostituito da un altro con lo stesso numero e la stessa lettera, con la verifica automatica `MYSTORAGETEST_AUTOTEST=sostituzione`: al disco del volume scelto vengono date identità e SMART finti di un contenitore JMicron JMS581, con la lettura periodica rimandata di dieci minuti; alla rilettura dell'elenco dei dischi lo SMART finto sparisce e quello vero compare dopo circa 4 secondi. Con la finestra nascosta nell'area di notifica il programma resta fermo (0,14 s di processore in 70 secondi) e si chiude con `--quit`;
- blocco e avvertenza per l'autodiagnosi attraverso i bridge JMicron USB-NVMe: prove automatiche su quali bridge sono bloccati (JMS581, 0581 e 1581) e quali hanno l'avvertenza (JMS583, JMS586 e gli altri PID JMicron USB-NVMe; non i bridge JMicron USB-SATA, Realtek e i dischi non USB), e i due testi verificati a schermo nella scheda SMART su un disco interno con un bridge simulato (`MYSTORAGETEST_FAKE_JMICRON=0581` e `=0583`);
- decodifica di comando e risposta negli eventi 153 e 154, con prove automatiche (`cargo test`) sui dati di eventi veri e confrontata con la decodifica a mano di circa 100.000 eventi reali: un SSD NVMe in contenitore USB con bridge JMicron che a tratti smette di rispondere (letture, scritture, svuotamento della cache, verifica di prontezza e lettura della capacità, tutte con errore hardware 4/44/81) e dischi USB con letture ritentate per tempo scaduto o interrotte;
- storico delle temperature degli SSD interni;
- scansione della superficie eseguita su un file al posto del volume: arresto a metà, punto ricordato e riproposto anche dopo la chiusura del programma, ripresa fino in fondo, tratto parziale, grafici di velocità e latenza, zoom con rotellina e tasti, scelta di un tratto trascinando (con un puntatore simulato) e sua rilettura con «Riscandisci»;
- monitoraggio: avvio nascosto, icona, notifica (con un avviso simulato), consumo nullo a riposo, chiusura con `--quit`;
- fumetti dei grafici (linea verticale, prova, valore e spiegazione) con un puntatore simulato; sui grafici di velocità e latenza uno sopra l'altro, il segno e la fascia della misura sullo stesso istante di entrambi e, nelle prove con coda, la coda della misura (Q2 con blocchi da 1M in sequenza, Q4 con blocchi da 256K a caso), confrontata con gli intervalli registrati;
- «Riavvia come amministratore» e l'avvio diretto da amministratore (`--admin` e «Avvia sempre come amministratore»): la sequenza di rilancio, provata senza elevazione;
- SMART da amministratore di un SSD NVMe in contenitore USB con bridge JMicron (152D:0581): la lettura riesce (prova fatta a mano dall'interfaccia);
- SMART da amministratore, dal «Rapporto di diagnosi»: due SATA interni con soglie e registro completo delle autodiagnosi; due dischi a piatti Seagate su USB (0BC2:3322 e 0BC2:61B7, UASP) con SAT a 12 byte, soglie, registro delle autodiagnosi e risposta alla domanda «il disco gira?»; lo stesso bridge JMicron senza un disco riconosciuto, che risponde a ogni comando con un errore dell'hardware e viene riportato come tale;
- comandi diretti a due chiavette USB (una su UASP, una no), che Windows lascia passare anche senza privilegi: i comandi SAT partono, vengono rifiutati come previsto e le chiavette risultano senza SMART, senza altri tentativi;
- scheda Info e `--info` su tredici dischi (NVMe e SATA interni, un volume distribuito su due dischi, dischi USB, chiavette, un disco virtuale) e sui volumi senza un disco fisico: partizioni GPT e MBR con le lettere al loro posto, settori, TRIM, cache in scrittura; velocità USB coerente con gli hub attraversati (dischi a 5 Gbit/s dietro un hub USB 3, una chiavetta USB 3 riconosciuta come limitata a 480 Mbit/s dietro un hub USB 2); per ogni disco USB il dispositivo a cui «Espelli» chiederebbe la rimozione, confrontato con l'albero dei dispositivi di Windows;
- interfaccia e collegamento attuale senza privilegi: sei NVMe (versione NVMe e PCIe confrontate con quelle che Windows riporta per il dispositivo; uno PCIe 4.0 x4 collegato a x1 viene segnalato), due SATA interni a 6 Gbit/s con la revisione SATA 3.3, cinque dispositivi USB con la velocità e il tipo di porta (USB 2 o 3, USB-C);
- Grafo Storage su un PC con Ryzen 9 3900X e chipset X570 e tredici dischi, confrontato con l'albero dei dispositivi di Windows: due NVMe sulle porte PCIe del processore, tre su porte del chipset (quello a PCIe 4.0 x1 in giallo), due SATA sul controller AHCI del chipset con la loro porta, i dischi USB sui due controller USB del chipset attraverso fino a tre hub in cascata, un disco virtuale e quattro volumi senza disco fisico; alla dimensione normale e grande della finestra. Poi gli altri canali PCIe (scheda grafica a PCIe 4.0 x8 su 16 linee possibili con il suo audio HDMI, rete Intel I211, audio e USB interni al processore, un controller SATA senza dischi), le bande teoriche e i fumetti delle linee, i colori per tipo di collegamento, le icone dei tipi di disco anche nella colonna, il volume a strisce sui due NVMe riconosciuto come tale da Windows, tre contenitori VeraCrypt collegati ai volumi che contengono i loro file (uno dei quali è proprio il volume a strisce), un volume di Google Drive con il suo driver di file system, e l'evidenziazione reciproca fra grafo e colonna (con un puntatore simulato); `--graph` stampa lo stesso grafo come testo. La vista del grafo con eventi simulati: rotellina e tasto + ingrandiscono attorno al puntatore, le frecce spostano, 0 torna alla vista iniziale, e un trascinamento che parte sopra un disco sposta la vista senza aprirlo. Gli identificativi di porte, controller, NVMe, hub radice, hub e dispositivi USB e volumi, confrontati con le proprietà che Windows dà per ciascun dispositivo (ID istanza, indirizzi PCI, percorsi di posizione, nomi ACPI delle porte del processore e del chipset); il clic destro che li copia non è nelle verifiche automatiche, per non toccare gli appunti.
- disco che non risponde: il caso reale di un contenitore Realtek RTL9210B bloccato (più di 1.000 letture ritentate e 12 reimpostazioni del bridge in un quarto d'ora), con cui la versione precedente non apriva la finestra, non si chiudeva e non elencava i dischi; poi, con la simulazione `MYSTORAGETEST_FAKE_STUCK=8,L` (domande al disco 8 e al volume L: che non tornano mai, dall'avvio o dopo qualche secondo con `@3`): `--list` risponde in 2,6 secondi invece di restare fermo, la finestra mostra la prima schermata in 3,3 secondi e completa la sequenza di verifica, con il disco e il volume segnati «non risponde» (dall'avvio solo con il numero e la lettera, dopo con i dati letti prima), il grafo intatto e la chiusura da sola; senza simulazione nulla cambia (`--list` in 0,4 secondi, 0,4 s di processore in 40 secondi a finestra aperta);
- bridge dei contenitori USB nel Grafo Storage, senza privilegi: un Realtek RTL9210B-CG (0BDA:9210) a 10 Gbit/s con UAS e due dischi Seagate (0BC2:3322 e 0BC2:61B7) dietro un hub USB 3, ciascuno con il suo bridge fra l'hub e il disco; il nome del chip e il prodotto USB confrontati con le proprietà di Windows. Il tipo di disco dietro un bridge che sa collegare sia NVMe sia SATA si conosce dopo la lettura dello SMART, che senza privilegi non c'è; chiavette e lettori di schede restano senza bridge;

Non ancora verificato:

- **collegamento SATA dentro un contenitore USB**: si legge solo da amministratore, attraverso il bridge;
- **«Espelli»**: la richiesta a Windows è quella documentata per la rimozione sicura e il dispositivo a cui va è stato controllato, ma l'espulsione vera non è stata eseguita, per non scollegare i dischi in uso; l'espulsione del solo supporto (lettori con più alloggiamenti, supporti rimovibili non USB) non ha avuto un dispositivo su cui girare;
- **avvio e interruzione dell'autodiagnosi**, sui dischi interni e attraverso i bridge diversi dal JMS581: richiedono l'amministratore; i comandi sono scritti secondo le specifiche NVMe e ATA e la documentazione di Windows, ma non sono stati eseguiti;
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
| `src/topology.rs` | Grafo Storage: catena di ogni disco e dispositivo PCI nell'albero dei dispositivi, fino al processore; bande teoriche, volumi su più dischi, contenitori cifrati, driver dei volumi senza disco |
| `src/icons.rs` | iconcine disegnate a vettori: tipi di disco e componenti del grafo |
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
`MYSTORAGETEST_VOL2`, `MYSTORAGETEST_NOACTIVATE`, `MYSTORAGETEST_POS`, `MYSTORAGETEST_SIZE` e `MYSTORAGETEST_FAKE_ALERT` servono
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

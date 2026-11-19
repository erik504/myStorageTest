# myStorageTest

Test, diagnosi e monitoraggio dei dischi per Windows 10 e 11, in un solo eseguibile scritto in Rust.

Nasce da un caso reale: un SSD in un contenitore USB che per ore ha risposto con errori hardware a
ogni scrittura senza che nessuno se ne accorgesse. Il programma serve a due cose: provare un disco
quando si ha un dubbio, e accorgersi subito quando un disco comincia a dare errori.

## Cosa fa

**Test** sul volume scelto, con grafici in tempo reale di velocità, latenza e distribuzione delle latenze:

- scrittura e lettura sequenziale, con verifica che i dati riletti siano identici a quelli scritti;
- lettura e scrittura casuale a blocchi da 4 KiB (operazioni al secondo, latenza media, percentili, massimo);
- flush su disco, misurato operazione per operazione;
- lettura dei file già presenti sul volume, in sola lettura, per trovare settori illeggibili;
- lettura grezza dell'intera superficie del volume (solo da amministratore);
- controllo degli errori di disco comparsi nel registro di sistema durante la prova.

Ogni errore di I/O viene classificato: supporto danneggiato (CRC, settore non trovato), errore hardware
del dispositivo, collegamento (reset del bus), timeout, disco scollegato, file system danneggiato, dati
corrotti (rilettura diversa dalla scrittura), spazio esaurito, sola lettura. Un'operazione che resta
bloccata viene segnalata mentre è ancora in corso, e «Ferma» la annulla.

**SMART** di ogni disco, con gli attributi in chiaro e i valori anomali evidenziati.

**Eventi di sistema**: gli errori e gli avvisi di dischi, controller e file system registrati da Windows
nelle ultime 24 ore, 7 giorni o 30 giorni, con una spiegazione per i codici più comuni.

**Monitoraggio**: un'icona nell'area di notifica sorveglia in tempo reale gli errori di disco nel registro
di sistema e rilegge periodicamente lo SMART di tutti i dischi. Se compare un problema l'icona cambia
colore e arriva una notifica sul desktop con i dettagli; ogni avviso finisce anche in
`%LOCALAPPDATA%\myStorageTest\monitor.log`.

## Nessuna traccia sul disco provato

Le prove in scrittura usano un solo file temporaneo nascosto, aperto con `FILE_FLAG_DELETE_ON_CLOSE`:
Windows lo elimina quando l'handle si chiude, anche se il programma viene terminato a metà. A fine prova
il programma controlla che il file non esista più e lo dichiara.

La lettura dei file esistenti non modifica nulla, nemmeno la data di ultimo accesso, e salta i segnaposto
dei servizi cloud per non farli scaricare. Impostazioni e registro degli avvisi stanno sul disco di
sistema, mai sui dischi provati.

Resta inevitabile ciò che il file system annota da sé quando un file viene creato e cancellato (voci del
journal NTFS).

## Cosa serve l'amministratore

| Funzione | Utente normale | Amministratore |
|---|---|---|
| Test su file, lettura dei file esistenti | sì | sì |
| Eventi di sistema e monitoraggio degli errori | sì | sì |
| SMART dei dischi NVMe interni | sì | sì |
| SMART dei dischi SATA interni | sì, senza soglie | sì, con soglie |
| SMART dei dischi USB | no | sì |
| Scansione della superficie | no | sì |

«Riavvia come amministratore» in basso a sinistra rilancia il programma con i privilegi necessari.

Per i dischi USB i comandi passano attraverso il bridge: SAT per i contenitori SATA, protocolli
proprietari per i contenitori NVMe con bridge JMicron (JMS583/JMS581), Realtek (RTL9210) e ASMedia
(ASM236x). I comandi proprietari vengono inviati solo ai bridge riconosciuti dal loro identificativo USB.

## Riga di comando

```
mystoragetest                     apre l'interfaccia
mystoragetest --monitor           avvia nascosto nell'area di notifica, con il monitoraggio attivo
mystoragetest --quit              chiude l'istanza in esecuzione
mystoragetest --list              elenca volumi e dischi
mystoragetest --smart [N]         stato SMART di tutti i dischi o del disco N
mystoragetest --events [ORE]      errori di disco nel registro di sistema (predefinito: 24 ore)
mystoragetest --test X: [opzioni] esegue le prove sul volume X:
    --size MB    dimensione del file di prova (predefinito 256)
    --secs N     durata delle prove casuali (predefinito 5)
    --scan MB    legge anche i file esistenti fino a MB (0 = tutti)
    --surface    scansione dell'intera superficie (solo da amministratore)
    --no-write   salta le prove in scrittura
```

`--test` esce con 0 se non ci sono errori, 1 con avvisi, 2 con errori, 3 se interrotto.

«Avvia con Windows» nella scheda Monitoraggio registra `mystoragetest --monitor` per l'utente corrente.

## Compilare

Serve Rust stabile con il toolchain MSVC.

```
cargo build --release
```

L'eseguibile è `target\release\mystoragetest.exe`, senza altre dipendenze da installare.

## Stato delle verifiche

Provato su Windows 10 22H2 con dischi NVMe e SATA interni e con dischi USB:

- test completi su un NVMe interno e su un SSD NVMe in contenitore USB, con controllo che il file di prova sparisca;
- SMART senza privilegi su sei NVMe e due SATA interni;
- lettura degli eventi di sistema, compresa una raffica reale di circa 38.000 errori di un disco USB;
- monitoraggio: avvio nascosto, icona, notifica (con un avviso simulato), consumo nullo a riposo, chiusura con `--quit`.

Non ancora verificato:

- **SMART dei dischi USB** e **SMART SATA con soglie**: richiedono l'amministratore; i comandi sono scritti secondo i protocolli documentati ma non sono stati eseguiti su hardware reale;
- **scansione della superficie**: richiede l'amministratore;
- riapertura della finestra dall'icona e chiusura nell'area di notifica;
- comportamento davanti a errori di I/O veri: sui dischi sani a disposizione non si sono presentati;
- Windows 11: le API usate sono le stesse, ma il programma non vi è stato eseguito.

I numeri dei dischi fisici cambiano dopo un riavvio o un ricollegamento: per questo gli eventi di sistema
precedenti all'ultimo avvio vengono attribuiti a un disco solo tramite la lettera del volume.

## Struttura del codice

| File | Contenuto |
|---|---|
| `src/disks.rs` | elenco di volumi e dischi fisici, bridge USB |
| `src/tester.rs` | motore delle prove di I/O |
| `src/errors.rs` | classificazione dei codici d'errore |
| `src/smart.rs` | lettura SMART (NVMe, ATA, pass-through USB) |
| `src/eventlog.rs` | registro eventi di Windows: query e sottoscrizione |
| `src/monitor.rs` | monitoraggio continuo e avvisi |
| `src/tray.rs` | icona nell'area di notifica e notifiche |
| `src/ui.rs` | interfaccia (egui) e grafici |
| `src/cli.rs` | modalità a riga di comando |

Le variabili d'ambiente `MYSTORAGETEST_SHOT`, `MYSTORAGETEST_AUTOTEST`, `MYSTORAGETEST_VOL`,
`MYSTORAGETEST_NOACTIVATE`, `MYSTORAGETEST_POS` e `MYSTORAGETEST_FAKE_ALERT` servono alle verifiche
automatiche dell'interfaccia: eseguono una prova, salvano le schermate di ogni scheda e chiudono.

# myStorageTest

Test, diagnosi e monitoraggio dei dischi per Windows 10 e 11, in un solo eseguibile scritto in Rust.

Nasce da un caso reale: un SSD in un contenitore USB che per ore ha risposto con errori hardware a
ogni scrittura senza che nessuno se ne accorgesse. Il programma serve a due cose: provare un disco
quando si ha un dubbio, e accorgersi subito quando un disco comincia a dare errori.

## Cosa fa

La colonna a sinistra elenca i dischi, ognuno con la temperatura e il pallino dello stato SMART; in cima
c'è la voce **Monitoraggio**, con la casella che lo accende e lo spegne. Ogni disco ha quattro schede.

**Test** sul volume scelto, con grafici in tempo reale di velocità, latenza e distribuzione delle latenze
(passando il mouse su un grafico un fumetto dice a quale prova appartiene il punto, il valore e che cosa
significa):

- scrittura e lettura sequenziale, con verifica che i dati riletti siano identici a quelli scritti;
- lettura e scrittura casuale a blocchi da 4 KiB (operazioni al secondo, latenza media, percentili, massimo);
- flush su disco, misurato operazione per operazione;
- lettura dei file già presenti sul volume, in sola lettura, per trovare settori illeggibili;
- lettura grezza dell'intera superficie del volume (solo da amministratore), su un grafico a parte:
  velocità lungo il disco, con i blocchi illeggibili segnati;
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

**Temperatura**: lo storico della temperatura del disco da quando il programma è aperto. Gli SSD interni
vengono letti ogni 30 secondi; i dischi USB e quelli a piatti solo all'intervallo SMART del monitoraggio,
perché una lettura più frequente potrebbe impedire loro di andare a riposo.

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

«Riavvia come amministratore» in basso a sinistra rilancia il programma con i privilegi necessari.

Per i dischi USB i comandi passano attraverso il bridge: SAT per i contenitori SATA, protocolli
proprietari per i contenitori NVMe con bridge JMicron (JMS583/JMS581), Realtek (RTL9210) e ASMedia
(ASM236x). I comandi proprietari vengono inviati solo ai bridge riconosciuti dal loro identificativo USB.
Fra i bridge USB-NVMe solo JMicron inoltra il comando di avvio dell'autodiagnosi; Realtek e ASMedia
inoltrano soltanto le letture.

L'autodiagnosi degli NVMe interni passa dal driver di Windows, che accetta il comando da Windows 10 1903.

## Riga di comando

```
mystoragetest                     apre l'interfaccia
mystoragetest --monitor           avvia nascosto nell'area di notifica, con il monitoraggio attivo
mystoragetest --quit              chiude l'istanza in esecuzione
mystoragetest --list              elenca volumi e dischi
mystoragetest --smart [N]         stato SMART di tutti i dischi o del disco N
mystoragetest --events [ORE]      errori di disco nel registro di sistema (predefinito: 24 ore)
mystoragetest --selftest [N]      autodiagnosi dei dischi: stato e prove registrate
mystoragetest --selftest N breve|estesa|stop
                                  avvia o interrompe l'autodiagnosi del disco N (da amministratore)
mystoragetest --check-events [N]  confronta la lettura veloce degli eventi con i testi di Windows
mystoragetest --test X: [opzioni] esegue le prove sul volume X:
    --size MB    dimensione del file di prova (predefinito 256)
    --secs N     durata delle prove casuali (predefinito 5)
    --scan MB    legge anche i file esistenti fino a MB (0 = tutti)
    --surface    scansione dell'intera superficie (solo da amministratore)
    --no-write   salta le prove in scrittura
```

`--test` esce con 0 se non ci sono errori, 1 con avvisi, 2 con errori, 3 se interrotto.

«Avvia con Windows» nella pagina Monitoraggio registra `mystoragetest --monitor` per l'utente corrente.
L'avvio automatico parte senza privilegi di amministratore: in quel caso lo SMART dei dischi USB non
viene sorvegliato e per quei dischi restano gli errori del registro di sistema.

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
- monitoraggio: avvio nascosto, icona, notifica (con un avviso simulato), consumo nullo a riposo, chiusura con `--quit`;
- fumetti dei grafici (linea verticale, prova, valore e spiegazione) con un puntatore simulato;
- «Riavvia come amministratore»: la sequenza di chiusura e riapertura, provata senza elevazione;
- SMART da amministratore di un SSD NVMe in contenitore USB con bridge JMicron (152D:0581): la lettura riesce (prova fatta a mano dall'interfaccia).

Non ancora verificato:

- **avvio e interruzione dell'autodiagnosi**, su qualunque disco: richiedono l'amministratore; i comandi sono scritti secondo le specifiche NVMe e ATA e la documentazione di Windows, ma non sono stati eseguiti;
- **registro completo delle autodiagnosi dei dischi SATA** e **autodiagnosi dei dischi USB**: richiedono l'amministratore;
- **SMART dei dischi USB con bridge SAT, Realtek e ASMedia** e **SMART SATA con soglie**: richiedono l'amministratore; i comandi sono scritti secondo i protocolli documentati ma non sono stati eseguiti su hardware reale;
- **scansione della superficie**: richiede l'amministratore; il suo grafico è stato provato solo con dati inventati;
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
| `src/tester.rs` | motore delle prove di I/O |
| `src/errors.rs` | classificazione dei codici d'errore |
| `src/smart.rs` | SMART e autodiagnosi (NVMe, ATA, pass-through USB) |
| `src/eventlog.rs` | registro eventi di Windows: query, sottoscrizione, testi veloci |
| `src/monitor.rs` | monitoraggio continuo e avvisi |
| `src/tray.rs` | icona nell'area di notifica e notifiche |
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

## Licenza

[MIT](LICENSE).

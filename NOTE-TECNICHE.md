# Note tecniche

Cose imparate sviluppando myStorageTest che non stanno nel README: dettagli delle API di Windows, formati
dei comandi, comportamenti osservati sull'hardware vero, insidie delle librerie e problemi già risolti.
Le regole di lavoro sono in [CLAUDE.md](CLAUDE.md).

## Interfaccia: eframe/egui 0.37 ed egui_plot 0.38

### API e cambi di nome
- eframe 0.37 divide il lavoro in due funzioni:
  - `App::logic(&mut self, ctx, frame)`, che gira anche a finestra nascosta se c'è un `request_repaint`;
  - `App::ui(&mut self, ui, frame)`.
- I pannelli: `egui::Panel::left(..).show(ui, ..)` e `CentralPanel::default().show(ui, ..)`.
- Le schermate si fanno con `ctx.request_screenshot(cb)`.
- L'input simulato per le prove passa da `App::raw_input_hook`.
- Nomi deprecati o spariti, e cosa usare:
  - `ui.id()` → `ui.scope_id()`;
  - `Id::new` → `Id::unique`;
  - `show_inside` → `show`;
  - `wants_keyboard_input` → `egui_wants_keyboard_input()`;
  - `ctx.screen_rect()` non c'è più → `ctx.content_rect()`.
- `Grid::new(id)` e `Plot::new(id)` vogliono un id `Hash + Debug` (`impl egui::AsIdSalt`).
- Fumetti in una posizione scelta:
  `egui::Tooltip::always_open(ctx, layer, id, PopupAnchor::ParentRect(rect))`, con
  `tip.popup.gap().align(RectAlign::RIGHT_START).align_alternatives(..)`, oppure
  `PopupAnchor::Pointer`.
- Per il grafo: `Sense::CLICK | Sense::DRAG` (bitflag, non prende il fuoco), `ctx.copy_text` per gli
  appunti e `ui.scroll_to_rect_animation(rect, None, ScrollAnimation::none())` per portare un riquadro
  in vista.

### Comportamenti da sapere
- **eframe mostra sempre la finestra dopo il primo fotogramma**: ignora `with_visible(false)`.
  - Per partire nascosti (`--monitor`): si crea la finestra a (-32000, -32000) con
    `with_active(false)` e la si nasconde con `ViewportCommand::Visible(false)` al primo giro
    (`pending_hide`).
  - Verificato con EnumWindows: prima della correzione la finestra restava visibile per circa 10 s.
- **I fumetti di egui compaiono solo con il puntatore fermo.** Il puntatore simulato deve mandare
  `PointerMoved` solo quando cambia posizione, non a ogni fotogramma.
- Eventi simulati usati dalle sequenze:
  - `PointerMoved`;
  - `MouseWheel { unit, delta, phase: TouchPhase::Move, source: MouseWheelSource::Wheel, modifiers }`;
  - `Key { key, physical_key, pressed, repeat, modifiers }`;
  - `PointerButton { pos, button, pressed, modifiers }`, con pressione e rilascio in fotogrammi
    diversi.
- **In debug, emath va in panico su posizioni NaN** («Comparing NaN vectors»). Il puntatore simulato
  non va messo al centro di `Rect::NOTHING`: controllare `rect.is_positive()`.
- Una `ScrollArea` consuma `input.smooth_scroll_delta`. Per prendere la rotellina dentro un grafico:
  `std::mem::take(&mut i.smooth_scroll_delta)` quando il puntatore è sopra.
- `horizontal_wrapped` spezza un'icona dalla sua etichetta. Per tenerle insieme:
  - confrontare lo spazio con `ui.available_size_before_wrap().x`, non con `available_width()`, che
    è la larghezza intera;
  - se non basta, `ui.end_row()`;
  - etichetta con `TextWrapMode::Extend`.
- Testi:
  - il testo giustificato nei layout verticali si evita con `with_layout(Layout::top_down(Align::Min))`;
  - un registro lungo va in un'unica `LayoutJob` con `set_max_width`;
  - le colonne allineate si fanno con `Grid` o con celle `allocate_ui_with_layout`.
- **Glifi che il carattere non ha** (Segoe UI): `ⓘ` e `⚠` escono come quadratini.
  - La lente è disegnata a mano.
  - Gli avvisi cominciano con la parola «Attenzione.», con il punto: con i due punti verrebbe
    «Attenzione: Con…».
- Le schermate sono in scala 1,25: una finestra di 1240×820 punti diventa 1550×1025 pixel.
- La finestra predefinita (1240×820, minima 1040×720) lascia ai grafici circa 380 punti di altezza.
  Ogni impaginazione va controllata anche lì.

### egui_plot
- Il grafico ha per definizione `Sense::click_and_drag()`, quindi `drag_started_by/dragged_by/
  drag_stopped_by(Primary)` funzionano anche con `allow_drag(false)`. Lo spostamento è sul tasto
  destro (`pan_pointer_button(Secondary)`), perché il trascinamento sinistro sceglie un tratto.
- **Assi collegati** (`link_axis(Id, [true, false])`):
  - `plot_bounds()` di ogni grafico restituisce i suoi limiti del fotogramma prima;
  - i limiti del gruppo li scrive l'ultimo grafico disegnato.
  - Quindi lo zoom da tastiera va gestito da **un solo** grafico, e la rotellina solo da quello sotto
    il puntatore.
  - Velocità e latenza della scheda Test si collegano anche con i rettangoli del fotogramma prima.
- `Span::new(nome, range)` scrive il nome dentro la fascia: si usa `""`.
- Le etichette dell'asse Y spariscono se i segni sono a meno di 20 punti, e su un grafico basso si
  vede solo «0». Per questo ci sono spaziature fatte in casa, `round_marks` e `log_marks`.
  `GridInput.base_step_size` è il valore che corrisponde a 8 punti dello schermo.
- `draw_series` spezza la linea dove c'è un salto più lungo di 6 passi, per non disegnare diagonali
  finte.

### Colori e scelte grafiche
- Il tema è scuro, con la palette `BG/PANEL/CARD/ACCENT/GOOD/WARN/BAD` e i caratteri Segoe UI (più una
  famiglia «semibold»).
- Colori dei collegamenti nel grafo:

  | Collegamento | RGB |
  |---|---|
  | PCIe | 79,140,255 |
  | SATA | 45,212,191 |
  | USB | 192,132,252 |
  | software (volumi, contenitori) | 148,160,180 |
  | video, HD Audio, LPC e ACPI (grafo completo) | 244,114,182 |

- La latenza della superficie è disegnata in 34,211,238.
- Icona: proposta 7 «Pila», variante B «tagli scuri». È una pila di dischi a strati, verde 38,196,133,
  disegnata a vettori in `icon.rs` con sovracampionamento 4×4 e tratti più spessi sotto i 24 px.
  - `build.rs` scrive a mano un file `.res` (RT_ICON 3, RT_GROUP_ICON 14, dimensioni da 16 a 256) e
    lo passa al linker con `cargo:rustc-link-arg-bins=`. Non servono file .ico né rc.exe.

## Motore delle prove (`tester.rs`)

### Ordine delle fasi
1. Scrittura in sequenza.
2. Lettura in sequenza.
3. Letture con coda.
4. Lettura casuale.
5. Scrittura casuale, seguita dalla verifica.
6. Flush, seguito dalla verifica.
7. Scritture con coda.
8. Lettura dei file esistenti.
9. Lettura a inizio, metà e fine del disco.
10. Superficie.

Note sull'ordine:
- La verifica, cioè la rilettura dei blocchi scritti, è una fase a sé, «Verifica». Spiega l'intervallo
  fra scrittura casuale e flush che l'utente aveva notato.
- L'ordine è stato discusso con l'utente ed è voluto.
- Le letture con coda stanno subito dopo la lettura in sequenza, sul file appena scritto. Dopo le
  scritture casuali risultavano più lente.
- Le scritture con coda vanno per ultime.

### Nessuna traccia
- Il file di prova è `.mystoragetest-<pid>-<seed>.tmp` nella radice del volume.
  - È aperto con NO_BUFFERING, DELETE_ON_CLOSE, HIDDEN e NOT_INDEXED.
  - Dopo la chiusura si ricontrolla che non esista più.
- Se la radice accetta solo cartelle (una chiavetta con ACL particolari), il file va in una cartella
  nascosta creata apposta, con DELETE_ON_CLOSE anche sulla cartella.
  - Chiudendo a forza un processo, Windows chiude gli handle in ordine di valore. L'handle della
    cartella deve quindi avere un valore **più alto** di quelli del file, altrimenti la cartella vuota
    resta. Per questo la cartella si riapre nelle fasi con coda.
  - Le cartelle rimaste da un'esecuzione uccisa vengono tolte all'avvio successivo.
- La lettura dei file esistenti salta reparse point e segnaposto del cloud e conserva l'ora
  dell'ultimo accesso con `SetFileTime(…, 0xFFFFFFFF)`.

### I/O
- I dati sono generati con splitmix64, a partire da seme, blocco e generazione. Ogni blocco da 4K ha la
  sua generazione, così la verifica scopre anche dati vecchi o finiti nel posto sbagliato.
- L'I/O sincrono posizionato passa da OVERLAPPED. Per fermarlo, `CancelSynchronousIo`.
- Uno stallo viene segnalato dopo 2 s senza risposta.
- Le prove con coda Q1–Q32 girano su un solo thread, con IOCP:
  - `CreateIoCompletionPort`, `GetQueuedCompletionStatusEx` e `OVERLAPPED_ENTRY`;
  - `#[repr(C)] IoSlot` con OVERLAPPED come primo campo, così dal puntatore si risale all'indice.
  - In uscita, `CancelIoEx` e `GetOverlappedResult`. Se qualche richiesta non torna, i buffer si
    lasciano (`mem::forget`), perché il kernel potrebbe ancora scriverci.
  - Con `WaitForMultipleObjects` le richieste restavano ferme fino a circa 1 s.
  - Il ciclo a eventi si fermava a circa 100.000 IOPS in 4K a Q32. Con IOCP si arriva a circa 110.000,
    il limite di un solo thread.
- La lettura in sequenza a Q1 è legata alla latenza: un NVMe veloce arriva al massimo solo con più
  richieste in volo. È il motivo della tabella con coda.
- **NTFS**: il file è preallocato con `SetEndOfFile`, quindi le letture oltre la «valid data length»
  restituiscono zeri senza toccare il disco. Non si devono mai leggere zone non scritte.
- I punti dei grafici sono raccolti ogni 100 ms; sopra i 3000 punti vengono dimezzati.
  - Va emesso un punto (`s.emit`) **prima** della verifica di fine fase, altrimenti nel grafico della
    latenza compare una diagonale.
- La scansione della superficie:
  - legge blocchi da 4 MiB, raccolti in tratti (`Tract { start, mbps, lat_avg_ms, lat_max_ms }`);
  - arrotonda **per eccesso** la fine dell'intervallo, così anche un tratto scelto piccolo viene letto;
  - ricorda per ogni volume dove si è fermata, ma solo se la scansione era davvero partita
    (`surface_started`);
  - con «Riscandisci» rilegge solo il tratto scelto e non sposta quel punto.
- Istogramma della latenza: 24 classi per decade (192 in tutto); la vista lineare usa 80 classi e
  taglia allo 0,5 % più lento.

### Lettura a inizio, metà e fine del disco
- Tre fasi, `Phase::Zone(Zone::Start|Middle|End)`, ognuna con il suo `Sampler`: entrano da sole nei
  grafici nel tempo, nell'istogramma e nella riga di comando. Lo stato condiviso ha `zone_secs` (0 = non
  richiesta), `zone_spans` (tratto letto da ogni punto, aggiornato ogni 100 ms), `zone_before` (prove
  precedenti, vedi sotto) e `file_tests`, che serve a nascondere i riquadri vuoti delle prove su file.
- Si apre `\\.\PhysicalDriveN` quando il volume sta su un disco solo (sui dischi meccanici conta la
  posizione sul disco, non nel volume); altrimenti, o se l'apertura fallisce, il volume `\\.\X:`. Le
  due strade richiedono l'amministratore. La lunghezza viene da `IOCTL_DISK_GET_LENGTH_INFO`.
- Ogni punto resta nel suo terzo del disco. L'ampiezza del tratto si prevede dalla velocità **sul tempo
  trascorso** del punto prima (byte letti diviso secondi di orologio), non da quella delle singole
  letture: la prima versione usava questa, sovrastimava (sul file sostitutivo le letture durano 0,1 ms
  e la pausa da 10 ms non conta) e leggeva la «metà» al 33–47 % e la «fine» al 67–80 %.
  - Metà: centrata sul 50 % con l'ampiezza prevista dalla velocità dell'inizio.
  - Fine: parte da `fine − ampiezza prevista`, con la velocità della metà moltiplicata per il calo visto
    fra inizio e metà (fra 0,5 e 1): sui dischi meccanici la fine è più lenta della metà. Senza margine:
    con il 5 % in più la fine si fermava al 99,3 %. Se arriva all'ultimo blocco prima del tempo, legge il
    tratto subito prima di quello già letto (un solo salto indietro) invece di rileggerlo.
  - Se un terzo intero finisce prima del tempo (disco piccolo o velocissimo) si rilegge da capo e la nota
    del risultato lo dice: la cache del disco può gonfiare la velocità.
- Con `MYSTORAGETEST_SURFACE_FILE` si legge il file: è creato con `SetLength`, quindi oltre la valid
  data length Windows restituisce zeri senza toccare il disco (velocità di 10–20 GB/s, che non contano).
  Una pausa di 10 ms per lettura tiene i tratti piccoli.
- Confronto con le prove precedenti: ogni `ZoneSpan` ha `started` (secondi dall'inizio della prova).
  Quando si avvia una prova, `launch` prende dalla prova precedente dello stesso volume
  `Shared::zone_run` (serie spostate perché partano dall'inizio di ogni punto, senza il massimo della
  latenza né l'istogramma) più il suo `zone_before`, e ne tiene `ZONE_KEEP` = 2 nel `zone_before` della
  nuova, come `surface_before` per la superficie. Una prova che non legge i tre punti le passa avanti
  intatte. Solo in memoria.
  - Disegno (`zone_ghosts`): ogni punto precedente comincia dove comincia lo stesso punto della prova; per i
    punti non ancora cominciati, solo mentre la prova va avanti, dove si prevede (inizio del punto prima +
    `zone_secs`). Finita la prova restano solo i punti letti davvero. Sono disegnati prima delle serie
    della prova, la più vecchia sotto, con il colore del punto a 0,5 e 0,28 di opacità e tratteggiate:
    più chiare e basta si confondevano con la linea tenue del massimo della latenza. Della latenza solo
    la media.
  - Fumetti: il campione della prova precedente più vicino allo stesso istante dall'inizio del punto,
    se non è lontano più di mezzo secondo; segno sulla linea verticale anche per quelli.
  - La casella sta fuori dal blocco disattivato durante la prova (`add_enabled_ui(!testing)`): riguarda
    solo i grafici. È `Settings::zone_compare`, predefinita vera.
- `Drop for TestRun` aspetta il thread al massimo 3 s.

## Elenco dei dischi (`disks.rs`)

### Senza privilegi
- Un handle con accesso 0 su `\\.\PhysicalDriveN` basta per gli IOCTL `FILE_ANY_ACCESS`:

  | IOCTL | Codice |
  |---|---|
  | `IOCTL_STORAGE_QUERY_PROPERTY` | 0x2D1400 |
  | `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX` | 0x700A0 |
  | `IOCTL_DISK_GET_LENGTH_INFO` | 0x7405C |
  | `IOCTL_STORAGE_PREDICT_FAILURE` | 0x2D1100 |
  | `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` | 0x560000 |

- Il pass-through (`IOCTL_ATA_PASS_THROUGH` 0x4D02C, `IOCTL_SCSI_PASS_THROUGH` 0x4D004) vuole un handle
  in lettura e scrittura, quindi l'amministratore.
  - Eccezione: i supporti rimovibili USB accettano i comandi attraverso l'handle del volume `\\.\X:`
    anche senza privilegi. È servito a provare il SCSI pass-through sulle chiavette.
- Tipi di bus: ATA 3, USB 7, RAID 8, SATA 11, NVMe 17, virtuali 14/15/16.

### Attribuzioni
- Un volume su una sola estensione viene attribuito a un disco solo se modello, numero di serie e bus
  coincidono. Senza questo controllo il volume virtuale di Google Drive finiva sul disco 0.
- I bridge JMicron restituiscono numeri di serie sporchi, che vanno puliti (`clean_serial`).
- I numeri dei dischi cambiano dopo un riavvio o un ricollegamento: gli eventi precedenti all'ultimo
  avvio si attribuiscono solo con la lettera del volume.
- Identità di un disco: `instance_id|model|serial|size|usb_id`. Se cambia sotto lo stesso numero (un
  contenitore sostituito sulla stessa lettera), lo stato legato al numero si cancella e il disco
  nuovo si legge subito.

### Dischi che non rispondono
- Ogni domanda a un dispositivo gira in un thread suo (stack da 256 KB), con al massimo una domanda in
  sospeso per dispositivo (`PENDING`).
- Le domande hanno una scadenza comune di 2,5 s.
- Se un dispositivo non risponde in tempo si usa l'ultima risposta buona, che diventa «non risponde»
  dopo 8 s.
- Dischi (0..63), numeri e volumi si chiedono **tutti subito** e si raccolgono dopo. Chiedendo i numeri
  dopo la scadenza, un solo disco bloccato all'avvio mandava tutti i dischi sotto «Collegamento non
  trovato».
- **Windows non termina un processo con una richiesta in sospeso verso un dispositivo bloccato.**
  `--list` risponde in 2,5 s, ma il processo esce circa 43 s dopo, o quando il dispositivo viene
  scollegato. Anche «Termina attività» non lo chiude. È un limite di Windows.

### Dischi virtuali e volumi senza lettera
- Il file di un disco virtuale si legge con `GetStorageDependencyInformation` (feature
  `Win32_Storage_Vhd`):
  - handle del disco con accesso 0, flag HOST_VOLUMES|DISK_HANDLE (3), versione 2;
  - le voci sono da `32 + 4*dimensione_puntatore` byte, dopo un'intestazione di 8;
  - AncestorLevel sta a +28, i puntatori alle stringhe da +32;
  - il percorso è HostVolumeName più DependentVolumeRelativePath, con il volume tradotto in lettera da
    `GetVolumePathNamesForVolumeNameW`.
- Il disco base di Sandbox di Windows (Msft Virtual Disk da 8 GB, senza lettera) è
  `\ProgramData\Microsoft\Windows\Containers\BaseImages\…\BaseLayer.vhdx`. Windows lo crea e lo
  collega da sé, e non va toccato.
- Volumi senza lettera:
  - si trovano con `FindFirstVolumeW`/`FindNextVolumeW`, saltando quelli che hanno una lettera;
  - le estensioni si confrontano con gli offset delle partizioni;
  - etichetta e file system vengono da `GetVolumeInformationW`.

## SMART e autodiagnosi (`smart.rs`)

### NVMe
- Senza amministratore, con `IOCTL_STORAGE_QUERY_PROPERTY` e la proprietà specifica del protocollo:
  - registro della salute;
  - Identify Controller (CNS 1, 4096 byte): OACS bit 4 = autodiagnosi, EDSTT a 316, VER ai byte 80..83;
  - registro delle autodiagnosi 06h (564 byte).
- Per avviare un'autodiagnosi serve l'amministratore: `IOCTL_STORAGE_PROTOCOL_COMMAND`, opcode 14h,
  NSID 0xFFFFFFFF, CDW10 = 1 breve, 2 estesa, 0xF interruzione. Richiede Windows 10 1903 o successivo.
- Registro delle autodiagnosi:
  - byte 0 & 0x0F = prova in corso (1 breve, 2 estesa);
  - byte 1 & 0x7F = percentuale;
  - una voce con il nibble di stato 0xF è vuota.
- Stato NVMe nella risposta: `dw3 >> 17`, con `sct = (s >> 8) & 7` e `sc = s & 0xFF`.

### ATA e SATA
- Senza amministratore:
  - i valori degli attributi arrivano da `IOCTL_STORAGE_PREDICT_FAILURE`, ma senza soglie;
  - IDENTIFY viene dalla proprietà specifica del protocollo (ATA=2, Identify=1, 512 byte), che Windows
    tiene in cache.
- Parole di IDENTIFY usate:

  | Parola | Contenuto |
  |---|---|
  | 76 | velocità SATA supportate |
  | 77 bit 3:1 | velocità negoziata |
  | 83 bit 15:14 = 01 | parole valide |
  | 217 | giri al minuto |
  | 222 | revisione SATA (bit più alto di 0x0FFF) |

- Con l'amministratore, comandi ATA:
  - D0 valori, D1 soglie;
  - D5 lettura di un registro: 06h autodiagnosi, E0h temperatura SCT al byte 200;
  - D4 autodiagnosi: 1 breve, 2 estesa, 0x7F interruzione;
  - D8 attivazione dello SMART;
  - E5 CHECK POWER MODE: count 00h/01h/40h = fermo, 80h = a riposo, FFh = in funzione.
- Dati plausibili solo con almeno 4 attributi o con la somma di controllo a zero. Temperature
  accettate solo fra 1 e 125 °C.
- Significato degli attributi (dalla tabella `drivedb.h` di smartmontools):
  - 202 e 206 dipendono dal produttore.
    - Valori predefiniti dei dischi a piatti: Data Address Mark e Flying Height.
    - Crucial/Micron: 202 = vita consumata (grezzo = % usata), 206 = tasso di errori di scrittura.
    - Samsung e SK hynix: 202 = Exception_Mode_Status. Intel: End_of_Life. Lexar e Indilinx: 206 =
      Min_Erase_Count.
    - Quindi solo Crucial/Micron, riconosciuti dal modello, vengono reinterpretati; per gli altri SSD
      il nome è «Attributo del produttore».
  - 187 a 65.535 è un contatore saturo: è un dato vero del disco, non un errore di lettura.
  - Sul Crucial MX500, il 197 che passa da 0 a 1 e torna indietro è un difetto noto del firmware.
- Il sito di smartmontools blocca gli agenti con una protezione anti-bot: serve il testo dei risultati
  di ricerca, oppure scaricare `drivedb.h` dal repository.

### Pass-through USB
I formati sono presi come fatti da smartmontools (`scsiata.cpp` e `scsinvme.cpp`). Il codice è scritto
da capo, e il README cita smartmontools.

- **SAT**: «ATA PASS-THROUGH».
  - CDB da 12 byte: `A1 (proto<<1) flags feat count lbaL lbaM lbaH dev cmd 0 0`.
  - CDB da 16 byte: `85 (proto<<1) flags 0 feat 0 count 0 lbaL 0 lbaM 0 lbaH dev cmd 0`.
  - Protocollo: 3 = senza dati, 4 = PIO in lettura.
  - `flags = (ck_cond<<5)|(t_dir<<3)|(byte_block<<2)|t_length`: 0x0E lettura, 0x0C senza dati, 0x2C
    senza dati con i registri di ritorno.
  - I registri tornano nel descrittore di sense 0x09 (count a +5, stato a +13) oppure in formato fisso
    con 00/1D (count [6], stato [4]).
- Vecchi bridge USB-SATA:
  - JMicron `DF 10 00 lenH lenL feat count lbaL lbaM lbaH (A0|B0) cmd`; la porta si trova con
    `DF 10 00 00 01 00 72 0F 00 00 00 FD` (0x04 → porta 0, 0x40 → porta 1);
  - Cypress `24 24 …`;
  - Sunplus `F8 00 22 …`;
  - Prolific `D8 15 00 …`.
- Bridge USB-NVMe:
  - **JMicron**: `A1 80|proto`, in tre passi.
    - proto 0 = invio del comando: 512 byte con la firma «NVME» 0x454D564E, opcode al byte 8, NSID a
      12, CDW10 a 48.
    - Poi 1 = senza dati oppure 2 = lettura.
    - Infine F = risposta, con lo stato in `word5 >> 17`.
    - Il registro delle autodiagnosi passa solo fino a 536 byte.
  - **Realtek**: `E4 lenLo lenHi opcode cdw10low`, registri fino a 512 byte.
  - **ASMedia**: `E6 opcode 0 cdw10low 0 0 cdw10[31:24] cdw10[23:16]`.
- SCSI LOG SENSE `4D 00 (40|pagina) …`: pagina 0x0D temperatura, pagina 0x2F eccezioni informative.

### Regole di sicurezza della sonda USB
- **Mai mandare alla cieca il comando JMicron NVMe** (A1 con byte 1 = 0x80): su un bridge SAT è un hard
  reset ATA.
- I comandi Realtek e ASMedia alla cieca solo se il disco si dichiara SSD, con un controllo stretto
  della risposta (i byte 7..32 del registro della salute devono essere zero).
- Ci si ferma appena il disco ha risposto a un IDENTIFY valido.
- IDENTIFY e attivazione dello SMART si mandano solo dopo un ABORTED COMMAND (chiave 0x0B) o dati non
  plausibili, **mai dopo un errore hardware**.
- Una UNIT ATTENTION (chiave 6) si ritenta una volta.
- Un dispositivo è «incastrato» con gli errori Win32 21, 55, 121, 433, 1167 o con risposte da 5 s in
  su: da lì non gli si manda altro.
- Chiavette e lettori di schede: nessuno SMART, e niente altri tentativi.
- Il metodo che funziona si ricorda per ogni disco (modello, serie, VID:PID, dimensione); dopo 3
  fallimenti di fila si dimentica.
- Le letture periodiche non svegliano un disco fermo:
  - `GetDevicePowerState` funziona con accesso 0;
  - CHECK POWER MODE attraverso SAT solo se il bridge restituisce i registri.
- Tempi massimi: SCSI 8 s, ATA 30 s.
- In un processo nuovo la strada di un bridge non è ancora nota. `self_test` legge prima lo SMART,
  altrimenti ricade su SAT e viene rifiutato in 0 ms.
- «Rapporto di diagnosi» (`--diagnosi`) rifà tutte le sonde con la traccia dei comandi e delle
  risposte. È il modo per far verificare all'utente i percorsi da amministratore.

## Bridge e contenitori USB osservati

### JMicron JMS581 (152D:0581)
- **Autodiagnosi**: accetta l'avvio, ma il passo «esecuzione» non finisce, né in 8 né in 60 s.
  - Poi arrivano UASPStor 129 (reset), disk 51 ed Ntfs 140, il disco sparisce e il ricollegamento
    fallisce con il codice 43.
  - È successo sette volte, a 10 e a 5 Gbit/s, quindi non dipende dalla velocità.
  - Ora la prova è bloccata su 0581/1581. Gli altri bridge JMicron NVMe (JMS583, JMS586…) hanno solo
    un'avvertenza.
- Ha avuto a tratti errori hardware 4/44/81 su letture, scritture e flush, con il collegamento USB
  sempre su. La causa era il bridge, non l'SSD.
- Senza un disco riconosciuto risponde a ogni comando con 02h, chiave 4, 44/81, e mostra 0 B.
- Firmware:
  - la 04.01.00.04 si presenta come 04.02.00.04 (REV 4204) per un flag nella NVRAM;
  - esiste una 04.02.00.06, solo per la versione DL;
  - il programma di aggiornamento non salva la NVRAM, e un bridge rovinato si recupera solo con un
    programmatore SPI.
  - Consiglio dato: non aggiornarlo.
- Con un SSD dentro ha passato 15 minuti di prove miste senza errori, a circa 1 GB/s in lettura.

### Realtek RTL9210B (0BDA:9210)
- Con il firmware 1.23.5 (che a Windows dichiara 1.00) si bloccava circa 2 minuti dopo ogni
  collegamento, con due SSD diversi. Si vedevano centinaia di letture di LBA 0 ritentate, reset
  UASPStor, Ntfs 140 e il codice 43.
- L'aggiornamento alla **1.35.6** ha risolto: poi ha letto 371 GB in 11 minuti senza errori, a circa
  706 MB/s.
- L'autodiagnosi dentro E4h viene rifiutata come comando non valido (ILLEGAL REQUEST, 20/00) sia senza
  dati sia come lettura, anche con il firmware 1.35.6. Le letture (SMART, Identify, registro delle
  prove) rispondono in 2–3 ms.
- Aggiornamento del firmware con UTHSB_MPtool_Lite:
  - L'ini indica `CfgSel=configure/RTL9210B_CG.cfg` e `FwFlashBinSel=gdfw/RTL9210B_v1.35.6.040126.bin`.
  - Il programma accetta solo il cfg con il nome scritto nell'ini, altrimenti dice «No configure
    file». Il nostro cfg va scritto con quel nome e l'originale va conservato a parte.
  - Il programma riscrive il cfg incrementando SERIAL.
  - Il dump è in formato `CHIAVE : valore` e il cfg in `CHIAVE = valore`.
  - Il primo aggiornamento può finire in FAIL: il numero di serie cambia, il bridge si ripresenta a
    Windows e il programma va in timeout. Rifarlo riesce.
- Fonti del firmware, tutte di terzi:
  - `github.com/bensuperpc/rtl9210` ha RTL9210B 1.35.6.040126 (la più recente), 1.34.39 e 1.34.29
    (corregge le disconnessioni casuali);
  - lo stesso repository sconsiglia la 1.32.87 e raccomanda almeno la 1.33.44;
  - per l'RTL9210C(N) c'è la 9.34.74.091125: **mai il firmware B su un C**;
  - station-drivers rispondeva HTTP 500 e sono servite le copie della Wayback Machine;
  - lo zip scaricato si verifica con l'impronta del blob git del repository.

### Altri dispositivi
- **Realtek RTL9210C** con l'identificativo di un altro produttore (3535:9210, firmware 1.01):
  - SAT e lettura ASMedia rifiutate subito;
  - lettura Realtek in 1–9 ms;
  - autodiagnosi rifiutata come sull'RTL9210B.
- **Dischi Seagate USB** (0BC2:3322 e 0BC2:61B7, UASP): SAT a 12 byte con soglie, registro delle
  autodiagnosi e stato di rotazione (FFh).
- **Chiavette USB**: SAT rifiutato (chiave 5, 24/00 o 20/00), nessuno SMART.
- Un contenitore che funziona a 5 Gbit/s e non a 10 di solito ha un problema di bridge, cavo o porta.
  La prova si fa cambiando una cosa alla volta, prima senza myStorageTest aperto.

## Eventi di sistema (`eventlog.rs`)

- `EvtQuery` regge circa 23 espressioni per XPath. Per questo la query è una `<QueryList>` con più
  `<Select>` da 8 provider ciascuno, con `&lt;` e `&gt;` per i confronti.
- `EvtFormatMessage` era lento: 5,3 s su 6,4 per 20.000 eventi. Ora si ricava un modello per tipo di
  evento:
  - `EvtFormatMessage` viene chiamato con valori sostitutivi segnati da U+E000 N U+E001;
  - **il `Count` di un EVT_VARIANT stringa deve essere la lunghezza in caratteri**, altrimenti gli
    inserti tornano vuoti;
  - un modello è affidabile dopo 3 confronti riusciti;
  - i tipi con campi tipizzati (Ntfs 140) usano una cache dei testi esatti.
  - Risultato: 20.000 eventi in 0,9 s invece di 6,2. `--check-events` confronta i testi con quelli di
    Windows, e sono risultati 78.134 uguali.
- Il monitoraggio usa `EvtSubscribe`. Il polling con `query_after` rileggeva tutto il registro e teneva
  il processore all'11 % a riposo; ora è a zero.
- **Eventi disk 153/154**: il campo Binary è un `IO_ERROR_LOG_PACKET`.
  - DumpDataSize sta a +2, ErrorCode a +12 (i 16 bit bassi sono il numero dell'evento) e DumpData a +40.
  - In DumpData:
    - byte 1 = stato SCSI;
    - byte 2 = stato SRB (& 0x3F);
    - byte 3 = opcode del CDB;
    - poi il sense: formato fisso 0x70/71 con SK = s[2] & 0xF, ASC = s[12], ASCQ = s[13]; formato a
      descrittori 0x72/73 con s[1..3].
  - Il testo decodificato va **prima** della spiegazione, altrimenti nella colonna viene tagliato.

## Info e collegamenti (`info.rs`)

- **PCIe**: DEVPKEY `{3ab22e31-8264-4b4e-9af5-a8d2d8e33e62}`.
  - pid 3/4 = classe e sottoclasse;
  - 9/10 = velocità e larghezza attuali;
  - 11/12 = velocità e larghezza massime (velocità 1..6 = generazione).
  - Ci sono solo sul nodo del dispositivo finale, non su bridge e porte radice. Quanto potrebbe dare la
    porta non si sa, quindi la causa di un collegamento più lento è dedotta.
- MB/s per linea PCIe dopo la codifica, dalla generazione 1 alla 6: 250, 500, 984,6, 1969,2, 3938,5,
  7563.
- **USB**, dagli IOCTL degli hub (header dell'SDK `usbioctl.h`):

  | IOCTL | Codice | Cosa dà |
  |---|---|---|
  | GET_NODE_CONNECTION_INFORMATION_EX | 0x220448 | bcdUSB al byte 6; Speed al byte 23 (0 Low, 1 Full, 2 High, 3 Super) |
  | GET_NODE_CONNECTION_INFORMATION_EX_V2 | 0x22045C | protocolli della porta a +8 (bit 2 = USB 3); flag a +12 (bit 0 SS in uso, bit 1 SS possibile, bit 2 SS+ in uso, bit 3 SS+ possibile) |
  | GET_PORT_CONNECTOR_PROPERTIES | 0x220458 | bit 3 a +8 = USB-C |

  - L'hub si trova con `GUID_DEVINTERFACE_USB_HUB {F18A0E88-C30C-11D0-8815-00A0C906BED8}`; il numero
    della porta è `CM_DRP_ADDRESS`.
  - Negli hub USB 3, la metà USB 2 dell'hub non va segnalata come lenta.
- Partizioni: `IOCTL_DISK_GET_DRIVE_LAYOUT_EX` (0x70050).
  - L'intestazione è di 48 byte e le voci di 144.
  - MBR: tipo a 32, avvio a 33.
  - GPT: GUID del tipo 32..48, nome UTF-16 72..144.
- Altre proprietà: StorageAccessAlignmentProperty (6), StorageDeviceTrimProperty (8),
  StorageDeviceWriteCacheProperty (4).
- **Espulsione**:
  - del dispositivo: `CM_Request_Device_EjectW`, 3 tentativi, con i motivi del rifiuto;
  - del solo supporto: FSCTL_LOCK_VOLUME, FSCTL_DISMOUNT_VOLUME, IOCTL_STORAGE_MEDIA_REMOVAL e
    IOCTL_STORAGE_EJECT_MEDIA.
  - Non è mai stata eseguita davvero nelle prove.

## Grafo Storage (`topology.rs`)

### Albero dei dispositivi
- CfgMgr32: `CM_Locate_DevNodeW`, `CM_Get_Parent/Child/Sibling` e `CM_Get_Device_IDW`.
- Proprietà del registro `CM_DRP_*`:

  | Proprietà | Valore |
  |---|---|
  | DEVICEDESC | 0x01 |
  | COMPATIBLEIDS | 0x03 |
  | SERVICE | 0x05 |
  | CLASS | 0x08 |
  | FRIENDLYNAME | 0x0D |
  | LOCATION_INFORMATION | 0x0E |
  | CAPABILITIES | 0x10 (rimovibile = 0x4) |
  | UI_NUMBER | 0x13 |
  | ADDRESS | 0x1D |
  | LOCATION_PATHS | 0x24, multi-sz |

- I dispositivi presenti si elencano con `SetupDiGetClassDevsW(null, "PCI" | "USB",
  DIGCF_PRESENT|DIGCF_ALLCLASSES)`.
- Nome del prodotto riportato dal bus: DEVPKEY `{540b947e-8b40-45bc-a8a2-6a0b894cbda2}`, pid 4.
- I bridge host del processore non hanno la proprietà della classe PCI. La classe si legge da
  `CC_xxyy` negli ID compatibili.
- Nome del processore: `ProcessorNameString` nel registro.
- Chipset AMD X570:
  - è uno switch PCIe 1022:57AD sotto la porta del processore 1022:1483, con le porte a valle
    57A3/57A4;
  - il collegamento verso il processore è PCIe 4.0 x4 (7,9 GB/s);
  - i bridge interni sono 1022:1484, 57A4 e 1635.
- Spessore delle linee: `clamp(1 + 0.75·log2(banda_MB/s / 60), 1, 8)`.
- MB/s di riferimento: USB per velocità 0,1875 / 1,5 / 60 / 500 / 1212; SATA 150 / 300 / 600.

### Volumi senza disco fisico
- I driver di un volume si trovano così:
  1. `EnumDeviceDrivers` e `GetDeviceDriverBaseNameW` danno i driver caricati;
  2. per ogni candidato, `NtQueryVolumeInformationFile(FileFsDriverPathInformation = 9)` su `X:\`
     aperto con accesso 0 e `FILE_FLAG_BACKUP_SEMANTICS`. Il buffer è
     `{ BOOLEAN DriverInPath; ULONG len a +4; WCHAR nome a +8 }` e si prova sia `\Driver\X` sia
     `\FileSystem\X`.
  - Servono le feature `Wdk_Storage_FileSystem` e `Win32_System_ProcessStatus`.
- **VeraCrypt**:
  - `\\.\VeraCrypt` aperto con accesso 0, IOCTL 0x222018 (TC_IOCTL_GET_MOUNTED_VOLUMES);
  - `MOUNT_LIST_STRUCT` è una maschera u32 seguita da 26 `wszVolume` da 520 byte;
  - dal percorso del file contenitore si risale al volume che lo ospita.
- Volumi su più dischi: `IOCTL_VOLUME_LOGICAL_TO_PHYSICAL` (0x560020).
  - Con più di un offset è un mirror.
  - Se il disco cambia entro 16 × 64K è a strisce.
  - Altrimenti è un volume esteso.

### Bridge
- Il chip si ricava da una tabella di VID:PID (JMicron, Realtek, ASMedia) e dalle sigle nel nome del
  prodotto (RTL9…, JMS5…, JM20…, ASM1…, ASM2…).
- Se il bridge sa collegare sia NVMe sia SATA, il tipo di disco dentro si sa solo dopo la lettura
  dello SMART, che senza privilegi non c'è.

### Disegno
- Le colonne vanno per profondità nell'albero.
- I bridge stanno tutti in una colonna, subito prima di quella dei dischi; dischi e volumi sono
  allineati nell'ultima colonna.
- I contenitori (VeraCrypt) vanno a destra del loro volume, in una colonna laterale (colonna
  dell'ospite + 1).
- Le linee sono curve di Bézier.
- Zoom:
  - adattato alla finestra fra 0,45 e 1,0;
  - scelto a mano fra 0,3 e 2,5;
  - la vista sta nei dati temporanei sotto `Id::unique("vista del grafo storage")` e si azzera quando
    si cambia modo (dischi o completo).

### Grafo completo
- Esclusi:
  - i bridge host (06/00);
  - le funzioni fittizie AMD (classe 13h);
  - l'audio HDMI della scheda grafica, unito alla scheda;
  - le risorse di sistema e i processori ACPI.
- Senza la spunta restano solo i controller di dischi (classe 01h) e le porte libere.
- **USB**:
  - si saltano gli hub radice, le interfacce `&MI_` e i dischi già mostrati;
  - gli hub restano anche vuoti;
  - il ruolo di un dispositivo viene dalle classi Windows delle sue interfacce e dei figli HID, fino
    a 3 livelli.
  - Ordine di priorità:
    1. Bluetooth;
    2. display (scheda video USB);
    3. webcam;
    4. immagini;
    5. archiviazione;
    6. un nome con «mouse» più la classe mouse;
    7. un nome con «keyboard»/«tastiera» più la classe tastiera;
    8. tastiera e mouse insieme;
    9. tastiera;
    10. mouse;
    11. audio;
    12. rete, stampante, porte, WPD, lettore di smart card, biometria;
    13. Billboard;
    14. HID;
    15. altrimenti «dispositivo USB».
  - Un mouse che dichiara anche una tastiera resta un mouse, grazie al nome.
- **Monitor**:
  - EDID da `HKLM\SYSTEM\CurrentControlSet\Enum\<id>\Device Parameters\EDID`;
  - produttore = tre lettere da 5 bit nei byte 8–9;
  - nome = descrittore 0xFC;
  - risoluzione nativa = primo timing dettagliato all'offset 54.
- **HD Audio**: i codec sono i figli `HDAUDIO\FUNC_01`.
- **ACPI**:
  - figli `ACPI\` delle classi tastiera, mouse, porte, sicurezza, batteria e HID;
  - PNP0C09 è il controller integrato (EC);
  - sotto il bridge LPC il collegamento si chiama «LPC», altrove «ACPI».
- Tempi: `--graph completo` 0,9 s, `--graph` 0,4 s.

## Monitoraggio, finestra e avvio

- Icona nell'area di notifica:
  - thread dedicato, con una finestra nascosta di classe `myStorageTestTray[-ISTANZA]`;
  - `Shell_NotifyIconW` con NOTIFYICON_VERSION_4;
  - il fumetto NIF_INFO su Windows 10/11 diventa una notifica;
  - l'icona è creata con `CreateIcon` da RGBA.
- Un'istanza sola: la seconda trova quella finestra e le manda un messaggio. Un'istanza senza privilegi
  può svegliarne una elevata grazie a
  `ChangeWindowMessageFilterEx(hwnd, WM_TRAY_SHOW, MSGFLT_ALLOW)` (UIPI).
- **Riavvio come amministratore**:
  - `ShellExecuteW` con il verbo «runas» e `--after <pid>`;
  - il nuovo processo aspetta la fine del vecchio (`OpenProcess(SYNCHRONIZE)` +
    `WaitForSingleObject`) prima del controllo dell'istanza unica. Senza questa attesa il programma non
    si riapriva.
  - «Avvia sempre come amministratore» e `--admin` fanno lo stesso all'avvio; se l'UAC viene rifiutato
    si continua senza privilegi.
- Avvio con Windows: valore `myStorageTest` in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, con
  `"<exe>" --monitor`.
- Monitoraggio:
  - la prima volta lo SMART si legge per intero, poi a campione;
  - le notifiche uguali non si ripetono per 10 minuti;
  - un disco che non risponde dà un avviso critico;
  - in chiusura il thread si aspetta al massimo 2 s.
- Impostazioni e registro stanno in `%LOCALAPPDATA%\myStorageTest\` (`settings.json`, `monitor.log`,
  `diagnosi-smart.txt`) e si salvano da sole quando cambiano, tranne nelle sequenze guidate.

## Riga di comando e code page (`conout.rs`)

- PowerShell legge l'output degli exe con la code page della console, 850 in italiano.
- Quando lo stdout è una pipe o un file, il programma lo passa da una pipe anonima a un thread che
  converte da UTF-8 a `GetConsoleOutputCP()` con `WideCharToMultiByte(WC_NO_BEST_FIT_CHARS)`.
- I caratteri che la code page non ha diventano ASCII:

  | Carattere | Diventa |
  |---|---|
  | `—` `–` `−` | `-` |
  | `…` | `...` |
  | `→` | `->` |
  | `←` | `<-` |
  | `•` `●` | `*` |
  | `⚠` | `!` |

- In una console UTF-8 non cambia nulla.

## Durata delle prove sequenziali (in sospeso)

L'analisi già fatta, da proporre quando l'utente lo chiede:
- Oggi `seq_write` riempie tutto il file (dimensione arrotondata a 1 MiB) e `seq_read` lo rilegge e lo
  verifica tutto.
  - `map` tiene la generazione di ogni blocco da 4K.
  - Lettura e scrittura casuali e flush usano `map.len()`; le prove con coda usano `size`.
- Proposta:
  - `seq_secs` nelle impostazioni (predefinito 5) e in `TestConfig`;
  - scrivere e leggere in sequenza per N secondi, ricominciando dall'inizio quando il file finisce,
    come fanno i programmi di benchmark;
  - se il tempo finisce prima di aver riempito il file, ridurre dimensione e `map` alla parte scritta,
    per via della valid data length di NTFS;
  - eventualmente una scelta «tutto il file», che conserva la verifica completa;
  - un'opzione anche nella riga di comando.

## Analisi di un crash di Windows

- I minidump in `C:\Windows\Minidump` si leggono solo da amministratore: l'utente li copia in una
  cartella accessibile.
- kd.exe viene dal pacchetto WinDbg, che sta in WindowsApps. Da lì l'avvio dà «Accesso negato»: si
  copia la cartella `amd64` altrove con robocopy, escludendo `ttd`.
- Il comando:

  ```
  kd.exe -z <dump> -y "srv*<cartella simboli>*https://msdl.microsoft.com/download/symbols" -logo <log> -c "!analyze -v; q"
  ```

  Con `-logo` e non `*>`.
- Gli eventi utili sono Kernel-Power 41, WER-SystemErrorReporting 1001, disk 153/154, Ntfs 140 e
  nvlddmkm 13/153.
- Nel caso visto, la causa era `nvlddmkm.sys` (driver NVIDIA) e non il disco USB sospettato all'inizio.

## PC di sviluppo

Serve a leggere le verifiche del README.
- Ryzen 9 3900X su ASUS PRIME X570-PRO (BIOS 4021, AGESA 1.2.0.3 Patch C), con una RTX 4080 a PCIe 4.0
  x8.
- Porte PCIe del processore:
  - 00:01.1 (GPP0): NVMe di sistema;
  - 00:01.2 (BXBR): chipset;
  - 00:03.1 (GPP8): scheda grafica;
  - 00:03.2 (GPP9): un altro NVMe;
  - 00:08.1: USB e audio interni.
- Porte del chipset (BYD1…BYD9): tre NVMe, di cui uno PCIe 4.0 x4 collegato a x1; la rete Intel I211;
  due controller USB; due controller SATA, uno senza dischi.
- Dischi e volumi:
  - due SATA, tra cui un Crucial MX500 con migliaia di errori CRC di interfaccia storici, segno di un
    cavo;
  - un volume a strisce su due NVMe;
  - contenitori VeraCrypt;
  - un volume di Google Drive;
  - dischi Seagate USB dietro hub Genesys Logic;
  - contenitori JMicron JMS581 e Realtek RTL9210B;
  - chiavette.

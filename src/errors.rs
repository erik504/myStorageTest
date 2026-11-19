//! Classificazione dei codici d'errore Win32 che un disco o un file system possono restituire.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrClass {
    /// Superficie o celle danneggiate: CRC, settore non trovato, errori di lettura o scrittura.
    Media,
    /// Il dispositivo ha risposto con un errore hardware generico.
    Device,
    /// Collegamento instabile: reset del bus, cavo, porta o bridge USB.
    Link,
    /// Il dispositivo non ha risposto in tempo.
    Timeout,
    /// Il disco è sparito durante l'operazione.
    Removed,
    /// Strutture del file system danneggiate.
    FileSystem,
    /// I dati riletti non corrispondono a quelli scritti.
    Integrity,
    /// Spazio esaurito o quota superata.
    Space,
    /// Protezione da scrittura.
    WriteProtect,
    /// Accesso negato o file in uso: non è un difetto del disco.
    Access,
    /// Operazione annullata dall'utente.
    Cancelled,
    Other,
}

impl ErrClass {
    pub fn label(self) -> &'static str {
        match self {
            ErrClass::Media => "Supporto danneggiato",
            ErrClass::Device => "Errore hardware",
            ErrClass::Link => "Collegamento",
            ErrClass::Timeout => "Timeout",
            ErrClass::Removed => "Disco scollegato",
            ErrClass::FileSystem => "File system",
            ErrClass::Integrity => "Dati corrotti",
            ErrClass::Space => "Spazio esaurito",
            ErrClass::WriteProtect => "Sola lettura",
            ErrClass::Access => "Accesso negato",
            ErrClass::Cancelled => "Annullato",
            ErrClass::Other => "Altro",
        }
    }

    /// Vero per gli errori che indicano un problema reale del disco, del collegamento o dei dati.
    pub fn is_fault(self) -> bool {
        matches!(
            self,
            ErrClass::Media
                | ErrClass::Device
                | ErrClass::Link
                | ErrClass::Timeout
                | ErrClass::Removed
                | ErrClass::FileSystem
                | ErrClass::Integrity
                | ErrClass::WriteProtect
        )
    }

    /// Vero quando non ha senso proseguire con le prove sul disco.
    pub fn is_fatal(self) -> bool {
        matches!(
            self,
            ErrClass::Removed | ErrClass::Space | ErrClass::WriteProtect | ErrClass::Cancelled
        )
    }
}

/// Classe e spiegazione in italiano per un codice d'errore Win32.
pub fn classify(code: u32) -> (ErrClass, &'static str) {
    use ErrClass::*;
    match code {
        23 => (
            Media,
            "Errore CRC: il disco non riesce a leggere correttamente questi dati (settore danneggiato o cavo difettoso)",
        ),
        25 => (
            Media,
            "Errore di posizionamento: il disco non trova la traccia richiesta",
        ),
        26 => (Media, "Il disco non è leggibile con questo formato"),
        27 => (Media, "Settore non trovato"),
        29 => (Media, "Errore di scrittura sul dispositivo"),
        30 => (Media, "Errore di lettura dal dispositivo"),
        1392 => (FileSystem, "File o directory danneggiati e illeggibili"),
        1393 => (FileSystem, "Struttura del disco danneggiata e illeggibile"),
        1005 => (FileSystem, "File system non riconosciuto sul volume"),
        1106 => (FileSystem, "Tabella delle partizioni non valida"),
        6000..=6010 => (FileSystem, "Errore di cifratura del file (EFS)"),
        323 => (
            Integrity,
            "Somma di controllo dei dati non valida (integrità del file system)",
        ),
        31 => (Device, "Un dispositivo collegato al sistema non funziona"),
        1117 => (
            Device,
            "Errore di I/O del dispositivo: la richiesta non è stata eseguita",
        ),
        1127 => (Device, "Operazione del disco fallita anche dopo i tentativi ripetuti"),
        1128 => (Device, "Reset del controller del disco fallito"),
        1126 => (Device, "Ricalibrazione del disco fallita"),
        1165 => (Device, "Il dispositivo richiede pulizia o manutenzione"),
        483 => (Device, "Errore hardware irreversibile del dispositivo"),
        1784 => (Device, "Il buffer fornito dal dispositivo non è valido"),
        21 => (Removed, "Il dispositivo non è pronto"),
        55 => (Removed, "Il dispositivo non è più disponibile"),
        433 => (Removed, "Il dispositivo non esiste più"),
        1167 => (Removed, "Il dispositivo non è collegato"),
        15 => (Removed, "Unità non trovata"),
        1006 => (
            Removed,
            "Il volume è stato modificato dall'esterno: il file aperto non è più valido",
        ),
        1110 => (Link, "Il supporto nell'unità potrebbe essere cambiato"),
        1111 => (Link, "Reset del bus di I/O"),
        1112 => (Removed, "Nessun supporto nell'unità"),
        1118 => (Link, "Nessun dispositivo seriale inizializzato"),
        1119 | 1120 => (Link, "Risorsa hardware condivisa non disponibile"),
        170 => (Link, "Risorsa richiesta in uso dal dispositivo"),
        121 => (Timeout, "Timeout: il dispositivo non ha risposto in tempo"),
        1460 => (Timeout, "Operazione scaduta per timeout"),
        258 => (Timeout, "Attesa scaduta"),
        995 => (Cancelled, "Operazione di I/O annullata"),
        1223 => (Cancelled, "Operazione annullata dall'utente"),
        112 => (Space, "Spazio su disco insufficiente"),
        39 => (Space, "Disco pieno"),
        1295 => (Space, "Quota disco superata"),
        19 => (WriteProtect, "Il supporto è protetto da scrittura"),
        5 => (Access, "Accesso negato"),
        32 => (Access, "File in uso da un altro programma"),
        33 => (Access, "Parte del file bloccata da un altro programma"),
        1314 => (Access, "Privilegio richiesto non posseduto"),
        2 => (Other, "File non trovato"),
        3 => (Other, "Percorso non trovato"),
        38 => (Other, "Fine del file raggiunta"),
        87 => (Other, "Parametro non valido"),
        1 => (Other, "Funzione non supportata dal dispositivo"),
        50 => (Other, "Richiesta non supportata dal dispositivo"),
        _ => (Other, "Errore non classificato"),
    }
}

//! Elenco dei volumi e dei dischi fisici, senza bisogno di privilegi di amministratore.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_IDW, CM_Get_Parent, SP_DEVICE_INTERFACE_DATA, SP_DEVINFO_DATA,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::core::GUID;

use crate::win::{self, ascii_at, rd_u32, rd_u64};

pub const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
pub const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
pub const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
pub const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;

pub const BUS_ATA: u32 = 3;
pub const BUS_USB: u32 = 7;
pub const BUS_SATA: u32 = 11;
pub const BUS_NVME: u32 = 17;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_RAMDISK: u32 = 6;

#[derive(Clone, Debug, Default)]
pub struct PhysDisk {
    pub number: u32,
    pub vendor: String,
    pub model: String,
    pub revision: String,
    pub serial: String,
    pub bus: u32,
    pub removable: bool,
    pub size: u64,
    /// `Some(true)` = SSD, `Some(false)` = disco meccanico, `None` = non dichiarato.
    pub ssd: Option<bool>,
    /// VID e PID del bridge USB, se il disco è collegato via USB.
    pub usb_id: Option<(u16, u16)>,
    pub letters: Vec<char>,
    /// Posto del disco nell'albero dei dispositivi di Windows (ID dell'istanza).
    pub instance_id: String,
    /// Il dispositivo USB a cui appartiene il disco, nello stesso albero; vuoto se non è USB.
    pub usb_node: String,
    /// Lo stesso dispositivo, se Windows lo dichiara scollegabile: è quello a cui chiedere la
    /// rimozione sicura.
    pub eject_node: String,
    /// Il disco non risponde: i dati sono quelli dell'ultima lettura riuscita, e finché resta
    /// così non gli si manda altro.
    pub stuck: bool,
}

impl PhysDisk {
    pub fn device_path(&self) -> String {
        format!(r"\\.\PhysicalDrive{}", self.number)
    }

    /// Collegato via USB: lo dice il bus, oppure il bridge trovato risalendo l'albero dei
    /// dispositivi (certi driver dei produttori presentano il disco come SCSI).
    pub fn is_usb(&self) -> bool {
        self.bus == BUS_USB || self.usb_id.is_some()
    }

    pub fn title(&self) -> String {
        let name = format!("{} {}", self.vendor, self.model);
        let name = name.trim();
        if name.is_empty() {
            format!("Disco {}", self.number)
        } else {
            name.to_string()
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self.ssd {
            Some(true) => "SSD",
            Some(false) => "HDD",
            None => "",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Volume {
    pub letter: char,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    /// Supporto rimovibile (chiavetta, scheda di memoria) secondo Windows.
    pub removable: bool,
    /// Numeri dei dischi fisici su cui si estende il volume.
    pub disks: Vec<u32>,
    /// Il volume non risponde (vedi `PhysDisk::stuck`).
    pub stuck: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub volumes: Vec<Volume>,
    pub disks: Vec<PhysDisk>,
}

impl Inventory {
    pub fn disk(&self, number: u32) -> Option<&PhysDisk> {
        self.disks.iter().find(|d| d.number == number)
    }

    pub fn volume(&self, letter: char) -> Option<&Volume> {
        self.volumes.iter().find(|v| v.letter == letter)
    }
}

pub fn bus_name(bus: u32) -> &'static str {
    match bus {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "FireWire",
        6 => "Fibre Channel",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        14 => "Virtuale",
        15 => "Disco virtuale",
        16 => "Spazi di archiviazione",
        17 => "NVMe",
        18 => "SCM",
        19 => "UFS",
        _ => "Sconosciuto",
    }
}

/// Legge un descrittore con IOCTL_STORAGE_QUERY_PROPERTY (query standard).
pub fn query_property(h: &win::Handle, property_id: u32, out: &mut [u8]) -> Result<usize, u32> {
    let mut input = [0u8; 12];
    input[0..4].copy_from_slice(&property_id.to_le_bytes());
    win::ioctl(h, IOCTL_STORAGE_QUERY_PROPERTY, &input, out)
}

/// Alcuni bridge USB restituiscono byte a caso al posto del numero di serie: in quel caso lo si scarta.
fn clean_serial(s: &str) -> String {
    let t = s.trim();
    if t.len() < 4
        || !t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
    {
        String::new()
    } else {
        t.to_string()
    }
}

/// Modello, numero di serie e bus dichiarati dal dispositivo che sta sotto un volume.
fn volume_descriptor(letter: char) -> Option<(String, String, u32)> {
    let h = win::open_device_query(&format!(r"\\.\{letter}:")).ok()?;
    let mut buf = vec![0u8; 2048];
    let n = query_property(&h, 0, &mut buf).ok()?;
    let b = &buf[..n.max(40).min(buf.len())];
    Some((
        ascii_at(b, rd_u32(b, 16) as usize),
        clean_serial(&ascii_at(b, rd_u32(b, 24) as usize)),
        rd_u32(b, 28),
    ))
}

/// Solo per le verifiche automatiche: `MYSTORAGETEST_FAKE_STUCK=8,L@6` fa restare senza risposta le
/// domande al disco 8 e al volume L: a partire da 6 secondi dopo la prima lettura dell'elenco (senza
/// «@», da subito), come con un contenitore USB bloccato.
fn fake_stuck(what: &str) -> bool {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    let start = *START;
    let Ok(spec) = std::env::var("MYSTORAGETEST_FAKE_STUCK") else { return false };
    let (targets, after) = spec.split_once('@').unwrap_or((spec.as_str(), "0"));
    let after = Duration::from_secs(after.trim().parse().unwrap_or(0));
    start.elapsed() >= after && targets.split(',').any(|t| t.trim().eq_ignore_ascii_case(what))
}

fn probe_disk(number: u32) -> Option<PhysDisk> {
    if fake_stuck(&number.to_string()) {
        std::thread::sleep(Duration::from_secs(3600));
    }
    let h = win::open_device_query(&format!(r"\\.\PhysicalDrive{number}")).ok()?;
    let mut d = PhysDisk {
        number,
        ..Default::default()
    };

    let mut buf = vec![0u8; 2048];
    if let Ok(n) = query_property(&h, 0, &mut buf) {
        let b = &buf[..n.max(40).min(buf.len())];
        d.removable = b[10] != 0;
        d.vendor = ascii_at(b, rd_u32(b, 12) as usize);
        d.model = ascii_at(b, rd_u32(b, 16) as usize);
        d.revision = ascii_at(b, rd_u32(b, 20) as usize);
        d.serial = clean_serial(&ascii_at(b, rd_u32(b, 24) as usize));
        d.bus = rd_u32(b, 28);
    }

    let mut geo = [0u8; 256];
    if win::ioctl(&h, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut geo).is_ok() {
        d.size = rd_u64(&geo, 24);
    }

    // Penalità di posizionamento: assente sugli SSD.
    let mut seek = [0u8; 16];
    if let Ok(n) = query_property(&h, 7, &mut seek) {
        if n >= 9 {
            d.ssd = Some(seek[8] == 0);
        }
    }
    if d.bus == BUS_NVME {
        d.ssd = Some(true);
    }
    Some(d)
}

fn volume_disks(letter: char) -> Vec<u32> {
    let Ok(h) = win::open_device_query(&format!(r"\\.\{letter}:")) else {
        return Vec::new();
    };
    let mut buf = [0u8; 2048];
    let mut out = Vec::new();
    match win::ioctl(&h, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut buf) {
        Ok(_) => {
            let count = rd_u32(&buf, 0) as usize;
            for i in 0..count.min(80) {
                let n = rd_u32(&buf, 8 + i * 24);
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        // Volumi virtuali (unità cloud, dischi RAM) non hanno un disco fisico dietro.
        Err(_) => {}
    }
    out
}

fn probe_volume(letter: char) -> Option<Volume> {
    if fake_stuck(&letter.to_string()) {
        std::thread::sleep(Duration::from_secs(3600));
    }
    let root = win::wide(&format!("{letter}:\\"));
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE && kind != DRIVE_RAMDISK {
        return None;
    }
    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            null_mut(),
            null_mut(),
            null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    unsafe {
        GetDiskFreeSpaceExW(root.as_ptr(), &mut avail, &mut total, &mut free);
    }
    if total == 0 {
        return None;
    }
    Some(Volume {
        letter,
        label: win::from_wide(&label),
        fs: win::from_wide(&fs),
        total,
        free: avail,
        removable: kind == DRIVE_REMOVABLE,
        disks: volume_disks(letter),
        stuck: false,
    })
}

// ---------------------------------------------------------------------------------------------
// Domande con un tempo massimo
// ---------------------------------------------------------------------------------------------

/// Quanto si aspetta ogni disco e ogni volume, tutti insieme. Di norma rispondono in pochi
/// millesimi; un disco che non risponde (un bridge USB bloccato) terrebbe fermo tutto l'elenco, e
/// con lui la finestra, il grafo e il monitoraggio.
const WAIT: Duration = Duration::from_millis(2500);
/// Dopo quanto tempo senza risposta un disco o un volume già visto si dichiara bloccato.
const STUCK_AFTER: Duration = Duration::from_secs(8);

/// Domande ancora senza risposta, per dispositivo, e da quando. Finché una è in sospeso non se ne
/// manda un'altra allo stesso dispositivo: un disco bloccato tiene ferma una richiesta sola.
static PENDING: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);
/// Ultima risposta di ogni disco, volume e posto nell'albero, da usare finché non rispondono.
static LAST_DISKS: LazyLock<Mutex<HashMap<u32, PhysDisk>>> = LazyLock::new(Default::default);
static LAST_VOLUMES: LazyLock<Mutex<HashMap<char, Volume>>> = LazyLock::new(Default::default);
static LAST_NUMBERS: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);

fn pending() -> std::sync::MutexGuard<'static, HashMap<String, Instant>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Fa la domanda `work` in un thread suo; `None` se al dispositivo `key` ne è già stata fatta una
/// che non ha ancora avuto risposta.
fn ask<T: Send + 'static>(key: String, work: impl FnOnce() -> T + Send + 'static) -> Option<Receiver<T>> {
    {
        let mut p = pending();
        if p.contains_key(&key) {
            return None;
        }
        p.insert(key.clone(), Instant::now());
    }
    let (tx, rx) = channel();
    let k = key.clone();
    let started = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
        let out = work();
        pending().remove(&k);
        let _ = tx.send(out);
    });
    if started.is_err() {
        pending().remove(&key);
        return None;
    }
    Some(rx)
}

/// La risposta, se arriva entro `deadline`.
fn answer<T>(rx: Option<Receiver<T>>, deadline: Instant) -> Option<T> {
    rx?.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()
}

/// Il dispositivo `key` non risponde da almeno `STUCK_AFTER`.
fn stuck_for_long(key: &str) -> bool {
    pending().get(key).is_some_and(|t| t.elapsed() >= STUCK_AFTER)
}

/// Il numero di un disco dal suo percorso nell'albero dei dispositivi.
fn disk_number(path: &str) -> Option<u32> {
    let h = win::open_device_query(path).ok()?;
    let mut dn = [0u8; 12];
    win::ioctl(&h, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut dn).ok()?;
    Some(rd_u32(&dn, 4))
}

/// Interfaccia dei dischi: {53F56307-B6BF-11D0-94F2-00A0C91EFB8B}.
const GUID_DEVINTERFACE_DISK: GUID = GUID {
    data1: 0x53F5_6307,
    data2: 0xB6BF,
    data3: 0x11D0,
    data4: [0x94, 0xF2, 0x00, 0xA0, 0xC9, 0x1E, 0xFB, 0x8B],
};

fn parse_usb_id(device_id: &str) -> Option<(u16, u16)> {
    let up = device_id.to_ascii_uppercase();
    if !up.starts_with("USB\\") {
        return None;
    }
    let vid = up
        .find("VID_")
        .and_then(|i| u16::from_str_radix(up.get(i + 4..i + 8)?, 16).ok())?;
    let pid = up
        .find("PID_")
        .and_then(|i| u16::from_str_radix(up.get(i + 4..i + 8)?, 16).ok())?;
    Some((vid, pid))
}

/// Posto di un disco nell'albero dei dispositivi di Windows.
#[derive(Default)]
struct DiskNode {
    number: u32,
    instance_id: String,
    usb_id: Option<(u16, u16)>,
    usb_node: String,
    eject_node: String,
}

fn device_id(node: u32) -> Option<String> {
    let mut id = [0u16; 400];
    (unsafe { CM_Get_Device_IDW(node, id.as_mut_ptr(), id.len() as u32, 0) } == 0).then(|| win::from_wide(&id))
}

/// Vero se Windows dichiara scollegabile il dispositivo (lo decide l'hub a cui è attaccato).
fn node_removable(node: u32) -> bool {
    const CM_DRP_CAPABILITIES: u32 = 0x10;
    const CM_DEVCAP_REMOVABLE: u32 = 0x4;
    let (mut value, mut len) = (0u32, 4u32);
    let rc = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            node,
            CM_DRP_CAPABILITIES,
            null_mut(),
            &mut value as *mut u32 as *mut c_void,
            &mut len,
            0,
        )
    };
    rc == 0 && value & CM_DEVCAP_REMOVABLE != 0
}

/// Le interfacce dei dischi presenti: percorso e nodo nell'albero dei dispositivi. Non interroga i
/// dischi: lo fa Windows dai suoi elenchi.
fn disk_interfaces() -> Vec<(String, u32)> {
    const DIGCF_PRESENT: u32 = 0x2;
    const DIGCF_DEVICEINTERFACE: u32 = 0x10;
    let mut found: Vec<(String, u32)> = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if set as isize == -1 || set as isize == 0 {
            return Vec::new();
        }
        for index in 0..256u32 {
            let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null(), &GUID_DEVINTERFACE_DISK, index, &mut ifd) == 0 {
                break;
            }
            let mut needed = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut needed, null_mut());
            if needed < 8 {
                continue;
            }
            // Buffer allineato a 8 byte: cbSize (u32) seguito dal percorso UTF-16.
            let mut raw = vec![0u64; (needed as usize).div_ceil(8) + 1];
            let detail = raw.as_mut_ptr() as *mut c_void;
            *(detail as *mut u32) = 8;
            let mut info: SP_DEVINFO_DATA = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, detail as *mut _, needed, null_mut(), &mut info) == 0 {
                continue;
            }
            let chars = (needed as usize - 4) / 2;
            let path_ptr = (detail as *const u8).add(4) as *const u16;
            found.push((win::from_wide(std::slice::from_raw_parts(path_ptr, chars)), info.DevInst));
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    found
}

/// Domanda di numero fatta a un disco: la risposta, il percorso, il nodo nell'albero.
type NumberAsked = (Option<Receiver<Option<u32>>>, String, u32);

fn ask_numbers() -> Vec<NumberAsked> {
    disk_interfaces()
        .into_iter()
        .map(|(path, inst)| {
            let p = path.clone();
            (ask(format!("numero {path}"), move || disk_number(&p)), path, inst)
        })
        .collect()
}

/// Per ogni disco trova il suo posto nell'albero dei dispositivi e, risalendolo, il bridge USB a
/// cui è collegato (VID e PID) e il dispositivo da scollegare per la rimozione sicura. Il numero di
/// ogni disco lo dà il disco stesso: chi non risponde entro `deadline` tiene quello di prima.
fn disk_nodes(asked: Vec<NumberAsked>, deadline: Instant) -> Vec<DiskNode> {
    let mut out = Vec::new();
    let mut last = LAST_NUMBERS.lock().unwrap_or_else(|e| e.into_inner());
    for (rx, path, inst) in asked {
        let number = match answer(rx, deadline) {
            Some(Some(n)) => {
                last.insert(path, n);
                n
            }
            Some(None) => {
                last.remove(&path);
                continue;
            }
            None => match last.get(&path) {
                Some(&n) => n,
                None => continue,
            },
        };
        let mut node = DiskNode {
            number,
            instance_id: device_id(inst).unwrap_or_default(),
            ..Default::default()
        };
        unsafe {
            let mut at = inst;
            for _ in 0..4 {
                let mut parent = 0u32;
                if CM_Get_Parent(&mut parent, at, 0) != 0 {
                    break;
                }
                if let Some(id) = device_id(parent) {
                    if node.usb_id.is_none() {
                        node.usb_id = parse_usb_id(&id);
                    }
                    // Il dispositivo USB vero e proprio, non una delle interfacce di un dispositivo
                    // composito (MI_xx): è lui che l'hub dichiara scollegabile.
                    let up = id.to_ascii_uppercase();
                    if up.starts_with("USB\\VID_") && !up.contains("&MI_") {
                        if node_removable(parent) {
                            node.eject_node = id.clone();
                        }
                        node.usb_node = id;
                        break;
                    }
                }
                at = parent;
            }
        }
        out.push(node);
    }
    out
}

/// Fotografia di volumi e dischi presenti in questo momento. Ogni disco e ogni volume si interroga
/// in un thread suo, e si aspetta al massimo `WAIT`: chi non risponde resta com'era alla lettura
/// precedente e, dopo `STUCK_AFTER`, è segnato come bloccato.
pub fn scan() -> Inventory {
    fake_stuck("");
    let deadline = Instant::now() + WAIT;
    // Tutte le domande partono insieme, e si aspettano insieme: un disco che non risponde non
    // ritarda le altre.
    let asked_disks: Vec<_> = (0..64u32)
        .map(|n| (n, ask(format!("disco {n}"), move || probe_disk(n))))
        .collect();
    let asked_numbers = ask_numbers();
    let mask = unsafe { GetLogicalDrives() };
    let asked_volumes: Vec<_> = (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let letter = (b'A' + i) as char;
            let probe = move || probe_volume(letter).map(|v| (v, volume_descriptor(letter)));
            (letter, ask(format!("volume {letter}"), probe))
        })
        .collect();

    let mut inv = Inventory::default();
    {
        let mut last = LAST_DISKS.lock().unwrap_or_else(|e| e.into_inner());
        for (n, rx) in asked_disks {
            match answer(rx, deadline) {
                Some(Some(d)) => {
                    last.insert(n, d.clone());
                    inv.disks.push(d);
                }
                Some(None) => {
                    last.remove(&n);
                }
                // Un numero senza disco risponde subito (non si apre): chi tace è un disco che c'è.
                None => {
                    let key = format!("disco {n}");
                    let mut d = last.get(&n).cloned().unwrap_or_else(|| PhysDisk {
                        number: n,
                        ..Default::default()
                    });
                    d.stuck = !last.contains_key(&n) || stuck_for_long(&key);
                    inv.disks.push(d);
                }
            }
        }
    }
    for node in disk_nodes(asked_numbers, deadline) {
        if let Some(d) = inv.disks.iter_mut().find(|d| d.number == node.number) {
            d.usb_id = node.usb_id;
            d.instance_id = node.instance_id;
            d.usb_node = node.usb_node;
            d.eject_node = node.eject_node;
        }
    }
    let mut last = LAST_VOLUMES.lock().unwrap_or_else(|e| e.into_inner());
    for (letter, rx) in asked_volumes {
        let v = match answer(rx, deadline) {
            Some(Some((mut v, descriptor))) => {
                // Le unità virtuali (cloud, dischi RAM) a volte dichiarano di stare sul disco 0:
                // si accetta il disco solo se il volume risponde con lo stesso modello e bus.
                if v.disks.len() == 1 {
                    let same = match (descriptor, inv.disk(v.disks[0])) {
                        (Some((model, serial, bus)), Some(d)) => {
                            d.stuck || (model == d.model && serial == d.serial && bus == d.bus)
                        }
                        _ => false,
                    };
                    if !same {
                        v.disks.clear();
                    }
                }
                last.insert(letter, v.clone());
                v
            }
            Some(None) => {
                last.remove(&letter);
                continue;
            }
            // Un volume che non risponde (il file system aspetta il disco): quello di prima, o solo
            // la lettera se non lo si è mai letto.
            None => {
                let key = format!("volume {letter}");
                let mut v = last.get(&letter).cloned().unwrap_or_else(|| Volume {
                    letter,
                    ..Default::default()
                });
                v.stuck = !last.contains_key(&letter) || stuck_for_long(&key);
                v
            }
        };
        for n in &v.disks {
            if let Some(d) = inv.disks.iter_mut().find(|d| d.number == *n) {
                d.letters.push(letter);
            }
        }
        inv.volumes.push(v);
    }
    inv
}

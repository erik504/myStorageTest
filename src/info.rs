//! Tutti i dati di un volume e dei dischi su cui sta (scheda Info e `--info`), ed espulsione dei
//! dispositivi rimovibili. Niente di tutto questo richiede i privilegi di amministratore.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::time::Duration;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_IDW,
    CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CM_Get_Parent, CM_Locate_DevNodeW,
    CM_Request_Device_EjectW,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceW, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
};
use windows_sys::core::GUID;

use crate::disks::{self, Inventory, PhysDisk, Volume, bus_name};
use crate::smart::{self, Health, SmartReport};
use crate::win::{self, human_bytes, rd_u16, rd_u32, rd_u64, thousands};

const IOCTL_DISK_GET_DRIVE_LAYOUT_EX: u32 = 0x0007_0050;
const IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX: u32 = 0x0022_0448;
const IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2: u32 = 0x0022_045C;
const IOCTL_USB_GET_PORT_CONNECTOR_PROPERTIES: u32 = 0x0022_0458;
const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
const IOCTL_STORAGE_MEDIA_REMOVAL: u32 = 0x002D_4804;
const IOCTL_STORAGE_EJECT_MEDIA: u32 = 0x002D_4808;

/// Interfaccia degli hub USB: {F18A0E88-C30C-11D0-8815-00A0C906BED8}.
const GUID_DEVINTERFACE_USB_HUB: GUID = GUID {
    data1: 0xF18A_0E88,
    data2: 0xC30C,
    data3: 0x11D0,
    data4: [0x88, 0x15, 0x00, 0xA0, 0xC9, 0x06, 0xBE, 0xD8],
};

// ---------------------------------------------------------------------------------------------
// Dati
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Partition {
    pub number: u32,
    pub offset: u64,
    pub length: u64,
    /// Tipo in chiaro: "Dati", "Sistema EFI"…
    pub kind: String,
    /// Nome scritto nella tabella delle partizioni (solo GPT).
    pub name: String,
    /// Partizione d'avvio (solo MBR).
    pub active: bool,
    pub letter: Option<char>,
}

/// Collegamento USB di un disco, come lo vede l'hub a cui è attaccato.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsbLink {
    /// Porta dell'hub.
    pub port: u32,
    /// Velocità a cui lavora: 0 Low, 1 Full, 2 High, 3 SuperSpeed, 4 SuperSpeedPlus.
    pub speed: u8,
    /// Velocità di cui il dispositivo sarebbe capace, sulla stessa scala.
    pub capable: u8,
    /// Versione dello standard dichiarata dal dispositivo (bcdUSB: 0x0320 = 3.2).
    pub bcd: u16,
    /// La porta è USB 3 (`Some(true)`), solo USB 2 (`Some(false)`), o l'hub non lo dice.
    pub port_usb3: Option<bool>,
    /// Porta del controller USB (hub radice) e non di un hub esterno.
    pub root_port: bool,
    pub type_c: bool,
}

/// Nome della velocità USB con il nome attuale dello standard.
pub fn usb_speed_label(speed: u8) -> &'static str {
    match speed {
        0 => "USB 1.1 Low Speed (1,5 Mbit/s)",
        1 => "USB 1.1 Full Speed (12 Mbit/s)",
        2 => "USB 2.0 High Speed (480 Mbit/s)",
        3 => "USB 3.2 Gen 1 (5 Gbit/s)",
        _ => "USB 3.2 Gen 2 (10 Gbit/s o più)",
    }
}

/// Collegamento PCI Express di un disco NVMe: di che cosa è capace e a che cosa si è accordato con
/// la porta. Velocità in generazioni (1 = 2,5 GT/s … 5 = 32 GT/s), larghezza in linee.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PcieLink {
    pub max_gen: u32,
    pub max_width: u32,
    pub generation: u32,
    pub width: u32,
}

/// Collegamento SATA, dai dati di IDENTIFY del disco.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SataLink {
    /// Revisione dello standard dichiarata, es. "SATA 3.3".
    pub version: Option<&'static str>,
    /// Generazione massima del disco: 1 = 1,5 Gbit/s, 2 = 3 Gbit/s, 3 = 6 Gbit/s.
    pub max_gen: u8,
    /// Generazione a cui si è accordato con il controller; 0 = non dichiarata.
    pub generation: u8,
}

pub fn pcie_label(generation: u32, width: u32) -> String {
    let version = match generation {
        1 => "1.0",
        2 => "2.0",
        3 => "3.0",
        4 => "4.0",
        5 => "5.0",
        6 => "6.0",
        _ => "?",
    };
    format!("PCIe {version} x{width}")
}

/// Banda di una linea PCIe in MB/s, al netto della codifica (8b/10b fino alla 2.0, 128b/130b dopo).
pub fn pcie_lane_mb(generation: u32) -> f64 {
    match generation {
        1 => 250.0,
        2 => 500.0,
        3 => 984.6,
        4 => 1969.2,
        5 => 3938.5,
        _ => 7563.0,
    }
}

/// Una banda in MB/s scritta per esteso: "600 MB/s", "7,9 GB/s".
pub fn rate(mb: f64) -> String {
    let gb = mb / 1000.0;
    if gb < 1.0 {
        format!("{mb:.0} MB/s")
    } else if gb < 10.0 {
        format!("{gb:.1} GB/s").replace('.', ",")
    } else {
        format!("{gb:.0} GB/s")
    }
}

/// Banda massima di un collegamento PCIe, al netto della codifica: "7,9 GB/s".
fn pcie_bandwidth(generation: u32, width: u32) -> String {
    let gb = pcie_lane_mb(generation) * width as f64 / 1000.0;
    if gb < 1.0 {
        format!("{:.0} MB/s", gb * 1000.0)
    } else {
        format!("{gb:.1} GB/s").replace('.', ",")
    }
}

pub fn pcie_gts(generation: u32) -> &'static str {
    match generation {
        1 => "2,5 GT/s",
        2 => "5 GT/s",
        3 => "8 GT/s",
        4 => "16 GT/s",
        5 => "32 GT/s",
        _ => "64 GT/s",
    }
}

fn sata_label(generation: u8) -> &'static str {
    match generation {
        1 => "1,5 Gbit/s (SATA I, fino a 150 MB/s)",
        2 => "3 Gbit/s (SATA II, fino a 300 MB/s)",
        _ => "6 Gbit/s (SATA III, fino a 600 MB/s)",
    }
}

/// Collegamento SATA dichiarato nei dati di IDENTIFY DEVICE: parole 76 (velocità supportate),
/// 77 (velocità attuale) e 222 (revisione dello standard).
fn sata_link(id: &[u8]) -> Option<SataLink> {
    let word = |n: usize| rd_u16(id, n * 2);
    let caps = word(76);
    if caps == 0 || caps == 0xFFFF {
        return None; // non è un disco SATA
    }
    let max_gen = (1..=3u8).rev().find(|g| caps & (1 << g) != 0)?;
    let generation = ((word(77) >> 1) & 7) as u8;
    let transport = word(222);
    let version = (transport & 0xF000 == 0x1000 && transport & 0x0FFF != 0)
        .then(|| {
            const NAMES: [&str; 11] = [
                "ATA8-AST",
                "SATA 1.0a",
                "SATA II Extensions",
                "SATA 2.5",
                "SATA 2.6",
                "SATA 3.0",
                "SATA 3.1",
                "SATA 3.2",
                "SATA 3.3",
                "SATA 3.4",
                "SATA 3.5",
            ];
            let top = 15 - (transport & 0x0FFF).leading_zeros() as usize;
            NAMES.get(top).copied().unwrap_or("SATA oltre la 3.5")
        });
    Some(SataLink {
        version,
        max_gen,
        generation: if (1..=3).contains(&generation) { generation } else { 0 },
    })
}

/// Versione dello standard NVMe dichiarata in Identify Controller (byte 80–83): "NVMe 1.4".
fn nvme_version(id: &[u8]) -> Option<String> {
    let ver = rd_u32(id, 80);
    let (major, minor, tertiary) = (ver >> 16, (ver >> 8) & 0xFF, ver & 0xFF);
    if major == 0 || major > 9 {
        return None;
    }
    Some(if tertiary > 0 {
        format!("NVMe {major}.{minor}.{tertiary}")
    } else {
        format!("NVMe {major}.{minor}")
    })
}

/// Una proprietà numerica di un dispositivo nell'albero di Windows.
fn devnode_u32(node: u32, key: &DEVPROPKEY) -> Option<u32> {
    let (mut kind, mut value, mut len) = (0u32, 0u32, 4u32);
    let rc = unsafe {
        CM_Get_DevNode_PropertyW(node, key, &mut kind, &mut value as *mut u32 as *mut u8, &mut len, 0)
    };
    (rc == 0 && len == 4).then_some(value)
}

/// Collegamento PCIe del controller da cui dipende il disco (per gli NVMe è il disco stesso).
fn pcie_link(instance_id: &str) -> Option<PcieLink> {
    let id = win::wide(instance_id);
    let (mut node, mut parent) = (0u32, 0u32);
    unsafe {
        if CM_Locate_DevNodeW(&mut node, id.as_ptr(), 0) != 0 || CM_Get_Parent(&mut parent, node, 0) != 0 {
            return None;
        }
    }
    let mut pid = [0u16; 400];
    if unsafe { CM_Get_Device_IDW(parent, pid.as_mut_ptr(), pid.len() as u32, 0) } != 0
        || !win::from_wide(&pid).to_ascii_uppercase().starts_with("PCI\\")
    {
        return None;
    }
    pcie_link_node(parent)
}

/// Collegamento PCIe di un dispositivo PCI dell'albero di Windows: Windows lo dichiara solo per i
/// dispositivi finali, non per le porte e i bridge.
pub fn pcie_link_node(node: u32) -> Option<PcieLink> {
    const PCI: windows_sys::core::GUID = windows_sys::core::GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62);
    let key = |pid: u32| DEVPROPKEY { fmtid: PCI, pid };
    let link = PcieLink {
        generation: devnode_u32(node, &key(9))?,
        width: devnode_u32(node, &key(10))?,
        max_gen: devnode_u32(node, &key(11))?,
        max_width: devnode_u32(node, &key(12))?,
    };
    (link.max_gen > 0 && link.max_width > 0 && link.generation > 0 && link.width > 0).then_some(link)
}

#[derive(Clone, Debug, Default)]
pub struct DiskDetails {
    pub number: u32,
    pub logical_sector: u32,
    pub physical_sector: u32,
    pub trim: Option<bool>,
    pub write_cache: Option<bool>,
    /// "GPT" o "MBR"; vuoto se il disco non ha una tabella delle partizioni leggibile.
    pub scheme: &'static str,
    pub partitions: Vec<Partition>,
    pub usb: Option<UsbLink>,
    pub pcie: Option<PcieLink>,
    /// Per un disco USB è il collegamento SATA fra il bridge e il disco, dentro il contenitore.
    pub sata: Option<SataLink>,
    pub nvme_version: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct VolumeDetails {
    pub serial: u32,
    pub max_name: u32,
    pub flags: u32,
    pub sector: u32,
    pub cluster: u32,
    /// Nome del volume per Windows, che non dipende dalla lettera: `\\?\Volume{…}\`.
    pub guid_path: String,
}

/// Dati di dettaglio del volume selezionato e dei suoi dischi.
#[derive(Clone, Debug, Default)]
pub struct Details {
    pub volume: Option<VolumeDetails>,
    pub disks: Vec<DiskDetails>,
}

impl Details {
    pub fn disk(&self, number: u32) -> Option<&DiskDetails> {
        self.disks.iter().find(|d| d.number == number)
    }

    /// La partizione che ospita il volume: disco, schema della tabella, partizione.
    pub fn partition_of(&self, letter: char) -> Option<(u32, &'static str, &Partition)> {
        self.disks.iter().find_map(|d| {
            d.partitions
                .iter()
                .find(|p| p.letter == Some(letter))
                .map(|p| (d.number, d.scheme, p))
        })
    }
}

/// Legge i dati di dettaglio. Alcune domande arrivano fino al disco (cache in scrittura), quindi
/// si fanno una volta sola quando la pagina si apre, non a ogni aggiornamento dell'elenco.
pub fn details(letter: Option<char>, disks: &[PhysDisk]) -> Details {
    Details {
        volume: letter.map(volume_details),
        disks: disks.iter().map(disk_details).collect(),
    }
}

fn volume_details(letter: char) -> VolumeDetails {
    let root = win::wide(&format!("{letter}:\\"));
    let mut v = VolumeDetails::default();
    let mut name = [0u16; 261];
    let mut fs = [0u16; 261];
    let mut guid = [0u16; 64];
    let (mut per_cluster, mut sector, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            &mut v.serial,
            &mut v.max_name,
            &mut v.flags,
            fs.as_mut_ptr(),
            fs.len() as u32,
        );
        if GetDiskFreeSpaceW(root.as_ptr(), &mut per_cluster, &mut sector, &mut free, &mut total) != 0 {
            v.sector = sector;
            v.cluster = per_cluster.saturating_mul(sector);
        }
        if GetVolumeNameForVolumeMountPointW(root.as_ptr(), guid.as_mut_ptr(), guid.len() as u32) != 0 {
            v.guid_path = win::from_wide(&guid);
        }
    }
    v
}

/// Tratti di disco occupati da un volume: disco, inizio, lunghezza.
fn extents(letter: char) -> Vec<(u32, u64, u64)> {
    let Ok(h) = win::open_device_query(&format!(r"\\.\{letter}:")) else {
        return Vec::new();
    };
    let mut buf = [0u8; 2048];
    if win::ioctl(&h, disks::IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut buf).is_err() {
        return Vec::new();
    }
    (0..(rd_u32(&buf, 0) as usize).min(80))
        .map(|i| {
            let at = 8 + i * 24;
            (rd_u32(&buf, at), rd_u64(&buf, at + 8), rd_u64(&buf, at + 16))
        })
        .collect()
}

/// Collegamenti di un disco per il grafo.
pub struct Links {
    pub pcie: Option<PcieLink>,
    pub usb: Option<UsbLink>,
    pub sata: Option<SataLink>,
    /// Velocità di rotazione dichiarata nei dati di IDENTIFY: `Some(0)` = SSD, altrimenti giri al
    /// minuto; `None` se il disco non la dice.
    pub rpm: Option<u16>,
}

/// Collegamenti di un disco per il grafo: PCIe (NVMe interni), USB, SATA. Solo domande che non
/// svegliano i dischi fermi: lo SATA viene dai dati di IDENTIFY che Windows conserva.
pub fn links(d: &PhysDisk) -> Links {
    let usb = usb_link(&d.usb_node);
    let pcie = if d.bus == disks::BUS_NVME && !d.is_usb() { pcie_link(&d.instance_id) } else { None };
    let id = if matches!(d.bus, 14 | 15 | 16) { None } else { smart::identity(d).0 };
    let rpm = id.as_deref().and_then(|id| match rd_u16(id, 217 * 2) {
        1 => Some(0),
        r @ 0x0401..=0xFFFE => Some(r),
        _ => None,
    });
    Links {
        pcie,
        usb,
        sata: id.as_deref().and_then(sata_link),
        rpm,
    }
}

/// Collegamento PCIe in breve, e se va più piano di quanto il disco potrebbe: «PCIe 4.0 x1 (il
/// disco arriverebbe a x4)».
pub fn pcie_short(p: &PcieLink) -> (String, bool) {
    let now = pcie_label(p.generation, p.width);
    if p.generation < p.max_gen || p.width < p.max_width {
        (format!("{now} (il disco arriverebbe a {})", pcie_label(p.max_gen, p.max_width)), true)
    } else {
        (now, false)
    }
}

/// Collegamento USB in breve: «USB 3.2 Gen 1 (5 Gbit/s)», con la velocità a cui il dispositivo
/// arriverebbe se il collegamento lo frena.
pub fn usb_short(u: &UsbLink) -> (String, bool) {
    let now = usb_speed_label(u.speed).to_string();
    if u.capable > u.speed {
        (format!("{now}, arriverebbe a {}", usb_speed_label(u.capable)), true)
    } else {
        (now, false)
    }
}

/// Collegamento SATA in breve: «SATA 6 Gbit/s».
pub fn sata_short(s: &SataLink) -> (String, bool) {
    let name = |g: u8| match g {
        1 => "1,5 Gbit/s",
        2 => "3 Gbit/s",
        _ => "6 Gbit/s",
    };
    if s.generation > 0 && s.generation < s.max_gen {
        (format!("SATA {} (il disco arriverebbe a {})", name(s.generation), name(s.max_gen)), true)
    } else if s.generation > 0 {
        (format!("SATA {}", name(s.generation)), false)
    } else {
        (format!("SATA, fino a {}", name(s.max_gen)), false)
    }
}

fn disk_details(d: &PhysDisk) -> DiskDetails {
    let mut out = DiskDetails {
        number: d.number,
        usb: usb_link(&d.usb_node),
        ..Default::default()
    };
    if d.bus == disks::BUS_NVME && !d.is_usb() {
        out.pcie = pcie_link(&d.instance_id);
    }
    // I dischi virtuali non hanno un'identità propria da chiedere.
    if !matches!(d.bus, 14 | 15 | 16) {
        let (ata, nvme) = smart::identity(d);
        out.sata = ata.as_deref().and_then(sata_link);
        out.nvme_version = nvme.as_deref().and_then(nvme_version);
    }
    let Ok(h) = win::open_device_query(&d.device_path()) else {
        return out;
    };
    // Allineamento: dimensione del settore logico e di quello fisico.
    let mut buf = [0u8; 64];
    if disks::query_property(&h, 6, &mut buf).is_ok_and(|n| n >= 24) {
        out.logical_sector = rd_u32(&buf, 16);
        out.physical_sector = rd_u32(&buf, 20);
    }
    if out.logical_sector == 0 {
        let mut geo = [0u8; 256];
        if win::ioctl(&h, disks::IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut geo).is_ok() {
            out.logical_sector = rd_u32(&geo, 20);
        }
    }
    let mut buf = [0u8; 16];
    if disks::query_property(&h, 8, &mut buf).is_ok_and(|n| n >= 9) {
        out.trim = Some(buf[8] != 0);
    }
    // Cache in scrittura: 1 = disattivata, 2 = attiva, altro = il disco non lo dice.
    let mut buf = [0u8; 64];
    if disks::query_property(&h, 4, &mut buf).is_ok_and(|n| n >= 16) {
        out.write_cache = match rd_u32(&buf, 12) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        };
    }
    (out.scheme, out.partitions) = partitions(&h);
    for letter in &d.letters {
        for (disk, start, _) in extents(*letter) {
            if disk != d.number {
                continue;
            }
            if let Some(p) = out
                .partitions
                .iter_mut()
                .find(|p| start >= p.offset && start < p.offset + p.length)
            {
                p.letter = Some(*letter);
            }
        }
    }
    out
}

/// Tabella delle partizioni del disco: schema ("GPT", "MBR") e partizioni in ordine di posizione.
fn partitions(h: &win::Handle) -> (&'static str, Vec<Partition>) {
    // DRIVE_LAYOUT_INFORMATION_EX: 48 byte di intestazione, poi una voce di 144 byte per partizione.
    const HEAD: usize = 48;
    const ENTRY: usize = 144;
    let mut buf = vec![0u8; HEAD + ENTRY * 128];
    if win::ioctl(h, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, &[], &mut buf).is_err() {
        return ("", Vec::new());
    }
    let style = rd_u32(&buf, 0);
    let scheme = match style {
        0 => "MBR",
        1 => "GPT",
        _ => return ("", Vec::new()),
    };
    let mut out = Vec::new();
    for i in 0..(rd_u32(&buf, 4) as usize).min(128) {
        let e = &buf[HEAD + i * ENTRY..HEAD + (i + 1) * ENTRY];
        let length = rd_u64(e, 16);
        if length == 0 {
            continue;
        }
        let mut p = Partition {
            number: rd_u32(e, 24),
            offset: rd_u64(e, 8),
            length,
            ..Default::default()
        };
        if style == 0 {
            // Le voci vuote e il contenitore delle partizioni logiche non sono partizioni da mostrare.
            if matches!(e[32], 0x00 | 0x05 | 0x0F) {
                continue;
            }
            p.kind = mbr_kind(e[32]);
            p.active = e[33] != 0;
        } else {
            p.kind = gpt_kind(&e[32..48]);
            let name: Vec<u16> = e[72..144]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            p.name = win::from_wide(&name);
        }
        out.push(p);
    }
    out.sort_by_key(|p| p.offset);
    (scheme, out)
}

fn mbr_kind(kind: u8) -> String {
    match kind {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x07 => "NTFS o exFAT",
        0x0B | 0x0C => "FAT32",
        0x27 => "Ripristino di Windows",
        0x42 => "Dischi dinamici",
        0x82 => "Swap di Linux",
        0x83 => "Linux",
        0x8E => "Linux LVM",
        0xEE => "Protettiva GPT",
        0xEF => "Sistema EFI",
        other => return format!("Tipo {other:02X}h"),
    }
    .to_string()
}

fn gpt_kind(guid: &[u8]) -> String {
    let id = format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        rd_u32(guid, 0),
        rd_u16(guid, 4),
        rd_u16(guid, 6),
        guid[8],
        guid[9],
        guid[10],
        guid[11],
        guid[12],
        guid[13],
        guid[14],
        guid[15]
    );
    match id.as_str() {
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "Dati",
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "Sistema EFI",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Riservata Microsoft",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Ripristino di Windows",
        "5808C8AA-7E8F-42E0-85D2-E1E90434CFB3" => "Dischi dinamici (metadati)",
        "AF9B60A0-1431-4F62-BC68-3311714A69AD" => "Dischi dinamici (dati)",
        "E75CAF8F-F680-4CEE-AFA3-B001E56EFC2D" => "Spazi di archiviazione",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "Swap di Linux",
        "E6D6D379-F507-44C2-A23C-238F2A3DF928" => "Linux LVM",
        "48465300-0000-11AA-AA11-00306543ECAC" => "Apple HFS+",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "Apple APFS",
        _ => return id,
    }
    .to_string()
}

/// Velocità del collegamento USB: la si chiede all'hub a cui il dispositivo è attaccato, per la
/// porta che occupa.
pub fn usb_link(usb_node: &str) -> Option<UsbLink> {
    const CM_DRP_ADDRESS: u32 = 0x1D;
    if usb_node.is_empty() {
        return None;
    }
    let id = win::wide(usb_node);
    let mut hub_id = [0u16; 400];
    let (mut port, mut len) = (0u32, 4u32);
    let path = unsafe {
        let (mut node, mut hub) = (0u32, 0u32);
        if CM_Locate_DevNodeW(&mut node, id.as_ptr(), 0) != 0 {
            return None;
        }
        // Per un dispositivo USB l'indirizzo è il numero della porta dell'hub.
        let rc = CM_Get_DevNode_Registry_PropertyW(
            node,
            CM_DRP_ADDRESS,
            null_mut(),
            &mut port as *mut u32 as *mut c_void,
            &mut len,
            0,
        );
        if rc != 0 || port == 0 || CM_Get_Parent(&mut hub, node, 0) != 0 {
            return None;
        }
        if CM_Get_Device_IDW(hub, hub_id.as_mut_ptr(), hub_id.len() as u32, 0) != 0 {
            return None;
        }
        let mut size = 0u32;
        if CM_Get_Device_Interface_List_SizeW(&mut size, &GUID_DEVINTERFACE_USB_HUB, hub_id.as_ptr(), 0) != 0
            || size < 2
        {
            return None;
        }
        let mut list = vec![0u16; size as usize];
        if CM_Get_Device_Interface_ListW(&GUID_DEVINTERFACE_USB_HUB, hub_id.as_ptr(), list.as_mut_ptr(), size, 0) != 0
        {
            return None;
        }
        win::from_wide(&list)
    };
    let h = win::open(&path, win::GENERIC_WRITE, win::SHARE_ALL, win::OPEN_EXISTING, 0).ok()?;

    // USB_NODE_CONNECTION_INFORMATION_EX (senza allineamento): porta, descrittore del dispositivo
    // (18 byte, con bcdUSB al byte 6), configurazione, velocità al byte 23.
    let mut info = [0u8; 512];
    info[0..4].copy_from_slice(&port.to_le_bytes());
    win::ioctl_inout(&h, IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX, &mut info).ok()?;
    let mut link = UsbLink {
        port,
        speed: info[23].min(3),
        bcd: rd_u16(&info, 6),
        root_port: win::from_wide(&hub_id).to_ascii_uppercase().starts_with("USB\\ROOT_HUB"),
        ..Default::default()
    };
    // Proprietà della porta: il bit 3 dice se il connettore è USB-C.
    let mut props = [0u8; 512];
    props[0..4].copy_from_slice(&port.to_le_bytes());
    if win::ioctl_inout(&h, IOCTL_USB_GET_PORT_CONNECTOR_PROPERTIES, &mut props).is_ok() {
        link.type_c = rd_u32(&props, 8) & 0x8 != 0;
    }
    // La versione 2 distingue le velocità di USB 3 e dice di che cosa sarebbe capace il dispositivo:
    // porta, lunghezza, protocolli capiti da chi chiede (1.1, 2.0, 3.0), poi le risposte: al posto
    // dei protocolli quelli della porta, e le capacità del dispositivo.
    let mut v2 = [0u8; 16];
    v2[0..4].copy_from_slice(&port.to_le_bytes());
    v2[4..8].copy_from_slice(&16u32.to_le_bytes());
    v2[8..12].copy_from_slice(&7u32.to_le_bytes());
    if win::ioctl_inout(&h, IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2, &mut v2).is_ok() {
        let protocols = rd_u32(&v2, 8);
        if protocols != 0 {
            link.port_usb3 = Some(protocols & 0x4 != 0);
        }
        let flags = rd_u32(&v2, 12);
        if flags & 0x4 != 0 {
            link.speed = 4;
        } else if flags & 0x1 != 0 {
            link.speed = 3;
        }
        if flags & 0x8 != 0 {
            link.capable = 4;
        } else if flags & 0x2 != 0 {
            link.capable = 3;
        }
    }
    link.capable = link.capable.max(link.speed);
    Some(link)
}

// ---------------------------------------------------------------------------------------------
// Righe della scheda Info
// ---------------------------------------------------------------------------------------------

/// Una riga di dati: voce, valore, e se il valore merita attenzione.
pub struct Row {
    pub label: &'static str,
    pub value: String,
    pub warn: bool,
}

fn row(label: &'static str, value: impl Into<String>) -> Row {
    Row {
        label,
        value: value.into(),
        warn: false,
    }
}

fn warning(label: &'static str, value: impl Into<String>) -> Row {
    Row {
        label,
        value: value.into(),
        warn: true,
    }
}

/// "931,5 GB (1.000.202.039.296 byte)".
fn exact(bytes: u64) -> String {
    format!("{} ({} byte)", human_bytes(bytes), thousands(bytes))
}

/// Dimensione di un settore o di un cluster: "512 byte", "4 KB".
fn block_size(bytes: u32) -> String {
    match bytes {
        b if b >= 1 << 20 && b % (1 << 20) == 0 => format!("{} MB", b >> 20),
        b if b >= 1024 && b % 1024 == 0 => format!("{} KB", b >> 10),
        b => format!("{b} byte"),
    }
}

/// Quota in percentuale: "45 %", "0,3 %".
pub fn percent(part: u64, total: u64) -> String {
    let p = part as f64 * 100.0 / total.max(1) as f64;
    if p > 0.0 && p < 10.0 {
        format!("{p:.1} %").replace('.', ",")
    } else {
        format!("{p:.0} %")
    }
}

fn fs_features(flags: u32) -> String {
    const NAMES: [(u32, &str); 9] = [
        (0x0000_0008, "permessi sui file"),
        (0x0000_0010, "compressione"),
        (0x0002_0000, "cifratura dei file"),
        (0x0000_0020, "quote per utente"),
        (0x0040_0000, "collegamenti fisici"),
        (0x0000_0080, "collegamenti simbolici"),
        (0x0000_0040, "file sparsi"),
        (0x0200_0000, "registro delle modifiche"),
        (0x0400_0000, "controllo di integrità"),
    ];
    let list: Vec<&str> = NAMES.iter().filter(|(bit, _)| flags & bit != 0).map(|(_, name)| *name).collect();
    list.join(" · ")
}

pub fn volume_rows(v: &Volume, det: Option<&Details>) -> Vec<Row> {
    let used = v.total.saturating_sub(v.free);
    let mut rows = vec![
        row("Etichetta", if v.label.is_empty() { "(nessuna)" } else { v.label.as_str() }),
        row("File system", v.fs.as_str()),
        row("Capacità", exact(v.total)),
        row("Spazio occupato", format!("{} · {}", exact(used), percent(used, v.total))),
        row("Spazio libero", format!("{} · {}", exact(v.free), percent(v.free, v.total))),
        row("Tipo di unità", if v.removable { "Supporto rimovibile" } else { "Disco fisso" }),
    ];
    let Some(det) = det else { return rows };
    if v.disks.len() > 1 {
        let list: Vec<String> = v.disks.iter().map(|n| n.to_string()).collect();
        rows.push(row("Dischi", format!("volume distribuito sui dischi {}", list.join(", "))));
    } else if let Some((disk, scheme, p)) = det.partition_of(v.letter) {
        let mut text = format!("n. {} del disco {disk} · {} ({scheme})", p.number, p.kind);
        if !p.name.is_empty() {
            text.push_str(&format!(" · «{}»", p.name));
        }
        text.push_str(&format!(" · inizia a {}", human_bytes(p.offset)));
        rows.push(row("Partizione", text));
    }
    let Some(d) = &det.volume else { return rows };
    if d.cluster > 0 {
        rows.push(row(
            "Unità di allocazione",
            format!("{} · settori da {}", block_size(d.cluster), block_size(d.sector)),
        ));
    }
    rows.push(row(
        "Numero di serie del volume",
        format!("{:04X}-{:04X}", d.serial >> 16, d.serial & 0xFFFF),
    ));
    if d.max_name > 0 {
        rows.push(row("Nomi dei file", format!("fino a {} caratteri", d.max_name)));
    }
    // Solo sui dischi veri: un'unità virtuale che si presenta come FAT32 non ha quel limite.
    if v.fs.eq_ignore_ascii_case("FAT32") && !v.disks.is_empty() {
        rows.push(row("Dimensione massima di un file", "4 GB (limite di FAT32)"));
    }
    let features = fs_features(d.flags);
    if !features.is_empty() {
        rows.push(row("Funzioni del file system", features));
    }
    if d.flags & 0x0000_8000 != 0 {
        rows.push(row("Compressione", "l'intero volume è compresso"));
    }
    if d.flags & 0x0008_0000 != 0 {
        rows.push(warning("Scrittura", "il volume è in sola lettura"));
    }
    if !d.guid_path.is_empty() {
        rows.push(row("Nome per Windows", d.guid_path.as_str()));
    }
    rows
}

/// Interfaccia del disco e collegamento effettivo: di che cosa è capace e a che cosa lavora.
fn link_rows(d: &PhysDisk, x: &DiskDetails) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(p) = x.pcie {
        let mut text = pcie_label(p.max_gen, p.max_width);
        if let Some(v) = &x.nvme_version {
            text = format!("{v} su {text}");
        }
        text.push_str(&format!(
            " · {} per linea, fino a {}",
            pcie_gts(p.max_gen),
            pcie_bandwidth(p.max_gen, p.max_width)
        ));
        rows.push(row("Interfaccia", text));
        let now = format!("{}, fino a {}", pcie_label(p.generation, p.width), pcie_bandwidth(p.generation, p.width));
        if p.generation < p.max_gen || p.width < p.max_width {
            let mut why = Vec::new();
            if p.width < p.max_width {
                why.push(format!(
                    "usa {} su {}: lo slot ne ha meno, la scheda madre le divide con altri dispositivi, oppure il disco non fa bene contatto",
                    if p.width == 1 { "1 linea".to_string() } else { format!("{} linee", p.width) },
                    p.max_width
                ));
            }
            if p.generation < p.max_gen {
                why.push(
                    "va a una generazione più lenta: lo slot o il processore non arrivano oltre, oppure il disco ha rallentato il collegamento perché è inattivo"
                        .to_string(),
                );
            }
            rows.push(warning(
                "Collegamento attuale",
                format!(
                    "{now} — il disco arriverebbe a {}: {}",
                    pcie_label(p.max_gen, p.max_width),
                    why.join("; ")
                ),
            ));
        } else {
            rows.push(row("Collegamento attuale", now));
        }
    } else if let Some(v) = &x.nvme_version {
        rows.push(row("Interfaccia", v.as_str()));
    }

    if let Some(u) = x.usb {
        let mut text = usb_speed_label(u.capable).to_string();
        if u.bcd >= 0x0300 {
            text.push_str(&format!(" · dichiara USB {}.{}", u.bcd >> 8, (u.bcd >> 4) & 0xF));
        }
        rows.push(row("Interfaccia", text));
        let mut now = format!(
            "{} · porta {} {}",
            usb_speed_label(u.speed),
            u.port,
            if u.root_port { "del controller USB" } else { "di un hub" }
        );
        match u.port_usb3 {
            Some(true) => now.push_str(" (porta USB 3)"),
            Some(false) => now.push_str(" (porta USB 2)"),
            None => {}
        }
        if u.type_c {
            now.push_str(" · connettore USB-C");
        }
        if u.capable > u.speed {
            let why = match u.port_usb3 {
                Some(false) => "la porta è USB 2 (o lo è un hub o una prolunga lungo il collegamento)",
                Some(true) => "la porta è USB 3, quindi lo limitano il cavo o il connettore, che non fanno contatto sulle linee USB 3",
                None => "lo limitano la porta, il cavo o l'hub",
            };
            rows.push(warning(
                "Collegamento attuale",
                format!("{now} — il dispositivo arriverebbe a {}: {why}", usb_speed_label(u.capable)),
            ));
        } else {
            rows.push(row("Collegamento attuale", now));
        }
    }

    if let Some(s) = x.sata {
        let label = if d.is_usb() { "SATA nel contenitore" } else { "Interfaccia" };
        let version = s.version.map(|v| format!("{v} · ")).unwrap_or_default();
        let current = if d.is_usb() { "SATA nel contenitore, attuale" } else { "Collegamento attuale" };
        rows.push(row(label, format!("{version}fino a {}", sata_label(s.max_gen))));
        if s.generation > 0 && s.generation < s.max_gen {
            rows.push(warning(
                current,
                format!(
                    "{} — il disco arriverebbe a {}: lo limitano il controller, la porta o il cavo",
                    sata_label(s.generation),
                    sata_label(s.max_gen)
                ),
            ));
        } else if s.generation > 0 {
            rows.push(row(current, sata_label(s.generation)));
        }
    }
    rows
}

pub fn disk_rows(d: &PhysDisk, det: Option<&DiskDetails>, smart: Option<&SmartReport>) -> Vec<Row> {
    let or_unknown = |s: &str| if s.is_empty() { "non dichiarato".to_string() } else { s.to_string() };
    let mut rows = vec![row("Modello", d.title())];
    rows.push(row("Numero di serie", or_unknown(&d.serial)));
    rows.push(row("Firmware", or_unknown(&d.revision)));
    let mut bus = bus_name(d.bus).to_string();
    if let Some((vid, pid)) = d.usb_id {
        if d.bus != disks::BUS_USB {
            bus.push_str(" attraverso USB");
        }
        bus.push_str(&format!(" · bridge {vid:04X}:{pid:04X}"));
    }
    rows.push(row("Bus", bus));
    if let Some(x) = det {
        rows.extend(link_rows(d, x));
    }
    let kind = match d.ssd {
        Some(true) => "SSD",
        Some(false) => "Disco a piatti",
        None => "Non dichiarato",
    };
    rows.push(row(
        "Tipo",
        format!("{kind} · {}", if d.removable { "supporto rimovibile" } else { "supporto fisso" }),
    ));
    // Capacità zero: un alloggiamento vuoto, oppure un disco che non risponde più ma di cui Windows
    // ricorda ancora le partizioni lette al collegamento.
    let remembered = det.is_some_and(|x| !x.partitions.is_empty());
    rows.push(match (d.size, remembered) {
        (0, true) => warning(
            "Capacità",
            "non leggibile: il disco non risponde, ma Windows ne ricorda le partizioni",
        ),
        (0, false) => row("Capacità", "nessun supporto inserito"),
        _ => row("Capacità", exact(d.size)),
    });
    if let Some(x) = det {
        if x.logical_sector > 0 {
            let mut text = format!("logici da {}", block_size(x.logical_sector));
            if x.physical_sector > 0 {
                text.push_str(&format!(" · fisici da {}", block_size(x.physical_sector)));
            }
            rows.push(row("Settori", text));
        }
        if let Some(on) = x.trim {
            rows.push(row("TRIM", if on { "attivo" } else { "non attivo" }));
        }
        if let Some(on) = x.write_cache {
            rows.push(row("Cache in scrittura", if on { "attiva" } else { "disattivata" }));
        }
        if d.size > 0 || remembered {
            rows.push(row(
                "Tabella delle partizioni",
                match (x.scheme, x.partitions.len()) {
                    ("", _) => "nessuna riconosciuta".to_string(),
                    (scheme, 0) => format!("{scheme} · nessuna partizione"),
                    (scheme, 1) => format!("{scheme} · 1 partizione"),
                    (scheme, n) => format!("{scheme} · {n} partizioni"),
                },
            ));
        }
    }
    if !d.letters.is_empty() {
        let letters: Vec<String> = d.letters.iter().map(|l| format!("{l}:")).collect();
        rows.push(row("Volumi", letters.join("  ")));
    }
    if let Some(r) = smart {
        let temp = r.temperature.map(|t| format!(" · {t} °C")).unwrap_or_default();
        if r.available {
            rows.push(Row {
                label: "Stato SMART",
                value: format!("{}{temp}", r.health.label()),
                warn: r.health >= Health::Warning,
            });
            for name in ["Ore di accensione", "Cicli di accensione", "Dati scritti", "Usura"] {
                if let Some(a) = r.attrs.iter().find(|a| a.name == name) {
                    rows.push(row(name, a.value.as_str()));
                }
            }
        } else {
            rows.push(row("Stato SMART", format!("non disponibile{temp}")));
        }
    }
    rows.push(row("Percorso per Windows", d.device_path()));
    if !d.instance_id.is_empty() {
        rows.push(row("Nell'albero dei dispositivi", d.instance_id.as_str()));
    }
    if !d.usb_node.is_empty() {
        rows.push(row("Dispositivo USB", d.usb_node.as_str()));
    }
    rows
}

// ---------------------------------------------------------------------------------------------
// Espulsione
// ---------------------------------------------------------------------------------------------

/// Come si espelle un disco rimovibile.
#[derive(Clone, Debug, PartialEq)]
pub enum Eject {
    /// Rimozione sicura dell'intero dispositivo USB: il dispositivo nell'albero di Windows, i
    /// volumi che se ne vanno con lui e gli altri dischi che ne fanno parte.
    Device {
        node: String,
        letters: Vec<char>,
        others: Vec<u32>,
    },
    /// Espulsione del solo supporto (una scheda in un lettore che ne ospita altre): i suoi volumi.
    Media { letters: Vec<char> },
}

fn letter_list(letters: &[char]) -> String {
    let list: Vec<String> = letters.iter().map(|l| format!("{l}:")).collect();
    list.join(", ")
}

impl Eject {
    pub fn letters(&self) -> &[char] {
        match self {
            Eject::Device { letters, .. } | Eject::Media { letters } => letters,
        }
    }

    /// Che cosa succede premendo «Espelli».
    pub fn describe(&self) -> String {
        match self {
            Eject::Device { letters, others, .. } => {
                let mut text = "Rimozione sicura: Windows ".to_string();
                text.push_str(&match letters.len() {
                    0 => String::new(),
                    1 => format!("chiude il volume {} e ", letter_list(letters)),
                    _ => format!("chiude i volumi {} e ", letter_list(letters)),
                });
                text.push_str("scollega il dispositivo USB, che poi si può staccare.");
                if !others.is_empty() {
                    let list: Vec<String> = others.iter().map(|n| n.to_string()).collect();
                    text.push_str(&format!(
                        " Riguarda anche {} {}, che fa parte dello stesso dispositivo.",
                        if others.len() == 1 { "il disco" } else { "i dischi" },
                        list.join(", ")
                    ));
                }
                text
            }
            Eject::Media { letters } => format!(
                "Espelle il supporto ({}): il lettore resta collegato.",
                letter_list(letters)
            ),
        }
    }

    /// Messaggio a espulsione riuscita.
    pub fn done(&self) -> &'static str {
        match self {
            Eject::Device { .. } => "Ora il dispositivo si può scollegare.",
            Eject::Media { .. } => "Supporto espulso: ora si può togliere.",
        }
    }
}

/// Come espellere il disco; `None` se non è un dispositivo USB o un supporto rimovibile, o se
/// Windows non offre un modo per farlo.
pub fn eject_plan(inv: &Inventory, d: &PhysDisk) -> Option<Eject> {
    if !d.is_usb() && !d.removable {
        return None;
    }
    let siblings: Vec<&PhysDisk> = inv
        .disks
        .iter()
        .filter(|o| o.number != d.number && !d.usb_node.is_empty() && o.usb_node == d.usb_node)
        .collect();
    // Una scheda in un lettore con più alloggiamenti: si espelle lei, non il lettore con le altre.
    if d.removable && (!siblings.is_empty() || d.eject_node.is_empty()) {
        return (!d.letters.is_empty()).then(|| Eject::Media {
            letters: d.letters.clone(),
        });
    }
    if d.eject_node.is_empty() {
        return None;
    }
    let mut letters = d.letters.clone();
    for s in &siblings {
        letters.extend(&s.letters);
    }
    letters.sort();
    letters.dedup();
    Some(Eject::Device {
        node: d.eject_node.clone(),
        letters,
        others: siblings.iter().map(|s| s.number).collect(),
    })
}

/// Esegue l'espulsione. L'errore dice perché Windows non l'ha concessa.
pub fn eject(plan: &Eject) -> Result<(), String> {
    match plan {
        Eject::Device { node, .. } => eject_device(node),
        Eject::Media { letters } => eject_media(letters),
    }
}

fn eject_device(node: &str) -> Result<(), String> {
    const CR_REMOVE_VETOED: u32 = 0x17;
    let id = win::wide(node);
    let mut inst = 0u32;
    if unsafe { CM_Locate_DevNodeW(&mut inst, id.as_ptr(), 0) } != 0 {
        return Err("il dispositivo non è più collegato".into());
    }
    let mut refusal = String::new();
    // Qualche tentativo: una lettura in corso su uno dei volumi tiene il dispositivo occupato per
    // un attimo.
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(700));
        }
        let mut veto = 0i32;
        let mut name = [0u16; 260];
        let rc = unsafe { CM_Request_Device_EjectW(inst, &mut veto, name.as_mut_ptr(), name.len() as u32, 0) };
        if rc == 0 && veto == 0 {
            return Ok(());
        }
        let who = win::from_wide(&name);
        let who = if who.is_empty() { String::new() } else { format!(" ({who})") };
        refusal = match veto {
            // In uso: file aperti, oppure un volume o un driver che non si lascia fermare.
            2 | 5 | 6 | 7 => format!(
                "il dispositivo è in uso{who}: chiudi i file e le finestre aperti sui suoi volumi e riprova"
            ),
            3 | 4 => format!("un programma o un servizio lo tiene occupato{who}"),
            12 => "Windows non lo consente a questo utente".to_string(),
            13 => return Ok(()),
            _ if rc == CR_REMOVE_VETOED => format!("Windows ha rifiutato la rimozione{who}"),
            _ => format!("Windows ha risposto con l'errore {rc}{who}"),
        };
    }
    Err(refusal)
}

fn eject_media(letters: &[char]) -> Result<(), String> {
    let mut handles = Vec::new();
    for l in letters {
        let h = win::open_device_rw(&format!(r"\\.\{l}:"))
            .map_err(|code| format!("impossibile aprire {l}: ({})", win::error_text(code)))?;
        // Il blocco riesce solo se nessuno ha file aperti sul volume; qualche tentativo, perché
        // Esplora risorse e l'antivirus li chiudono con un attimo di ritardo.
        let locked = (0..8).any(|attempt| {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(250));
            }
            win::ioctl(&h, FSCTL_LOCK_VOLUME, &[], &mut []).is_ok()
        });
        if !locked {
            return Err(format!(
                "{l}: è in uso: chiudi i file e le finestre aperti su quel volume e riprova"
            ));
        }
        handles.push(h);
    }
    let Some(first) = handles.first() else {
        return Err("non c'è un supporto da espellere".into());
    };
    for h in &handles {
        let _ = win::ioctl(h, FSCTL_DISMOUNT_VOLUME, &[], &mut []);
    }
    // Toglie l'eventuale blocco dello sportello, poi espelle.
    let _ = win::ioctl(first, IOCTL_STORAGE_MEDIA_REMOVAL, &[0u8], &mut []);
    win::ioctl(first, IOCTL_STORAGE_EJECT_MEDIA, &[], &mut [])
        .map(|_| ())
        .map_err(|code| format!("il dispositivo non ha accettato l'espulsione ({})", win::error_text(code)))
}

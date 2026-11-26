//! Grafo Storage: come sono collegati i dischi. Per ogni disco e per ogni dispositivo PCIe si risale
//! l'albero dei dispositivi di Windows fino al processore, si tengono i passaggi che dicono qualcosa
//! (processore, chipset, controller, hub USB) e si uniscono le catene in un albero unico, con i dischi
//! alla fine. Sui collegamenti: porte, velocità e banda teorica. A destra dei dischi i volumi che ne
//! usano più d'uno e i contenitori cifrati, collegati a ciò che li ospita; in fondo i volumi senza un
//! disco fisico, con il driver che li crea.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Wdk::Storage::FileSystem::{FileFsDriverPathInformation, NtQueryVolumeInformationFile};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Child, CM_Get_DevNode_PropertyW, CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_IDW, CM_Get_Parent,
    CM_Locate_DevNodeW, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;
use windows_sys::Win32::Storage::FileSystem::{GetVolumeNameForVolumeMountPointW, QueryDosDeviceW};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use windows_sys::Win32::System::ProcessStatus::{EnumDeviceDrivers, GetDeviceDriverBaseNameW};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

use crate::disks::{Inventory, PhysDisk, Volume, bus_name};
use crate::icons::Icon;
use crate::info::{self, PcieLink, UsbLink};
use crate::win::{self, human_bytes, rd_u32};

const CM_DRP_DEVICEDESC: u32 = 0x01;
const CM_DRP_SERVICE: u32 = 0x05;
const CM_DRP_CLASS: u32 = 0x08;
const CM_DRP_FRIENDLYNAME: u32 = 0x0D;
const CM_DRP_LOCATION_INFORMATION: u32 = 0x0E;
const CM_DRP_UI_NUMBER: u32 = 0x13;
const CM_DRP_ADDRESS: u32 = 0x1D;
const CM_DRP_LOCATION_PATHS: u32 = 0x24;

/// Identificativi da mostrare e copiare: voce e valore.
pub type Ids = Vec<(String, String)>;

/// Che cosa è un riquadro del grafo: decide la forma e il posto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Cpu,
    Chipset,
    SataController,
    UsbController,
    Controller,
    Hub,
    /// Bridge di un contenitore USB: il chip che fa da tramite fra l'USB e il disco (SATA o NVMe).
    Bridge,
    /// Dispositivo PCIe che non è un disco: scheda grafica, rete, audio…
    Device,
    /// Porta PCIe senza niente collegato.
    Port,
    /// Gruppo: dischi virtuali, volumi senza disco fisico.
    Virtual,
    Disk,
    /// Volume senza un disco fisico, nel gruppo in fondo.
    Volume,
    /// Volume su più dischi, a destra dei dischi.
    Span,
    /// Volume contenuto in un file o in una partizione di un altro volume (contenitore cifrato).
    Container,
}

/// Tipo di collegamento: dà il colore a riquadri e linee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Pcie,
    Sata,
    Usb,
    /// Fatto dal software: dischi virtuali, volumi su più dischi, contenitori.
    Soft,
}

impl Link {
    pub fn name(self) -> &'static str {
        match self {
            Link::Pcie => "PCIe",
            Link::Sata => "SATA",
            Link::Usb => "USB",
            Link::Soft => "software",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub icon: Icon,
    /// Il tipo di collegamento di cui il riquadro fa parte: dà il colore al riquadro.
    pub domain: Link,
    /// Il tipo del collegamento con il riquadro a sinistra: dà il colore alla linea.
    pub link: Link,
    pub title: String,
    pub lines: Vec<String>,
    /// Testo del fumetto: nomi completi e identificativi di Windows.
    pub tip: String,
    /// Etichetta del collegamento con il riquadro a sinistra: porta, velocità.
    pub edge: String,
    /// Banda teorica di quel collegamento in MB/s, se si conosce: dà lo spessore della linea.
    pub bw: Option<f64>,
    /// Fumetto della linea: la banda teorica e da dove viene.
    pub bw_tip: String,
    /// Il collegamento va più piano di quanto il dispositivo potrebbe.
    pub warn: bool,
    /// Identificativi del riquadro: ID istanza di Windows, indirizzo PCI, percorsi di posizione, numero
    /// di serie, nome del volume…
    pub ids: Ids,
    /// Identificativi del collegamento con il riquadro a sinistra: la porta PCIe (indirizzo, ID istanza,
    /// percorsi), la porta dell'hub USB o del controller SATA.
    pub link_ids: Ids,
    pub disk: Option<u32>,
    pub letters: Vec<char>,
    pub children: Vec<usize>,
    /// Per i riquadri a destra dei dischi: i riquadri da cui dipendono (i dischi di un volume su più
    /// dischi, il volume che ospita un contenitore).
    pub hosts: Vec<usize>,
    /// Colonna a destra dei dischi (1 = subito dopo); 0 per i riquadri dell'albero.
    pub side_col: usize,
    /// Chiave per mettere in ordine i fratelli: indirizzo PCI della porta, numero della porta.
    order: String,
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    /// Riquadri a destra dei dischi.
    pub side: Vec<usize>,
}

impl Graph {
    /// L'icona con cui il grafo mostra un disco o un volume senza disco, se c'è.
    pub fn icon_of_disk(&self, number: u32) -> Option<Icon> {
        self.nodes.iter().find(|n| n.kind == Kind::Disk && n.disk == Some(number)).map(|n| n.icon)
    }

    pub fn icon_of_volume(&self, letter: char) -> Option<Icon> {
        self.volume_node(letter).map(|n| n.icon)
    }

    /// Il riquadro di un volume che non è semplicemente su un disco: su più dischi, contenitore,
    /// senza disco fisico.
    pub fn volume_node(&self, letter: char) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|n| matches!(n.kind, Kind::Volume | Kind::Span | Kind::Container) && n.letters.contains(&letter))
    }
}

/// Un dispositivo della catena, con le proprietà che servono per riconoscerlo.
struct Dev {
    node: u32,
    id: String,
    service: String,
    class: String,
    name: String,
    location: String,
    address: Option<u32>,
    /// Percorsi di posizione: «PCIROOT(0)#PCI(0301)#PCI(0000)», «ACPI(_SB_)#ACPI(PCI0)#ACPI(GPP8)…»,
    /// per l'USB «…#USBROOT(0)#USB(8)#USB(1)».
    paths: Vec<String>,
    /// Numero dello slot, se il firmware lo dichiara.
    slot: Option<u32>,
}

fn dev_multi(node: u32, prop: u32) -> Vec<String> {
    let mut buf = [0u16; 2048];
    let mut len = (buf.len() * 2) as u32;
    let rc = unsafe {
        CM_Get_DevNode_Registry_PropertyW(node, prop, null_mut(), buf.as_mut_ptr() as *mut c_void, &mut len, 0)
    };
    if rc != 0 {
        return Vec::new();
    }
    buf[..(len as usize / 2).min(buf.len())]
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Gli identificativi di un dispositivo; `whose` si aggiunge alle voci («del controller NVMe»).
fn dev_ids(dev: &Dev, whose: &str) -> Ids {
    let sep = if whose.is_empty() { "" } else { " " };
    let mut v = vec![(format!("ID istanza{sep}{whose}"), dev.id.clone())];
    match pci_address(&dev.location).filter(|_| dev.id.to_ascii_uppercase().starts_with("PCI\\")) {
        Some(a) => v.push((format!("Indirizzo PCI{sep}{whose}"), a)),
        None if !dev.location.is_empty() => v.push((format!("Posizione{sep}{whose}"), dev.location.clone())),
        None => {}
    }
    for p in &dev.paths {
        let what = if p.starts_with("ACPI") { "Percorso ACPI" } else { "Percorso" };
        v.push((format!("{what}{sep}{whose}"), p.clone()));
    }
    if let Some(n) = dev.slot.filter(|_| dev.id.to_ascii_uppercase().starts_with("PCI\\")) {
        v.push((format!("Slot{sep}{whose}"), n.to_string()));
    }
    v
}

fn dev_string(node: u32, prop: u32) -> String {
    let mut buf = [0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    let rc = unsafe {
        CM_Get_DevNode_Registry_PropertyW(node, prop, null_mut(), buf.as_mut_ptr() as *mut c_void, &mut len, 0)
    };
    if rc == 0 { win::from_wide(&buf) } else { String::new() }
}

fn dev_u32(node: u32, prop: u32) -> Option<u32> {
    let (mut value, mut len) = (0u32, 4u32);
    let rc = unsafe {
        CM_Get_DevNode_Registry_PropertyW(node, prop, null_mut(), &mut value as *mut u32 as *mut c_void, &mut len, 0)
    };
    (rc == 0).then_some(value)
}

fn device_id(node: u32) -> Option<String> {
    let mut id = [0u16; 400];
    (unsafe { CM_Get_Device_IDW(node, id.as_mut_ptr(), id.len() as u32, 0) } == 0).then(|| win::from_wide(&id))
}

/// Classe e sottoclasse PCI di un dispositivo (0x03 = scheda grafica, 0x02 = rete…).
fn pci_class(node: u32) -> Option<(u8, u8)> {
    const PCI: windows_sys::core::GUID = windows_sys::core::GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62);
    let get = |pid: u32| -> Option<u8> {
        let key = DEVPROPKEY { fmtid: PCI, pid };
        let (mut kind, mut buf, mut len) = (0u32, [0u8; 8], 8u32);
        let rc = unsafe { CM_Get_DevNode_PropertyW(node, &key, &mut kind, buf.as_mut_ptr(), &mut len, 0) };
        (rc == 0 && len >= 1).then_some(buf[0])
    };
    Some((get(3)?, get(4)?))
}

/// La catena di un dispositivo, dalla radice dell'albero (esclusa) fino a lui.
fn chain_of(start: u32) -> Vec<Dev> {
    let mut out = Vec::new();
    let mut node = start;
    for _ in 0..24 {
        let Some(id) = device_id(node) else { break };
        let up = id.to_ascii_uppercase();
        if up.starts_with("HTREE\\ROOT") || up.starts_with("ROOT\\ACPI_HAL") || up.starts_with("ACPI_HAL\\") {
            break;
        }
        let friendly = dev_string(node, CM_DRP_FRIENDLYNAME);
        out.push(Dev {
            node,
            name: if friendly.is_empty() { dev_string(node, CM_DRP_DEVICEDESC) } else { friendly },
            service: dev_string(node, CM_DRP_SERVICE).to_ascii_lowercase(),
            class: dev_string(node, CM_DRP_CLASS).to_ascii_lowercase(),
            location: dev_string(node, CM_DRP_LOCATION_INFORMATION),
            address: dev_u32(node, CM_DRP_ADDRESS),
            paths: dev_multi(node, CM_DRP_LOCATION_PATHS),
            slot: dev_u32(node, CM_DRP_UI_NUMBER),
            id,
        });
        let mut parent = 0u32;
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != 0 {
            break;
        }
        node = parent;
    }
    out.reverse();
    out
}

fn chain_of_id(instance_id: &str) -> Vec<Dev> {
    let id = win::wide(instance_id);
    let mut node = 0u32;
    if instance_id.is_empty() || unsafe { CM_Locate_DevNodeW(&mut node, id.as_ptr(), 0) } != 0 {
        return Vec::new();
    }
    chain_of(node)
}

/// Tutti i dispositivi PCI presenti, ponti compresi.
fn pci_devices() -> Vec<u32> {
    const DIGCF_PRESENT: u32 = 0x2;
    const DIGCF_ALLCLASSES: u32 = 0x4;
    let mut out = Vec::new();
    let enumerator = win::wide("PCI");
    unsafe {
        let set = SetupDiGetClassDevsW(null(), enumerator.as_ptr(), null_mut(), DIGCF_PRESENT | DIGCF_ALLCLASSES);
        if set as isize == -1 || set as isize == 0 {
            return out;
        }
        for index in 0..512u32 {
            let mut info: SP_DEVINFO_DATA = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set, index, &mut info) == 0 {
                break;
            }
            out.push(info.DevInst);
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// Nome del processore, dal registro di Windows.
fn cpu_name() -> String {
    let key = win::wide("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0");
    let value = win::wide("ProcessorNameString");
    let mut buf = [0u16; 256];
    let mut len = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
        )
    };
    let name = if rc == 0 { win::from_wide(&buf).trim().to_string() } else { String::new() };
    if name.is_empty() { "Processore".into() } else { short_cpu(&name) }
}

/// Nome del processore senza le parti che non servono: «AMD Ryzen 9 3900X 12-Core Processor» →
/// «AMD Ryzen 9 3900X», «Intel(R) Core(TM) i9-12900K CPU @ 3.20GHz» → «Intel Core i9-12900K».
fn short_cpu(name: &str) -> String {
    let mut s = name.replace("(R)", "").replace("(TM)", "").replace("(tm)", "");
    for cut in [" CPU", " Processor", " with Radeon", " @"] {
        if let Some(at) = s.find(cut) {
            s.truncate(at);
        }
    }
    // «12-Core», «8-Core» e simili alla fine.
    if let Some(at) = s.rfind(' ') {
        if s[at + 1..].to_ascii_lowercase().ends_with("-core") {
            s.truncate(at);
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Venditore e dispositivo di un identificativo PCI o USB: (VEN, DEV) o (VID, PID).
fn ids(id: &str) -> Option<(u16, u16)> {
    let up = id.to_ascii_uppercase();
    let hex = |tag: &str| {
        let at = up.find(tag)? + tag.len();
        u16::from_str_radix(up.get(at..at + 4)?, 16).ok()
    };
    hex("VEN_").zip(hex("DEV_")).or_else(|| hex("VID_").zip(hex("PID_")))
}

/// Nome di qualche produttore di controller e bridge.
fn vendor(v: u16) -> Option<&'static str> {
    Some(match v {
        0x1022 => "AMD",
        0x8086 => "Intel",
        0x1B21 => "ASMedia",
        0x1B4B => "Marvell",
        0x197B | 0x152D => "JMicron",
        0x10DE => "NVIDIA",
        0x1002 => "AMD",
        0x05E3 => "Genesys Logic",
        0x2109 => "VIA Labs",
        0x0BDA | 0x10EC => "Realtek",
        0x174C => "ASMedia",
        0x0BC2 => "Seagate",
        0x1058 => "Western Digital",
        0x0781 => "SanDisk",
        0x04E8 | 0x144D => "Samsung",
        0x0951 => "Kingston",
        0x1908 => "GEMBIRD",
        0x067B => "Prolific",
        0x04B4 => "Cypress",
        0x14E4 => "Broadcom",
        0x168C | 0x17CB => "Qualcomm",
        0x1969 => "Qualcomm Atheros",
        0x1D6A => "Aquantia",
        0x15B7 => "Sandisk / WD",
        0x1987 => "Phison",
        0xC0A9 => "Micron / Crucial",
        0x1DBE => "INNOGRIT",
        _ => return None,
    })
}

/// Il nome del prodotto come lo dichiara il dispositivo stesso sul bus: per l'USB è la stringa del
/// prodotto («RTL9210C», «USB3.1 Storage Device»…), che Windows non mostra nel nome.
fn bus_reported_name(node: u32) -> String {
    let key = DEVPROPKEY {
        fmtid: windows_sys::core::GUID::from_u128(0x540b947e_8b40_45bc_a8a2_6a0b894cbda2),
        pid: 4,
    };
    let (mut kind, mut buf, mut len) = (0u32, [0u16; 256], 512u32);
    let rc = unsafe { CM_Get_DevNode_PropertyW(node, &key, &mut kind, buf.as_mut_ptr() as *mut u8, &mut len, 0) };
    if rc == 0 { win::from_wide(&buf).trim().to_string() } else { String::new() }
}

/// Che cosa collega il bridge di un contenitore USB al disco.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Inner {
    Nvme,
    Sata,
}

/// Il chip del bridge di un contenitore USB: il nome e, se il chip sa collegare un tipo di disco
/// solo, quale. Il nome viene dal prodotto USB; se il prodotto o il disco dichiarano il chip per
/// intero («RTL9210C», «RTL9210B-CG»), si usa quello, che dice anche la revisione. Molti
/// contenitori hanno l'identificativo USB del loro produttore e non quello del chip.
fn bridge_chip(v: u16, p: u16, product: &str, d: &PhysDisk) -> (Option<String>, Option<Inner>) {
    use Inner::{Nvme, Sata};
    let known: Option<(&str, Option<Inner>)> = match (v, p) {
        (0x152D, 0x0583 | 0xA583 | 0xF583) => Some(("JMicron JMS583", Some(Nvme))),
        (0x152D, 0x0581 | 0x1581) => Some(("JMicron JMS581", None)),
        (0x152D, 0x0586) => Some(("JMicron JMS586", None)),
        (0x152D, 0x0578) => Some(("JMicron JMS578", Some(Sata))),
        (0x152D, 0x0567) => Some(("JMicron JMS567", Some(Sata))),
        (0x152D, 0x0561) => Some(("JMicron JMS561", Some(Sata))),
        (0x152D, 0x0539) => Some(("JMicron JMS539", Some(Sata))),
        (0x152D, 0x2329) => Some(("JMicron JM20329", Some(Sata))),
        (0x152D, 0x2338) => Some(("JMicron JM20338", Some(Sata))),
        (0x152D, 0x2339) => Some(("JMicron JM20339", Some(Sata))),
        (0x0BDA, 0x9210) => Some(("Realtek RTL9210", None)),
        (0x0BDA, 0x9220) => Some(("Realtek RTL9220", None)),
        (0x174C, 0x2362) => Some(("ASMedia ASM2362", Some(Nvme))),
        (0x174C, 0x2364) => Some(("ASMedia ASM2364", Some(Nvme))),
        (0x174C, 0x2464) => Some(("ASMedia ASM2464", Some(Nvme))),
        (0x174C, 0x235C) => Some(("ASMedia ASM235CM", Some(Sata))),
        (0x174C, 0x1153 | 0x55AA) => Some(("ASMedia ASM1153", Some(Sata))),
        (0x174C, 0x1351) => Some(("ASMedia ASM1351", Some(Sata))),
        _ => None,
    };
    // Il nome del chip scritto nel prodotto USB o nel disco: «RTL9…», «JMS5…», «JM20…», «ASM1…», «ASM2…».
    let named = [product, d.model.as_str(), d.vendor.as_str()]
        .iter()
        .flat_map(|s| s.split(|c: char| c.is_whitespace() || c == '_' || c == ','))
        .map(|w| w.trim().to_ascii_uppercase())
        .find(|w| ["RTL9", "JMS5", "JM20", "ASM1", "ASM2"].iter().any(|k| w.starts_with(k)) && w.len() >= 7);
    let maker = |chip: &str| match &chip[..2] {
        "RT" => "Realtek",
        "JM" => "JMicron",
        _ => "ASMedia",
    };
    match (known, named) {
        // Il nome per intero, se è dello stesso chip della tabella (o se la tabella non lo conosce).
        (Some((k, only)), Some(n)) if n.starts_with(k.split(' ').nth(1).unwrap_or("")) => {
            (Some(format!("{} {n}", maker(&n))), only)
        }
        (Some((k, only)), _) => (Some(k.to_string()), only),
        (None, Some(n)) => (Some(format!("{} {n}", maker(&n))), None),
        (None, None) => (None, None),
    }
}

/// Come passano i comandi SMART attraverso il bridge, e l'autodiagnosi: quello che il programma
/// sa e ha provato.
fn bridge_smart_note(chip: &str, v: u16, p: u16, inner: Option<Inner>) -> &'static str {
    if inner == Some(Inner::Sata) {
        return "SMART del disco con i comandi SAT (ATA PASS-THROUGH), se il bridge li inoltra.";
    }
    if v == 0x152D && crate::smart::jmicron_nvme_product(p) {
        return if matches!(p, 0x0581 | 0x1581) {
            "SMART del disco con il protocollo JMicron. L'autodiagnosi è disattivata: con un JMS581 il comando \
             ha bloccato il bridge."
        } else {
            "SMART del disco con il protocollo JMicron; l'autodiagnosi si può avviare, ma il bridge potrebbe \
             bloccarsi."
        };
    }
    if chip.starts_with("Realtek") {
        return "SMART del disco con il comando di lettura Realtek (E4h). L'autodiagnosi non passa: RTL9210B e \
                RTL9210C rifiutano il comando.";
    }
    if chip.starts_with("ASMedia") && inner == Some(Inner::Nvme) {
        return "SMART del disco con il comando di lettura ASMedia (E6h); per l'autodiagnosi non c'è una strada nota.";
    }
    ""
}

/// Bus, dispositivo e funzione da «Bus PCI 0, dispositivo 3, funzione 2» (o la stessa frase in
/// un'altra lingua: contano solo i numeri, in quest'ordine).
fn pci_numbers(location: &str) -> Option<(u32, u32, u32)> {
    let numbers: Vec<u32> = location
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    (numbers.len() == 3).then(|| (numbers[0], numbers[1], numbers[2]))
}

/// «Bus PCI 0, dispositivo 3, funzione 2» → «00:03.2».
fn pci_address(location: &str) -> Option<String> {
    pci_numbers(location).map(|(b, d, f)| format!("{b:02x}:{d:02x}.{f}"))
}

fn is_bridge(d: &Dev) -> bool {
    d.id.to_ascii_uppercase().starts_with("PCI\\") && d.service == "pci"
}

/// Ponti interni al processore o al chipset, che non portano a una porta PCIe vera: dietro ci sono
/// le funzioni integrate (USB, SATA, audio).
fn internal_bridge(d: &Dev) -> bool {
    matches!(ids(&d.id), Some((0x1022, 0x1484 | 0x57A4 | 0x1635)))
}

fn is_root(d: &Dev) -> bool {
    let up = d.id.to_ascii_uppercase();
    up.starts_with("ACPI\\PNP0A08") || up.starts_with("ACPI\\PNP0A03")
}

// ---------------------------------------------------------------------------------------------
// Bande teoriche
// ---------------------------------------------------------------------------------------------

fn pcie_bw(l: &PcieLink) -> f64 {
    info::pcie_lane_mb(l.generation) * l.width as f64
}

fn pcie_slow(l: &PcieLink) -> bool {
    l.generation < l.max_gen || l.width < l.max_width
}

/// Fumetto di un collegamento PCIe: come si arriva alla banda teorica, e a che cosa arriverebbe
/// il dispositivo (`what`) se la porta lo frena.
fn pcie_tip(l: &PcieLink, what: &str) -> String {
    let mut s = format!(
        "{}: {} per linea × {} {}, codifica {} → {} teorici in ciascun verso.",
        info::pcie_label(l.generation, l.width),
        info::pcie_gts(l.generation),
        l.width,
        if l.width == 1 { "linea" } else { "linee" },
        if l.generation <= 2 { "8b/10b" } else { "128b/130b" },
        info::rate(pcie_bw(l)),
    );
    if pcie_slow(l) {
        s.push_str(&format!(
            "\n{what} arriverebbe a {} ({}): la porta o lo slot lo frenano.",
            info::pcie_label(l.max_gen, l.max_width),
            info::rate(info::pcie_lane_mb(l.max_gen) * l.max_width as f64)
        ));
    }
    s
}

fn usb_bw(speed: u8) -> f64 {
    [0.1875, 1.5, 60.0, 500.0, 1212.0][speed.min(4) as usize]
}

fn usb_tip(u: &UsbLink, what: &str) -> String {
    let coding = match u.speed {
        0..=2 => "senza contare i dati di controllo",
        3 => "al netto della codifica 8b/10b",
        _ => "al netto della codifica 128b/132b; con 20 Gbit/s sarebbe il doppio",
    };
    let mut s = format!("{}: {} teorici ({coding}).", info::usb_speed_label(u.speed), info::rate(usb_bw(u.speed)));
    if u.capable > u.speed {
        s.push_str(&format!(
            "\n{what} arriverebbe a {} ({}).",
            info::usb_speed_label(u.capable),
            info::rate(usb_bw(u.capable))
        ));
    }
    s
}

fn sata_bw(generation: u8) -> f64 {
    match generation {
        1 => 150.0,
        2 => 300.0,
        _ => 600.0,
    }
}

/// Collegamento PCIe con il riquadro a sinistra: etichetta, banda, fumetto, se frena il dispositivo.
/// Per le funzioni integrate (`internal`) Windows dichiara una porta x16 che non esiste: la banda
/// vera è quella del collegamento del chipset con il processore (`uplink`), se il percorso passa di lì.
fn pci_edge(link: Option<PcieLink>, internal: bool, uplink: Option<f64>, what: &str) -> (String, Option<f64>, String, bool) {
    match link {
        Some(l) if internal => {
            let declared = pcie_bw(&l);
            let tip = match uplink {
                Some(u) => format!(
                    "Collegamento interno al chipset: Windows lo dichiara {} ({}), ma tutto ciò che sta dietro il \
                     chipset divide la banda del suo collegamento con il processore ({}).",
                    info::pcie_label(l.generation, l.width),
                    info::rate(declared),
                    info::rate(u)
                ),
                None => format!(
                    "Collegamento interno al processore: Windows lo dichiara {} ({}), ma non passa da una porta \
                     PCIe vera.",
                    info::pcie_label(l.generation, l.width),
                    info::rate(declared)
                ),
            };
            (String::new(), Some(uplink.map_or(declared, |u| u.min(declared))), tip, false)
        }
        Some(l) => (
            info::pcie_label(l.generation, l.width),
            Some(pcie_bw(&l)),
            pcie_tip(&l, what),
            pcie_slow(&l),
        ),
        None => (String::new(), None, "Windows non dichiara la velocità di questo collegamento.".into(), false),
    }
}

// ---------------------------------------------------------------------------------------------
// Percorsi
// ---------------------------------------------------------------------------------------------

/// Un riquadro di un percorso, prima di finire nell'albero.
struct Item {
    key: String,
    node: Node,
}

fn item(key: String, kind: Kind, icon: Icon, title: String, lines: Vec<String>, tip: String) -> Item {
    let domain = match kind {
        Kind::SataController => Link::Sata,
        Kind::UsbController | Kind::Hub | Kind::Bridge => Link::Usb,
        Kind::Virtual | Kind::Volume | Kind::Span | Kind::Container => Link::Soft,
        _ => Link::Pcie,
    };
    Item {
        key,
        node: Node {
            kind,
            icon,
            domain,
            link: Link::Pcie,
            title,
            lines,
            tip,
            edge: String::new(),
            bw: None,
            bw_tip: String::new(),
            warn: false,
            ids: Vec::new(),
            link_ids: Vec::new(),
            disk: None,
            letters: Vec::new(),
            children: Vec::new(),
            hosts: Vec::new(),
            side_col: 0,
            order: String::new(),
        },
    }
}

/// Porta PCIe attraversata e non ancora attribuita a un riquadro.
struct Port {
    addr: String,
    internal: bool,
    /// Identificativi della porta: vanno sul collegamento con il riquadro che sta dietro.
    ids: Ids,
}

fn port_of(dev: &Dev) -> Port {
    Port {
        addr: pci_address(&dev.location).unwrap_or_default(),
        internal: internal_bridge(dev),
        ids: dev_ids(dev, "della porta"),
    }
}

/// I riquadri del percorso fino al dispositivo `chain[end]` escluso, e quello che il dispositivo
/// deve sapere del percorso.
struct Walk<'a> {
    items: Vec<Item>,
    port: Option<Port>,
    /// Il dispositivo USB nel contenitore (bridge o chiavetta): va sul disco.
    usb_device: Option<&'a Dev>,
    /// Il controller NVMe del disco: è il disco stesso.
    nvme: Option<&'a Dev>,
    /// Banda del collegamento fra chipset e processore, se il percorso passa dal chipset.
    uplink: Option<f64>,
}

fn walk<'a>(chain: &'a [Dev], end: usize, cpu: &str, nvme_disk: bool) -> Walk<'a> {
    let mut w = Walk {
        items: Vec::new(),
        port: None,
        usb_device: None,
        nvme: None,
        uplink: None,
    };
    for i in 0..end {
        let dev = &chain[i];
        let up = dev.id.to_ascii_uppercase();
        if is_root(dev) {
            let mut it = item(
                format!("cpu:{}", dev.id),
                Kind::Cpu,
                Icon::Cpu,
                cpu.to_string(),
                vec!["processore · radice PCIe".into()],
                format!("{cpu}\n{}", dev.name),
            );
            it.node.order = "0".into();
            it.node.ids = dev_ids(dev, "");
            w.items.push(it);
        } else if is_bridge(dev) {
            let parent_bridge = i > 0 && is_bridge(&chain[i - 1]);
            let next_bridge = chain.get(i + 1).is_some_and(is_bridge);
            if parent_bridge && next_bridge && !internal_bridge(dev) {
                // Porta a monte di uno switch PCIe: sulle schede AMD è il chipset.
                let (v, devid) = ids(&dev.id).unwrap_or_default();
                let title = match (v, devid) {
                    (0x1022, 0x57AD) => "Chipset AMD X570".to_string(),
                    (0x1022, _) => "Chipset AMD".to_string(),
                    (v, _) => format!("Switch PCIe {}", vendor(v).unwrap_or("")).trim().to_string(),
                };
                let via = w.port.take();
                let mut it = item(
                    format!("pci:{}", dev.id),
                    Kind::Chipset,
                    if v == 0x1022 { Icon::Chipset } else { Icon::Controller },
                    title.clone(),
                    vec![format!("switch PCIe · {v:04X}:{devid:04X}")],
                    format!(
                        "{}{}",
                        dev.name,
                        via.as_ref().map(|p| format!("\ncollegato alla porta PCIe {} del processore", p.addr)).unwrap_or_default()
                    ),
                );
                it.node.ids = dev_ids(dev, "");
                it.node.link_ids = via.as_ref().map(|p| p.ids.clone()).unwrap_or_default();
                // Windows non dichiara il collegamento del chipset: per l'X570 è noto (PCIe 4.0 x4).
                if (v, devid) == (0x1022, 0x57AD) {
                    let l = PcieLink {
                        max_gen: 4,
                        max_width: 4,
                        generation: 4,
                        width: 4,
                    };
                    it.node.edge = info::pcie_label(4, 4);
                    it.node.bw = Some(pcie_bw(&l));
                    it.node.bw_tip = format!(
                        "Collegamento fra processore e {title} (valore noto del chipset: Windows non lo dichiara).\n{}\n\
                         Tutto ciò che sta dietro il chipset (dischi NVMe e SATA, USB, rete) divide questa banda.",
                        pcie_tip(&l, "Il chipset")
                    );
                    w.uplink = it.node.bw;
                } else {
                    it.node.bw_tip = "Windows non dichiara la velocità del collegamento fra processore e chipset.".into();
                }
                it.node.order = via.map(|p| p.addr).unwrap_or_default();
                w.items.push(it);
            } else {
                w.port = Some(port_of(dev));
            }
        } else if up.starts_with("PCI\\") {
            if dev.service == "stornvme" || (nvme_disk && chain.get(i + 1).is_some_and(|n| n.class == "diskdrive")) {
                // Il controller NVMe è il disco stesso: il suo collegamento va sul disco.
                w.nvme = Some(dev);
                continue;
            }
            w.items.push(controller_item(dev, &mut w.port, w.uplink, true));
        } else if up.starts_with("USB\\ROOT_HUB") {
            // L'hub radice fa parte del controller: i suoi identificativi vanno lì.
            if let Some(ctrl) = w.items.last_mut().filter(|it| it.node.kind == Kind::UsbController) {
                ctrl.node.ids.extend(dev_ids(dev, "dell'hub radice"));
            }
        } else if up.starts_with("USB\\") {
            if dev.service.starts_with("usbhub") {
                let usb3 = dev.name.contains("SuperSpeed") || dev.name.contains("USB 3");
                let (v, p) = ids(&dev.id).unwrap_or_default();
                // Prima l'identificativo, che distingue gli hub; il produttore, se non ci sta, si taglia.
                let maker = vendor(v).map(|m| format!(" · {m}")).unwrap_or_default();
                let mut it = item(
                    format!("usb:{}", dev.id),
                    Kind::Hub,
                    Icon::Hub,
                    if usb3 { "Hub USB 3".into() } else { "Hub USB 2".into() },
                    vec![format!("{v:04X}:{p:04X}{maker}")],
                    dev.name.clone(),
                );
                it.node.link = Link::Usb;
                it.node.ids = dev_ids(dev, "");
                it.node.link_ids = usb_port_ids(dev);
                it.node.edge = dev.address.map(|a| format!("porta {a}")).unwrap_or_default();
                it.node.order = format!("p{:03}", dev.address.unwrap_or(0));
                match info::usb_link(&dev.id) {
                    Some(u) => {
                        // La metà USB 2 di un hub USB 3 si dichiara capace di USB 3: è normale, non è
                        // un collegamento che frena.
                        let u = UsbLink { capable: u.speed, ..u };
                        it.node.bw = Some(usb_bw(u.speed));
                        it.node.bw_tip = usb_tip(&u, "L'hub");
                    }
                    None => it.node.bw_tip = "Windows non dichiara la velocità di questo collegamento USB.".into(),
                }
                w.items.push(it);
            } else if !up.contains("&MI_") && w.usb_device.is_none() {
                w.usb_device = Some(dev);
            }
        } else if w.items.is_empty() {
            // Catene che non partono dal processore: dischi virtuali creati dal software.
            w.items.push(virtual_root(format!("{}\n{}", dev.name, dev.id)));
        }
    }
    w
}

/// Identificativi del collegamento USB di un dispositivo: la porta dell'hub e la posizione.
fn usb_port_ids(dev: &Dev) -> Ids {
    let mut v = Vec::new();
    if let Some(a) = dev.address {
        v.push(("Porta dell'hub".to_string(), a.to_string()));
    }
    if !dev.location.is_empty() {
        v.push(("Posizione".to_string(), dev.location.clone()));
    }
    if let Some(p) = dev.paths.iter().find(|p| p.contains("USB(")) {
        v.push(("Percorso".to_string(), p.clone()));
    }
    v
}

fn virtual_root(tip: String) -> Item {
    item(
        "virtuali".into(),
        Kind::Virtual,
        Icon::Group,
        "Dischi virtuali".into(),
        vec!["creati da Windows o da altri programmi".into()],
        tip,
    )
}

/// Riquadro di un controller PCI (SATA, USB, NVMe senza disco, altre schede). `with_disks`: ha dei
/// dischi dietro (altrimenti lo si dice).
fn controller_item(dev: &Dev, port: &mut Option<Port>, uplink: Option<f64>, with_disks: bool) -> Item {
    let (v, devid) = ids(&dev.id).unwrap_or_default();
    let maker = vendor(v).map(|m| format!("{m} · ")).unwrap_or_default();
    let via = port.take();
    let class = pci_class(dev.node);
    let (kind, icon, title) = if dev.service.starts_with("usb") || dev.class == "usb" || class.is_some_and(|c| c == (0x0C, 0x03)) {
        let name = match dev.service.as_str() {
            "usbxhci" => "Controller USB 3 (xHCI)",
            "usbehci" => "Controller USB 2 (EHCI)",
            _ => "Controller USB",
        };
        (Kind::UsbController, Icon::UsbController, name.to_string())
    } else if dev.service.contains("ahci") || class.is_some_and(|c| c == (0x01, 0x06)) {
        (Kind::SataController, Icon::SataController, "Controller SATA (AHCI)".to_string())
    } else if class.is_some_and(|c| c == (0x01, 0x08)) {
        (Kind::Controller, Icon::Nvme, dev.name.clone())
    } else {
        (Kind::Controller, Icon::Controller, dev.name.clone())
    };
    let link = info::pcie_link_node(dev.node);
    let internal = via.as_ref().is_some_and(|p| p.internal) || link.is_some_and(|l| l.max_width >= 16);
    let (edge, bw, bw_tip, warn) = pci_edge(link, internal, uplink, "Il controller");
    // La porta PCIe del controller va nel fumetto: per quelli dentro il chipset è un dettaglio interno.
    // L'indirizzo PCI distingue due controller uguali.
    let at = pci_address(&dev.location).map(|a| format!(" · {a}")).unwrap_or_default();
    let mut lines = vec![format!("{maker}{v:04X}:{devid:04X}{at}")];
    if !with_disks {
        lines.push("nessun disco collegato".into());
    }
    if internal {
        lines.push(if uplink.is_some() { "dentro il chipset".into() } else { "dentro il processore".into() });
    }
    let mut it = item(
        format!("pci:{}", dev.id),
        kind,
        icon,
        title,
        lines,
        format!(
            "{}{}",
            dev.name,
            via.as_ref().map(|p| format!("\ndietro la porta PCIe {}", p.addr)).unwrap_or_default(),
        ),
    );
    it.node.edge = edge;
    it.node.bw = bw;
    it.node.bw_tip = bw_tip;
    it.node.warn = warn;
    it.node.ids = dev_ids(dev, "");
    it.node.link_ids = via.as_ref().map(|p| p.ids.clone()).unwrap_or_default();
    it.node.order = via.map(|p| p.addr).or_else(|| pci_address(&dev.location)).unwrap_or_default();
    it
}

/// Il tipo di collegamento di un disco, per il colore: lo stesso che usa il grafo.
pub fn disk_link(d: &PhysDisk) -> Link {
    if d.is_usb() {
        Link::Usb
    } else if d.bus == crate::disks::BUS_NVME {
        Link::Pcie
    } else if matches!(d.bus, 14..=16) {
        Link::Soft
    } else {
        Link::Sata
    }
}

/// L'icona di un disco: dal tipo di collegamento, dal supporto e dal nome. `rpm`: velocità di
/// rotazione dichiarata (0 = SSD), quando la si conosce.
pub fn disk_icon(d: &PhysDisk, rpm: Option<u16>) -> Icon {
    let name = format!("{} {}", d.vendor, d.model).to_ascii_uppercase();
    let card = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| matches!(w, "SD" | "SDXC" | "SDHC" | "MICROSD" | "MMC") || w.contains("CARD") || w.contains("READER"));
    let ssd = match rpm {
        Some(0) => Some(true),
        Some(_) => Some(false),
        None => d.ssd,
    };
    match d.bus {
        14..=16 => return Icon::VirtualDisk,
        12 | 13 => return Icon::SdCard,
        _ => {}
    }
    if d.is_usb() {
        if d.removable {
            return if card { Icon::SdCard } else { Icon::UsbStick };
        }
        return match ssd {
            Some(true) if name.contains("PCIE") || name.contains("NVME") => Icon::Nvme,
            Some(true) => Icon::Ssd,
            Some(false) => Icon::Hdd,
            None => Icon::UsbDrive,
        };
    }
    if d.bus == crate::disks::BUS_NVME {
        return Icon::Nvme;
    }
    match ssd {
        Some(true) => Icon::Ssd,
        Some(false) => Icon::Hdd,
        None => Icon::Disk,
    }
}

/// Il percorso di un disco, dalla radice a lui. Restituisce anche il controller NVMe, che nel grafo
/// è il disco stesso.
fn path_of(d: &PhysDisk, cpu: &str) -> (Vec<Item>, Option<String>) {
    let chain = chain_of_id(&d.instance_id);
    let last = chain.len().saturating_sub(1);
    let mut w = walk(&chain, last, cpu, d.bus == crate::disks::BUS_NVME);
    let nvme_id = w.nvme.map(|n| n.id.clone());

    // Un disco di cui Windows non dà la catena (succede ad alcuni dischi virtuali) sta comunque sotto
    // un riquadro, invece di restare isolato.
    let virtual_bus = matches!(d.bus, 14..=16);
    if w.items.is_empty() {
        w.items.push(if virtual_bus {
            virtual_root(String::new())
        } else {
            item(
                "sconosciuto".into(),
                Kind::Virtual,
                Icon::Group,
                "Collegamento non trovato".into(),
                vec!["Windows non dice a che cosa è collegato".into()],
                String::new(),
            )
        });
    }

    // Il disco, alla fine.
    let links = info::links(d);
    let mut lines = vec![format!(
        "{} · {}{}",
        human_bytes(d.size),
        bus_name(d.bus),
        if d.letters.is_empty() {
            String::new()
        } else {
            format!(" · {}", d.letters.iter().map(|l| format!("{l}:")).collect::<Vec<_>>().join(" "))
        }
    )];
    let mut leaf = item(
        format!("disk:{}", d.number),
        Kind::Disk,
        disk_icon(d, links.rpm),
        format!("Disco {} · {}", d.number, d.title()),
        Vec::new(),
        String::new(),
    );
    let n = &mut leaf.node;
    n.domain = if virtual_bus { Link::Soft } else { Link::Sata };
    n.link = n.domain;
    let mut port_tip = String::new();
    // Identificativi del disco, poi del controller NVMe o del dispositivo USB che lo contiene.
    let mut more_ids: Ids = Vec::new();
    if let Some(nvme) = w.nvme {
        n.domain = Link::Pcie;
        n.link = Link::Pcie;
        let via = w.port.take();
        port_tip = via.as_ref().map(|p| format!("\nporta PCIe {}", p.addr)).unwrap_or_default();
        n.link_ids = via.as_ref().map(|p| p.ids.clone()).unwrap_or_default();
        more_ids = dev_ids(nvme, "del controller NVMe");
        n.order = via.map(|p| p.addr).unwrap_or_default();
        match links.pcie.or_else(|| info::pcie_link_node(nvme.node)) {
            Some(l) => {
                n.edge = info::pcie_label(l.generation, l.width);
                n.bw = Some(pcie_bw(&l));
                n.bw_tip = pcie_tip(&l, "Il disco");
                n.warn |= pcie_slow(&l);
                lines.push(format!("NVMe su {}", info::pcie_short(&l).0));
            }
            None => n.bw_tip = "Windows non dichiara la velocità di questo collegamento PCIe.".into(),
        }
    }
    // Il bridge di un contenitore USB, fra l'hub e il disco. Chiavette e schede di memoria (supporti
    // rimovibili) non ne hanno uno da mostrare: il dispositivo USB è il supporto stesso.
    let mut bridge: Option<Item> = None;
    if let Some(dev) = w.usb_device {
        n.domain = Link::Usb;
        n.link = Link::Usb;
        let proto = match dev.service.as_str() {
            "uaspstor" => " · UAS",
            "usbstor" => " · BOT",
            _ => "",
        };
        let (v, p) = ids(&dev.id).unwrap_or_default();
        let who = if d.removable { "Il disco" } else { "Il bridge" };
        let (usb_text, usb_slow, usb_band, usb_band_tip) = match links.usb {
            Some(u) => {
                let (text, slow) = info::usb_short(&u);
                (text, slow, Some(usb_bw(u.speed)), usb_tip(&u, who))
            }
            None => (
                "USB".to_string(),
                false,
                None,
                "Windows non dichiara la velocità di questo collegamento USB.".to_string(),
            ),
        };
        let edge = dev.address.map(|a| format!("porta {a}")).unwrap_or_default();
        let order = format!("p{:03}", dev.address.unwrap_or(0));
        if d.removable {
            let maker = vendor(v).map(|m| format!("{m} ")).unwrap_or_default();
            n.warn |= usb_slow;
            n.bw = usb_band;
            n.bw_tip = usb_band_tip;
            lines.push(format!("{usb_text}{proto} · {maker}{v:04X}:{p:04X}"));
            n.edge = edge;
            n.order = order;
            n.link_ids = usb_port_ids(dev);
            more_ids = dev_ids(dev, "del dispositivo USB");
        } else {
            let product = bus_reported_name(dev.node);
            let (chip, only) = bridge_chip(v, p, &product, d);
            // Che disco c'è dentro: dalla lettura dello SMART, se c'è già stata, altrimenti da quello
            // che il bridge sa collegare, dal disco meccanico (SATA) o dal nome.
            let name = format!("{} {}", d.vendor, d.model).to_ascii_uppercase();
            let inner = crate::smart::usb_inner_nvme(d)
                .map(|nvme| if nvme { Inner::Nvme } else { Inner::Sata })
                .or(links.sata.map(|_| Inner::Sata))
                .or(only)
                .or(links.rpm.filter(|r| *r > 0).map(|_| Inner::Sata))
                .or((name.contains("NVME") || name.contains("PCIE")).then_some(Inner::Nvme));
            let kind_text = match inner {
                Some(Inner::Nvme) => "USB-NVMe",
                Some(Inner::Sata) => "USB-SATA",
                None => "USB",
            };
            let title = chip.clone().unwrap_or_else(|| match vendor(v) {
                Some(m) => format!("Bridge {m}"),
                None => "Bridge USB".to_string(),
            });
            let mut tip = format!("{title}: bridge {kind_text} del contenitore\n{}", dev.name);
            if !product.is_empty() {
                tip.push_str(&format!("\nProdotto USB: {product}"));
            }
            let note = bridge_smart_note(&title, v, p, inner);
            if !note.is_empty() {
                tip.push_str(&format!("\n{note}"));
            }
            let mut b = item(
                format!("bridge:{}", dev.id),
                Kind::Bridge,
                Icon::Bridge,
                title,
                vec![format!("bridge {kind_text} · {v:04X}:{p:04X}"), format!("{usb_text}{proto}")],
                tip,
            );
            b.node.link = Link::Usb;
            b.node.warn = usb_slow;
            b.node.bw = usb_band;
            b.node.bw_tip = usb_band_tip;
            b.node.edge = edge;
            b.node.order = order;
            b.node.link_ids = usb_port_ids(dev);
            b.node.ids = dev_ids(dev, "");
            if !product.is_empty() {
                b.node.ids.push(("Prodotto USB".to_string(), product));
            }
            bridge = Some(b);
            // Il disco sta dietro il bridge: il suo collegamento è quello dentro il contenitore.
            n.order = format!("d{:03}", d.number);
            match inner {
                Some(Inner::Nvme) => {
                    n.link = Link::Pcie;
                    n.edge = "NVMe".into();
                    n.bw_tip = "Dentro il contenitore il bridge parla con l'SSD in NVMe, su PCIe. Windows non dichiara \
                                la velocità di questo collegamento: di solito il limite è quello USB."
                        .into();
                }
                Some(Inner::Sata) => {
                    n.link = Link::Sata;
                    n.edge = "SATA".into();
                    n.bw_tip = "Dentro il contenitore il bridge parla con il disco in SATA. La velocità di questo \
                                collegamento si legge da amministratore, attraverso il bridge."
                        .into();
                }
                None => {
                    n.bw_tip = "Collegamento dentro il contenitore, fra il bridge e il disco: Windows non dice di che \
                                tipo è."
                        .into();
                }
            }
        }
    }
    if let Some(s) = links.sata {
        let (text, slow) = info::sata_short(&s);
        if w.usb_device.is_some() && bridge.is_none() {
            lines.push(format!("nel contenitore: {text}"));
        } else {
            n.warn |= slow;
            let generation = if s.generation > 0 { s.generation } else { s.max_gen };
            let label = text.split(" (").next().unwrap_or_default().to_string();
            n.bw = Some(sata_bw(generation));
            n.bw_tip = format!(
                "{}{label}: {} teorici al netto della codifica 8b/10b.{}",
                if bridge.is_some() { "Dentro il contenitore, " } else { "" },
                info::rate(sata_bw(generation)),
                if slow {
                    format!("\nIl disco arriverebbe a {}.", info::rate(sata_bw(s.max_gen)))
                } else {
                    String::new()
                }
            );
            if bridge.is_some() {
                n.edge = label;
            } else {
                lines.push(text);
            }
        }
    }
    if let Some(rpm) = links.rpm.filter(|r| *r > 0) {
        lines.push(format!("disco meccanico · {rpm} giri al minuto"));
    }
    // Porta SATA del controller: Windows la dà come «Bus Number N» del disco.
    if w.items.last().is_some_and(|it| it.node.kind == Kind::SataController) {
        if let Some(disk_dev) = chain.last() {
            if let Some(port) = disk_dev
                .location
                .strip_prefix("Bus Number ")
                .and_then(|s| s.split(',').next())
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                n.edge = format!("porta {port}");
                n.order = format!("p{port:03}");
                n.link_ids = vec![
                    ("Porta SATA".to_string(), port.to_string()),
                    ("Posizione".to_string(), disk_dev.location.clone()),
                ];
            }
        }
    }
    if virtual_bus {
        n.order = format!("v{:03}", d.number);
        n.bw_tip = "Disco virtuale: i dati stanno in un file o in un altro disco, e la velocità dipende da lì.".into();
    }
    if n.bw_tip.is_empty() && !virtual_bus {
        n.bw_tip = "Windows non dichiara la velocità di questo collegamento.".into();
    }
    n.lines = lines;
    n.tip = format!("{}{port_tip}", chain.last().map(|x| x.name.as_str()).unwrap_or_default());
    n.ids = vec![
        ("Disco di Windows".to_string(), d.device_path()),
        ("ID istanza del disco".to_string(), d.instance_id.clone()),
    ];
    if let Some(disk_dev) = chain.last().filter(|x| !x.location.is_empty()) {
        n.ids.push(("Posizione del disco".to_string(), disk_dev.location.clone()));
    }
    if !d.serial.is_empty() {
        n.ids.push(("Numero di serie".to_string(), d.serial.clone()));
    }
    n.ids.extend(more_ids);
    n.disk = Some(d.number);
    n.letters = d.letters.clone();
    w.items.extend(bridge);
    w.items.push(leaf);
    (w.items, nvme_id)
}

/// Riquadro di un dispositivo PCIe che non porta a dischi: scheda grafica, rete, audio, controller
/// senza dischi. `None` per le funzioni interne che non interessano (ponti del processore,
/// strumentazione, sicurezza).
fn device_path(chain: &[Dev], cpu: &str, gpu_audio: &HashSet<(u32, u32)>) -> Option<Vec<Item>> {
    let dev = chain.last()?;
    if !chain.first().is_some_and(is_root) {
        return None;
    }
    let (bus, slot, _) = pci_numbers(&dev.location)?;
    let mut w = walk(chain, chain.len() - 1, cpu, false);
    if is_bridge(dev) {
        // Porta senza niente dietro (le porte interne non contano).
        let mut child = 0u32;
        if internal_bridge(dev) || unsafe { CM_Get_Child(&mut child, dev.node, 0) } == 0 {
            return None;
        }
        let addr = pci_address(&dev.location).unwrap_or_default();
        let mut it = item(
            format!("pci:{}", dev.id),
            Kind::Port,
            Icon::Port,
            "Porta PCIe libera".into(),
            vec![format!("{addr} · niente collegato")],
            dev.name.clone(),
        );
        it.node.ids = dev_ids(dev, "");
        it.node.bw_tip = "Porta PCIe senza dispositivi: uno slot o un connettore M.2 libero, o spento dal BIOS.".into();
        it.node.order = addr;
        w.items.push(it);
        return Some(w.items);
    }
    // Le funzioni integrate nel complesso radice (bus 0) non passano da una porta.
    if bus == 0 {
        return None;
    }
    let (base, sub) = pci_class(dev.node).unwrap_or((0xFF, 0));
    if matches!(base, 0x06 | 0x08 | 0x10 | 0x11 | 0x13) {
        return None;
    }
    // L'audio HDMI della scheda grafica è una funzione della scheda stessa.
    if base == 0x04 && gpu_audio.contains(&(bus, slot)) {
        return None;
    }
    if matches!(base, 0x01 | 0x0C) && !(base == 0x0C && sub != 0x03) {
        w.items.push(controller_item(dev, &mut w.port, w.uplink, false));
        return Some(w.items);
    }
    let (v, devid) = ids(&dev.id).unwrap_or_default();
    let maker = vendor(v).map(|m| format!("{m} · ")).unwrap_or_default();
    let lower = dev.name.to_ascii_lowercase();
    let (icon, what, role) = match base {
        0x03 => (Icon::Gpu, "La scheda", "scheda grafica"),
        0x02 if lower.contains("wi-fi") || lower.contains("wireless") || lower.contains("wlan") => {
            (Icon::Network, "La scheda", "rete senza fili")
        }
        0x02 => (Icon::Network, "La scheda", "scheda di rete"),
        0x04 => (Icon::Audio, "Il dispositivo", "audio"),
        _ => (Icon::Controller, "Il dispositivo", ""),
    };
    let via = w.port.take();
    let link = info::pcie_link_node(dev.node);
    let internal = via.as_ref().is_some_and(|p| p.internal);
    let (edge, bw, bw_tip, warn) = pci_edge(link, internal, w.uplink, what);
    let at = pci_address(&dev.location).unwrap_or_default();
    let mut first = role.to_string();
    if base == 0x03 && gpu_audio.contains(&(bus, slot)) {
        first.push_str(" · con audio HDMI/DP");
    }
    if internal {
        first = format!("{first} · dentro il processore").trim_start_matches(" · ").to_string();
    }
    let mut lines = Vec::new();
    if !first.is_empty() {
        lines.push(first);
    }
    lines.push(format!("{maker}{v:04X}:{devid:04X} · {at}"));
    if let Some(l) = link.filter(|l| pcie_slow(l) && !internal) {
        lines.push(format!(
            "{}, arriverebbe a {}",
            info::pcie_label(l.generation, l.width),
            info::pcie_label(l.max_gen, l.max_width)
        ));
    }
    let mut it = item(
        format!("pci:{}", dev.id),
        Kind::Device,
        icon,
        dev.name.clone(),
        lines,
        format!(
            "{}{}",
            dev.name,
            via.as_ref().map(|p| format!("\ndietro la porta PCIe {}", p.addr)).unwrap_or_default(),
        ),
    );
    it.node.edge = edge;
    it.node.bw = bw;
    it.node.bw_tip = bw_tip;
    it.node.warn = warn;
    it.node.ids = dev_ids(dev, "");
    it.node.link_ids = via.as_ref().map(|p| p.ids.clone()).unwrap_or_default();
    it.node.order = via.map(|p| p.addr).unwrap_or(at);
    w.items.push(it);
    Some(w.items)
}

// ---------------------------------------------------------------------------------------------
// Volumi su più dischi, contenitori, volumi senza disco
// ---------------------------------------------------------------------------------------------

/// Come un volume usa i suoi dischi.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Striped,
    Spanned,
    Mirror,
    Unknown,
}

/// Si chiede al volume dove stanno i suoi primi byte: in mirroring ogni byte sta su due dischi, a
/// strisce i blocchi da 64 KB si alternano fra i dischi, esteso i primi blocchi stanno tutti sul primo.
fn volume_layout(letter: char) -> Layout {
    const IOCTL_VOLUME_LOGICAL_TO_PHYSICAL: u32 = 0x0056_0020;
    let Ok(h) = win::open_device_query(&format!(r"\\.\{letter}:")) else { return Layout::Unknown };
    let map = |off: u64| -> Option<(u32, u32)> {
        let mut out = [0u8; 256];
        win::ioctl(&h, IOCTL_VOLUME_LOGICAL_TO_PHYSICAL, &off.to_le_bytes(), &mut out).ok()?;
        Some((rd_u32(&out, 0), rd_u32(&out, 8)))
    };
    let Some((count, first)) = map(0) else { return Layout::Unknown };
    if count > 1 {
        return Layout::Mirror;
    }
    for k in 1..=16u64 {
        if map(k * 65536).is_some_and(|(_, d)| d != first) {
            return Layout::Striped;
        }
    }
    Layout::Spanned
}

/// Contenitori cifrati montati da VeraCrypt (o TrueCrypt): lettera → file o partizione che li contiene.
fn mounted_containers() -> HashMap<char, String> {
    const GET_MOUNTED_VOLUMES: u32 = 0x0022_2018;
    let mut out = HashMap::new();
    for device in [r"\\.\VeraCrypt", r"\\.\TrueCrypt"] {
        let Ok(h) = win::open_device_query(device) else { continue };
        // MOUNT_LIST_STRUCT: maschera delle lettere, poi 26 percorsi da 260 caratteri.
        let mut buf = vec![0u8; 65536];
        let Ok(n) = win::ioctl_inout(&h, GET_MOUNTED_VOLUMES, &mut buf) else { continue };
        if n < 4 + 26 * 520 {
            continue;
        }
        let mask = rd_u32(&buf, 0);
        for i in 0..26usize {
            if mask & (1 << i) == 0 {
                continue;
            }
            let at = 4 + i * 520;
            let wide: Vec<u16> = buf[at..at + 520].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let path = win::from_wide(&wide);
            if !path.is_empty() {
                out.entry((b'A' + i as u8) as char).or_insert(path);
            }
        }
    }
    out
}

/// Dove sta un contenitore: «\??\S:\cartella\file.hc» → volume S:, «\Device\Harddisk3\Partition1» →
/// disco 3. Restituisce anche il percorso da mostrare.
fn container_host(path: &str) -> (Option<char>, Option<u32>, String) {
    let shown = path.trim_start_matches(r"\??\").trim_start_matches(r"\\?\").to_string();
    let b = shown.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return (Some((b[0] as char).to_ascii_uppercase()), None, shown);
    }
    let disk = shown
        .strip_prefix(r"\Device\Harddisk")
        .and_then(|s| s.split('\\').next())
        .and_then(|s| s.parse().ok());
    (None, disk, shown)
}

/// Nome Windows del dispositivo dietro una lettera: «\Device\VeraCryptVolumeK».
fn dos_device(letter: char) -> String {
    let name = win::wide(&format!("{letter}:"));
    let mut buf = [0u16; 512];
    let n = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 { String::new() } else { win::from_wide(&buf) }
}

/// Identificativi di un volume: il suo nome per Windows, che non dipende dalla lettera, e il
/// dispositivo dietro la lettera.
fn volume_ids(letter: char) -> Ids {
    let root = win::wide(&format!("{letter}:\\"));
    let mut guid = [0u16; 64];
    let mut v = Vec::new();
    if unsafe { GetVolumeNameForVolumeMountPointW(root.as_ptr(), guid.as_mut_ptr(), guid.len() as u32) } != 0 {
        v.push(("Volume".to_string(), win::from_wide(&guid)));
    }
    let device = dos_device(letter);
    if !device.is_empty() {
        v.push(("Dispositivo".to_string(), device));
    }
    v
}

/// Nomi dei driver caricati, senza estensione.
fn loaded_drivers() -> Vec<String> {
    let mut bases = vec![null_mut::<c_void>(); 4096];
    let mut needed = 0u32;
    let size = std::mem::size_of::<*mut c_void>();
    if unsafe { EnumDeviceDrivers(bases.as_mut_ptr(), (bases.len() * size) as u32, &mut needed) } == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for &base in &bases[..(needed as usize / size).min(bases.len())] {
        let mut name = [0u16; 260];
        if unsafe { GetDeviceDriverBaseNameW(base, name.as_mut_ptr(), name.len() as u32) } == 0 {
            continue;
        }
        let name = win::from_wide(&name);
        let name = match name.rfind('.') {
            Some(dot) => name[..dot].to_string(),
            None => name,
        };
        if !name.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&name)) {
            out.push(name);
        }
    }
    out
}

/// I driver che stanno sul percorso delle richieste per il volume: Windows risponde se un driver
/// dato è sul percorso, e lo si chiede per ogni driver caricato. Vero = driver di file system.
fn drivers_in_path(letter: char, candidates: &[String]) -> Vec<(String, bool)> {
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let Ok(h) = win::open(&format!("{letter}:\\"), 0, win::SHARE_ALL, win::OPEN_EXISTING, BACKUP_SEMANTICS) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in candidates {
        for (prefix, fs) in [(r"\Driver\", false), (r"\FileSystem\", true)] {
            let full: Vec<u16> = format!("{prefix}{name}").encode_utf16().collect();
            // FILE_FS_DRIVER_PATH_INFORMATION: risposta, lunghezza del nome in byte, nome.
            let mut buf = vec![0u8; 8 + full.len() * 2 + 8];
            buf[4..8].copy_from_slice(&((full.len() * 2) as u32).to_le_bytes());
            for (i, c) in full.iter().enumerate() {
                buf[8 + i * 2..10 + i * 2].copy_from_slice(&c.to_le_bytes());
            }
            let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
            let status = unsafe {
                NtQueryVolumeInformationFile(
                    h.raw(),
                    &mut iosb,
                    buf.as_mut_ptr() as *mut c_void,
                    buf.len() as u32,
                    FileFsDriverPathInformation,
                )
            };
            if status == 0 && buf[0] != 0 {
                out.push((name.clone(), fs));
            }
        }
    }
    out
}

/// Driver presenti sotto quasi ogni volume: non dicono che cosa sia.
const GENERIC_DRIVERS: [&str; 16] = [
    "fltmgr", "volmgr", "volmgrx", "volume", "volsnap", "fvevol", "iorate", "partmgr", "disk", "mountmgr", "rdyboost",
    "wof", "fileinfo", "ambakdrv", "classpnp", "storport",
];
const STANDARD_FS: [&str; 7] = ["ntfs", "fastfat", "exfat", "refs", "udfs", "cdfs", "msfs"];

/// Nome comprensibile di qualche driver che crea volumi.
fn driver_title(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    Some(match n.as_str() {
        _ if n.starts_with("googledrivefs") => "Google Drive",
        "veracrypt" => "VeraCrypt",
        "truecrypt" => "TrueCrypt",
        _ if n.starts_with("imdisk") => "ImDisk",
        _ if n.starts_with("winfsp") => "WinFsp",
        _ if n.starts_with("dokan") => "Dokan",
        _ if n.starts_with("cbfs") => "Callback File System",
        _ if n.starts_with("osfmount") => "OSFMount",
        _ if n.starts_with("aimwrfltr") || n.starts_with("phdskmnt") => "Arsenal Image Mounter",
        _ if n.starts_with("pcloud") => "pCloud",
        "vhdmp" => "disco virtuale di Windows",
        "mrxsmb" | "mrxsmb20" | "rdbss" => "condivisione di rete",
        "ramdisk" | "ramdriv" => "disco RAM",
        _ => return None,
    })
}

/// Che cosa c'è dietro un volume senza disco fisico.
struct Loose {
    device: String,
    /// Driver che lo creano (non i generici), con «vero» per i file system.
    drivers: Vec<(String, bool)>,
    fs_driver: Option<String>,
    container: Option<String>,
}

impl Loose {
    fn maker(&self) -> Option<String> {
        if self.container.is_some() && self.device.to_ascii_lowercase().contains("truecrypt") {
            return Some("TrueCrypt".into());
        }
        if self.container.is_some() {
            return Some("VeraCrypt".into());
        }
        self.drivers.iter().find_map(|(d, _)| driver_title(d).map(str::to_string))
    }

    fn icon(&self) -> Icon {
        match self.maker().as_deref() {
            Some("Google Drive") | Some("pCloud") => Icon::Cloud,
            Some("VeraCrypt") | Some("TrueCrypt") => Icon::Locked,
            Some("condivisione di rete") => Icon::Network,
            _ => Icon::Volume,
        }
    }

    /// «driver veracrypt · file system Ntfs».
    fn driver_line(&self) -> String {
        let names: Vec<&str> = self.drivers.iter().map(|(d, _)| d.as_str()).collect();
        let mut parts = Vec::new();
        if !names.is_empty() {
            parts.push(format!("driver {}", names.join(", ")));
        }
        if let Some(fs) = &self.fs_driver {
            parts.push(format!("file system {fs}"));
        }
        if parts.is_empty() { "driver non trovato".into() } else { parts.join(" · ") }
    }
}

fn probe_loose(v: &Volume, drivers: &[String], containers: &HashMap<char, String>) -> Loose {
    let found = drivers_in_path(v.letter, drivers);
    let generic = |d: &str| GENERIC_DRIVERS.iter().any(|g| g.eq_ignore_ascii_case(d));
    let standard = |d: &str| STANDARD_FS.iter().any(|g| g.eq_ignore_ascii_case(d));
    Loose {
        device: dos_device(v.letter),
        fs_driver: found.iter().find(|(d, fs)| *fs && standard(d)).map(|(d, _)| d.clone()),
        drivers: found.iter().filter(|(d, _)| !generic(d) && !standard(d)).cloned().collect(),
        container: containers.get(&v.letter).cloned(),
    }
}

fn volume_title(v: &Volume) -> String {
    format!("{}: {}", v.letter, if v.label.is_empty() { "Senza nome" } else { &v.label })
}

fn volume_size_line(v: &Volume) -> String {
    format!("{} · {}", if v.fs.is_empty() { "?" } else { &v.fs }, human_bytes(v.total))
}

// ---------------------------------------------------------------------------------------------
// Costruzione
// ---------------------------------------------------------------------------------------------

/// Costruisce il grafo: tutti i dischi, i dispositivi PCIe, i volumi su più dischi, i contenitori e i
/// volumi senza un disco fisico.
pub fn build(inv: &Inventory) -> Graph {
    let cpu = cpu_name();
    let mut g = Graph::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    let add = |g: &mut Graph, index: &mut HashMap<String, usize>, parent: Option<usize>, it: Item| -> usize {
        if let Some(&n) = index.get(&it.key) {
            return n;
        }
        let n = g.nodes.len();
        g.nodes.push(it.node);
        index.insert(it.key, n);
        match parent {
            Some(p) => g.nodes[p].children.push(n),
            None => g.roots.push(n),
        }
        n
    };

    // I dischi, ciascuno con il suo percorso.
    let mut merged: HashSet<String> = HashSet::new();
    for d in &inv.disks {
        let (items, nvme) = path_of(d, &cpu);
        merged.extend(nvme);
        let mut parent = None;
        for it in items {
            parent = Some(add(&mut g, &mut index, parent, it));
        }
    }

    // Gli altri dispositivi PCIe: scheda grafica, rete, audio, controller senza dischi, porte libere.
    let chains: Vec<Vec<Dev>> = pci_devices().into_iter().map(chain_of).filter(|c| !c.is_empty()).collect();
    let mut gpus: HashSet<(u32, u32)> = HashSet::new();
    let mut audio: HashSet<(u32, u32)> = HashSet::new();
    for c in &chains {
        let dev = c.last().unwrap();
        if let (Some((bus, slot, _)), Some((base, _))) = (pci_numbers(&dev.location), pci_class(dev.node)) {
            match base {
                0x03 => {
                    gpus.insert((bus, slot));
                }
                0x04 => {
                    audio.insert((bus, slot));
                }
                _ => {}
            }
        }
    }
    let gpu_audio: HashSet<(u32, u32)> = gpus.intersection(&audio).copied().collect();
    for c in &chains {
        let dev = c.last().unwrap();
        if index.contains_key(&format!("pci:{}", dev.id)) || merged.contains(&dev.id) {
            continue;
        }
        let Some(items) = device_path(c, &cpu, &gpu_audio) else { continue };
        let mut parent = None;
        for it in items {
            parent = Some(add(&mut g, &mut index, parent, it));
        }
    }

    // Volumi su più dischi, a destra dei loro dischi.
    for v in inv.volumes.iter().filter(|v| v.disks.len() > 1) {
        let hosts: Vec<usize> = v.disks.iter().filter_map(|n| index.get(&format!("disk:{n}")).copied()).collect();
        if hosts.is_empty() {
            continue;
        }
        let (what, how) = match volume_layout(v.letter) {
            Layout::Striped => (
                "a strisce (RAID 0)",
                "Windows divide i dati a blocchi da 64 KB alternando i dischi: letture e scritture lunghe usano \
                 tutti i dischi insieme, e la banda teorica è la somma delle loro.",
            ),
            Layout::Spanned => (
                "esteso",
                "Windows riempie un disco dopo l'altro: la velocità è quella del disco su cui stanno i dati.",
            ),
            Layout::Mirror => (
                "in mirroring (RAID 1)",
                "Windows scrive gli stessi dati su ogni disco: le letture possono usarli insieme, le scritture \
                 vanno alla velocità del più lento.",
            ),
            Layout::Unknown => ("distribuito", "Windows non dice come divide i dati fra i dischi."),
        };
        let disks_list = v.disks.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
        let mut it = item(
            format!("vol:{}", v.letter),
            Kind::Span,
            Icon::Raid,
            volume_title(v),
            vec![format!("{what} su {} dischi", v.disks.len()), volume_size_line(v)],
            format!("Volume {}: {what} sui dischi {disks_list}", v.letter),
        );
        it.node.link = Link::Soft;
        let sum: f64 = hosts.iter().filter_map(|&h| g.nodes[h].bw).sum();
        it.node.bw_tip = format!(
            "Il volume {}: è {what} sui dischi {disks_list}. {how}{}",
            v.letter,
            if sum > 0.0 && what.starts_with("a strisce") {
                format!("\nSomma delle bande teoriche dei collegamenti dei dischi: {}.", info::rate(sum))
            } else {
                String::new()
            }
        );
        it.node.letters = vec![v.letter];
        it.node.ids = volume_ids(v.letter);
        it.node.hosts = hosts;
        let n = g.nodes.len();
        index.insert(it.key, n);
        g.nodes.push(it.node);
        g.side.push(n);
    }

    // Volumi senza disco fisico: contenitori cifrati (collegati a ciò che li ospita) e gli altri, con
    // il driver che li crea.
    let loose: Vec<&Volume> = inv.volumes.iter().filter(|v| v.disks.is_empty()).collect();
    if !loose.is_empty() {
        let drivers = loaded_drivers();
        let containers = mounted_containers();
        let mut pending: Vec<(usize, Option<char>, Option<u32>)> = Vec::new();
        for v in &loose {
            let probe = probe_loose(v, &drivers, &containers);
            let maker = probe.maker();
            let (host_letter, host_disk, shown) = probe
                .container
                .as_deref()
                .map(container_host)
                .unwrap_or((None, None, String::new()));
            let first = match (&maker, probe.container.is_some()) {
                (Some(m), true) => match (host_letter, host_disk) {
                    (Some(l), _) => format!("{m} · file su {l}:"),
                    (None, Some(d)) => format!("{m} · partizione del disco {d}"),
                    _ => format!("{m} · contenitore"),
                },
                (Some(m), false) => format!("{m} · {}", probe.driver_line()),
                (None, _) => probe.driver_line(),
            };
            let mut tip = format!("Volume {}: senza un disco fisico", v.letter);
            if let Some(m) = &maker {
                tip.push_str(&format!("\nCreato da: {m}"));
            }
            tip.push_str(&format!("\n{}", probe.driver_line()));
            let mut it = item(
                format!("vol:{}", v.letter),
                Kind::Volume,
                probe.icon(),
                volume_title(v),
                vec![first, volume_size_line(v)],
                tip,
            );
            it.node.link = Link::Soft;
            it.node.letters = vec![v.letter];
            it.node.ids = volume_ids(v.letter);
            for (d, fs) in &probe.drivers {
                it.node.ids.push((
                    if *fs { "Driver di file system".into() } else { "Driver".into() },
                    format!(r"{}\{d}", if *fs { r"\FileSystem" } else { r"\Driver" }),
                ));
            }
            if !shown.is_empty() {
                it.node.ids.push(("Contenitore".into(), shown.clone()));
                it.node.link_ids = vec![("File del contenitore".into(), shown.clone())];
            }
            it.node.bw_tip = if probe.container.is_some() {
                format!(
                    "{}: è un contenitore cifrato {} in {shown}: ogni lettura e scrittura passa dalla cifratura, \
                     fatta dal processore, e poi da ciò che lo ospita.",
                    v.letter,
                    maker.as_deref().unwrap_or("")
                )
            } else {
                String::new()
            };
            let n = g.nodes.len();
            index.insert(it.key, n);
            g.nodes.push(it.node);
            pending.push((n, host_letter, host_disk));
        }
        // Chi ospita i contenitori: un volume su più dischi, il disco con quella lettera, un altro
        // volume senza disco; il disco della partizione.
        let mut orphans = Vec::new();
        for (n, letter, disk) in pending {
            let host = letter
                .and_then(|l| index.get(&format!("vol:{l}")).copied().filter(|&h| h != n))
                .or_else(|| {
                    letter.and_then(|l| {
                        g.nodes.iter().position(|x| x.kind == Kind::Disk && x.letters.contains(&l))
                    })
                })
                .or_else(|| disk.and_then(|d| index.get(&format!("disk:{d}")).copied()));
            match host {
                Some(h) => {
                    g.nodes[n].kind = Kind::Container;
                    g.nodes[n].hosts = vec![h];
                    g.side.push(n);
                }
                None => orphans.push(n),
            }
        }
        if !orphans.is_empty() {
            let root = add(
                &mut g,
                &mut index,
                None,
                item(
                    "volumi".into(),
                    Kind::Virtual,
                    Icon::Group,
                    "Senza disco fisico".into(),
                    vec!["cifrati, rete, cloud…".into()],
                    "Volumi che Windows non collega a un disco: contenitori cifrati, unità di rete o di cloud, \
                     dischi virtuali di altri programmi. Per ciascuno il driver che lo crea."
                        .into(),
                ),
            );
            for n in orphans {
                g.nodes[root].children.push(n);
            }
        }
    }

    // Tutti in una colonna a destra dei dischi: chi è ospitato da un altro riquadro di quella colonna
    // (un contenitore su un volume su più dischi) gli sta sotto.
    for &s in &g.side.clone() {
        g.nodes[s].side_col = 1;
    }
    // Fratelli in ordine di porta; il processore prima dei gruppi.
    let orders: Vec<String> = g.nodes.iter().map(|n| n.order.clone()).collect();
    for node in &mut g.nodes {
        node.children.sort_by(|a, b| orders[*a].cmp(&orders[*b]));
    }
    g.roots.sort_by_key(|&r| (g.nodes[r].kind != Kind::Cpu, g.nodes[r].title.starts_with("Senza")));
    g
}

//! Elenco dei volumi e dei dischi fisici, senza bisogno di privilegi di amministratore.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

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

fn probe_disk(number: u32) -> Option<PhysDisk> {
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
    })
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

/// Per ogni disco trova il suo posto nell'albero dei dispositivi e, risalendolo, il bridge USB a
/// cui è collegato (VID e PID) e il dispositivo da scollegare per la rimozione sicura.
fn disk_nodes() -> Vec<DiskNode> {
    const DIGCF_PRESENT: u32 = 0x2;
    const DIGCF_DEVICEINTERFACE: u32 = 0x10;
    let mut out = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if set as isize == -1 || set as isize == 0 {
            return out;
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
            let path = win::from_wide(std::slice::from_raw_parts(path_ptr, chars));

            let Ok(h) = win::open_device_query(&path) else { continue };
            let mut dn = [0u8; 12];
            if win::ioctl(&h, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut dn).is_err() {
                continue;
            }
            let mut node = DiskNode {
                number: rd_u32(&dn, 4),
                instance_id: device_id(info.DevInst).unwrap_or_default(),
                ..Default::default()
            };
            let mut at = info.DevInst;
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
            out.push(node);
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// Fotografia di volumi e dischi presenti in questo momento.
pub fn scan() -> Inventory {
    let mut inv = Inventory::default();
    for n in 0..64u32 {
        if let Some(d) = probe_disk(n) {
            inv.disks.push(d);
        }
    }
    for node in disk_nodes() {
        if let Some(d) = inv.disks.iter_mut().find(|d| d.number == node.number) {
            d.usb_id = node.usb_id;
            d.instance_id = node.instance_id;
            d.usb_node = node.usb_node;
            d.eject_node = node.eject_node;
        }
    }
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        if let Some(mut v) = probe_volume(letter) {
            // Le unità virtuali (cloud, dischi RAM) a volte dichiarano di stare sul disco 0:
            // si accetta il disco solo se il volume risponde con lo stesso modello e bus.
            if v.disks.len() == 1 {
                let same = match (volume_descriptor(letter), inv.disk(v.disks[0])) {
                    (Some((model, serial, bus)), Some(d)) => model == d.model && serial == d.serial && bus == d.bus,
                    _ => false,
                };
                if !same {
                    v.disks.clear();
                }
            }
            for n in &v.disks {
                if let Some(d) = inv.disks.iter_mut().find(|d| d.number == *n) {
                    d.letters.push(letter);
                }
            }
            inv.volumes.push(v);
        }
    }
    inv
}

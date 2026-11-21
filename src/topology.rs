//! Grafo Storage: come sono collegati i dischi. Per ogni disco si risale l'albero dei dispositivi di
//! Windows fino al processore, si tengono i passaggi che dicono qualcosa (processore, chipset,
//! controller, hub USB) e si uniscono le catene in un albero unico, con i dischi alla fine. Le porte
//! PCIe, le porte degli hub e quelle SATA diventano le etichette dei collegamenti.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::null_mut;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_IDW, CM_Get_Parent, CM_Locate_DevNodeW,
};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

use crate::disks::{Inventory, PhysDisk, bus_name};
use crate::info;
use crate::win::{self, human_bytes};

const CM_DRP_DEVICEDESC: u32 = 0x01;
const CM_DRP_SERVICE: u32 = 0x05;
const CM_DRP_CLASS: u32 = 0x08;
const CM_DRP_FRIENDLYNAME: u32 = 0x0D;
const CM_DRP_LOCATION_INFORMATION: u32 = 0x0E;
const CM_DRP_ADDRESS: u32 = 0x1D;

/// Che cosa è un riquadro del grafo: decide colore e forma.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Cpu,
    Chipset,
    SataController,
    UsbController,
    Controller,
    Hub,
    Virtual,
    Disk,
    /// Volume senza un disco fisico (cifrato, di rete, di cloud…).
    Volume,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub title: String,
    pub lines: Vec<String>,
    /// Testo del fumetto: nomi completi e identificativi di Windows.
    pub tip: String,
    /// Etichetta del collegamento con il riquadro a sinistra: porta, velocità.
    pub edge: String,
    /// Il collegamento va più piano di quanto il dispositivo potrebbe.
    pub warn: bool,
    pub disk: Option<u32>,
    pub letter: Option<char>,
    pub children: Vec<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
}

/// Un dispositivo della catena, con le proprietà che servono per riconoscerlo.
struct Dev {
    id: String,
    service: String,
    class: String,
    name: String,
    location: String,
    address: Option<u32>,
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

/// La catena di un dispositivo, da lui fino alla radice dell'albero (esclusa).
fn ancestors(instance_id: &str) -> Vec<Dev> {
    let mut out = Vec::new();
    let id = win::wide(instance_id);
    let mut node = 0u32;
    if instance_id.is_empty() || unsafe { CM_Locate_DevNodeW(&mut node, id.as_ptr(), 0) } != 0 {
        return out;
    }
    for _ in 0..24 {
        let Some(id) = device_id(node) else { break };
        let up = id.to_ascii_uppercase();
        if up.starts_with("HTREE\\ROOT") || up.starts_with("ROOT\\ACPI_HAL") || up.starts_with("ACPI_HAL\\") {
            break;
        }
        let friendly = dev_string(node, CM_DRP_FRIENDLYNAME);
        out.push(Dev {
            name: if friendly.is_empty() { dev_string(node, CM_DRP_DEVICEDESC) } else { friendly },
            service: dev_string(node, CM_DRP_SERVICE).to_ascii_lowercase(),
            class: dev_string(node, CM_DRP_CLASS).to_ascii_lowercase(),
            location: dev_string(node, CM_DRP_LOCATION_INFORMATION),
            address: dev_u32(node, CM_DRP_ADDRESS),
            id,
        });
        let mut parent = 0u32;
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != 0 {
            break;
        }
        node = parent;
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
        0x05E3 => "Genesys Logic",
        0x2109 => "VIA Labs",
        0x0BDA => "Realtek",
        0x174C => "ASMedia",
        0x0BC2 => "Seagate",
        0x1058 => "Western Digital",
        0x0781 => "SanDisk",
        0x04E8 => "Samsung",
        0x0951 => "Kingston",
        0x1908 => "GEMBIRD",
        0x067B => "Prolific",
        0x04B4 => "Cypress",
        _ => return None,
    })
}

/// «Bus PCI 0, dispositivo 3, funzione 2» → «00:03.2».
fn pci_address(location: &str) -> Option<String> {
    let numbers: Vec<u32> = location
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    (numbers.len() == 3).then(|| format!("{:02x}:{:02x}.{}", numbers[0], numbers[1], numbers[2]))
}

fn is_bridge(d: &Dev) -> bool {
    d.id.to_ascii_uppercase().starts_with("PCI\\") && d.service == "pci"
}

/// Un riquadro del percorso di un disco, prima di finire nell'albero.
struct Item {
    key: String,
    node: Node,
}

fn item(key: String, kind: Kind, title: String, lines: Vec<String>, tip: String, edge: String) -> Item {
    Item {
        key,
        node: Node {
            kind,
            title,
            lines,
            tip,
            edge,
            warn: false,
            disk: None,
            letter: None,
            children: Vec::new(),
        },
    }
}

/// Il percorso di un disco, dalla radice a lui: solo i riquadri che dicono qualcosa.
fn path_of(d: &PhysDisk, cpu: &str) -> Vec<Item> {
    let mut chain = ancestors(&d.instance_id);
    chain.reverse();
    let mut path: Vec<Item> = Vec::new();
    // Porta PCIe attraversata e non ancora attribuita a un riquadro.
    let mut port: Option<String> = None;
    let mut usb_device: Option<(&Dev, Option<(u16, u16)>)> = None;
    let mut nvme = false;
    let last = chain.len().saturating_sub(1);
    for (i, dev) in chain.iter().enumerate() {
        let up = dev.id.to_ascii_uppercase();
        if i == last {
            break;
        }
        if up.starts_with("ACPI\\PNP0A08") || up.starts_with("ACPI\\PNP0A03") {
            path.push(item(
                format!("cpu:{}", dev.id),
                Kind::Cpu,
                cpu.to_string(),
                vec!["processore · radice PCIe".into()],
                format!("{cpu}\n{}\n{}", dev.name, dev.id),
                String::new(),
            ));
        } else if is_bridge(dev) {
            let parent_bridge = i > 0 && is_bridge(&chain[i - 1]);
            let next_bridge = chain.get(i + 1).is_some_and(is_bridge);
            if parent_bridge && next_bridge {
                // Porta a monte di uno switch PCIe: sulle schede AMD è il chipset.
                let (v, devid) = ids(&dev.id).unwrap_or_default();
                let title = match (v, devid) {
                    (0x1022, 0x57AD) => "Chipset AMD X570".to_string(),
                    (0x1022, _) => "Chipset AMD".to_string(),
                    (v, _) => format!("Switch PCIe {}", vendor(v).unwrap_or("")).trim().to_string(),
                };
                let via = port.take().filter(|p| !p.is_empty());
                path.push(item(
                    format!("pci:{}", dev.id),
                    Kind::Chipset,
                    title,
                    vec![format!("switch PCIe · {v:04X}:{devid:04X}")],
                    format!(
                        "{}\n{}\n{}{}",
                        dev.name,
                        dev.location,
                        dev.id,
                        via.map(|p| format!("\ncollegato alla porta PCIe {p} del processore")).unwrap_or_default()
                    ),
                    String::new(),
                ));
            } else {
                port = pci_address(&dev.location).or(Some(String::new()));
            }
        } else if up.starts_with("PCI\\") {
            let (v, devid) = ids(&dev.id).unwrap_or_default();
            let maker = vendor(v).map(|m| format!("{m} · ")).unwrap_or_default();
            let via = port.take().filter(|p| !p.is_empty());
            let tip = format!(
                "{}\n{}{}\n{}",
                dev.name,
                dev.location,
                via.as_ref().map(|p| format!(" · dietro la porta PCIe {p}")).unwrap_or_default(),
                dev.id
            );
            if dev.service == "stornvme" || (d.bus == crate::disks::BUS_NVME && chain.get(i + 1).is_some_and(|n| n.class == "diskdrive")) {
                // Il controller NVMe è il disco stesso: il suo collegamento va sul disco.
                nvme = true;
                port = via;
                continue;
            }
            let (kind, title) = if dev.service.starts_with("usb") || dev.class == "usb" {
                let name = match dev.service.as_str() {
                    "usbxhci" => "Controller USB 3 (xHCI)",
                    "usbehci" => "Controller USB 2 (EHCI)",
                    _ => "Controller USB",
                };
                (Kind::UsbController, name.to_string())
            } else if dev.service.contains("ahci") || dev.class == "hdc" {
                (Kind::SataController, "Controller SATA (AHCI)".to_string())
            } else {
                (Kind::Controller, dev.name.clone())
            };
            // La porta PCIe del controller va nel fumetto: per quelli dentro il chipset è un dettaglio
            // interno, che sul collegamento sarebbe solo rumore. L'indirizzo PCI distingue due
            // controller uguali.
            let at = pci_address(&dev.location).map(|a| format!(" · {a}")).unwrap_or_default();
            path.push(item(
                format!("pci:{}", dev.id),
                kind,
                title,
                vec![format!("{maker}{v:04X}:{devid:04X}{at}")],
                tip,
                String::new(),
            ));
        } else if up.starts_with("USB\\ROOT_HUB") {
            // L'hub radice fa parte del controller.
        } else if up.starts_with("USB\\") {
            if dev.service.starts_with("usbhub") {
                let usb3 = dev.name.contains("SuperSpeed") || dev.name.contains("USB 3");
                let (v, p) = ids(&dev.id).unwrap_or_default();
                // Prima l'identificativo, che distingue gli hub; il produttore, se non ci sta, si taglia.
                let maker = vendor(v).map(|m| format!(" · {m}")).unwrap_or_default();
                path.push(item(
                    format!("usb:{}", dev.id),
                    Kind::Hub,
                    if usb3 { "Hub USB 3".into() } else { "Hub USB 2".into() },
                    vec![format!("{v:04X}:{p:04X}{maker}")],
                    format!("{}\n{}\n{}", dev.name, dev.location, dev.id),
                    dev.address.map(|a| format!("porta {a}")).unwrap_or_default(),
                ));
            } else if !up.contains("&MI_") && usb_device.is_none() {
                // Il dispositivo USB nel contenitore (bridge o chiavetta): va sul disco.
                usb_device = Some((dev, ids(&dev.id)));
            }
        } else if path.is_empty() {
            // Catene che non partono dal processore: dischi virtuali creati dal software.
            path.push(item(
                "virtuali".into(),
                Kind::Virtual,
                "Dischi virtuali".into(),
                vec!["creati da Windows o da altri programmi".into()],
                format!("{}\n{}", dev.name, dev.id),
                String::new(),
            ));
        }
    }

    // Un disco di cui Windows non dà la catena (succede ad alcuni dischi virtuali) sta comunque sotto
    // un riquadro, invece di restare isolato.
    if path.is_empty() {
        let virtual_bus = matches!(d.bus, 14 | 15 | 16);
        path.push(item(
            if virtual_bus { "virtuali".into() } else { "sconosciuto".into() },
            Kind::Virtual,
            if virtual_bus { "Dischi virtuali".into() } else { "Collegamento non trovato".into() },
            vec![if virtual_bus {
                "creati da Windows o da altri programmi".into()
            } else {
                "Windows non dice a che cosa è collegato".into()
            }],
            String::new(),
            String::new(),
        ));
    }

    // Il disco, alla fine.
    let (pcie, usb, sata) = info::links(d);
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
    let mut warn = false;
    let mut edge = String::new();
    if let Some(p) = pcie.filter(|_| nvme) {
        let (text, slow) = info::pcie_short(&p);
        warn |= slow;
        edge = text.split(" (").next().unwrap_or_default().to_string();
        lines.push(format!("NVMe su {text}"));
    }
    if let Some((dev, vid)) = usb_device {
        let proto = match dev.service.as_str() {
            "uaspstor" => " · UAS",
            "usbstor" => " · BOT",
            _ => "",
        };
        let bridge = vid
            .map(|(v, p)| format!(" · {}{v:04X}:{p:04X}", vendor(v).map(|m| format!("{m} ")).unwrap_or_default()))
            .unwrap_or_default();
        match usb {
            Some(u) => {
                let (text, slow) = info::usb_short(&u);
                warn |= slow;
                lines.push(format!("{text}{proto}{bridge}"));
            }
            None => lines.push(format!("USB{proto}{bridge}")),
        }
        edge = dev.address.map(|a| format!("porta {a}")).unwrap_or_default();
    }
    if let Some(s) = sata {
        let (text, slow) = info::sata_short(&s);
        warn |= slow;
        lines.push(if usb_device.is_some() { format!("nel contenitore: {text}") } else { text });
    }
    // Porta SATA del controller: Windows la dà come «Bus Number N» del disco.
    if path.last().is_some_and(|it| it.node.kind == Kind::SataController) {
        if let Some(disk_dev) = chain.last() {
            if let Some(n) = disk_dev
                .location
                .strip_prefix("Bus Number ")
                .and_then(|s| s.split(',').next())
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                edge = format!("porta {n}");
            }
        }
    }
    let disk_dev = chain.last();
    let mut leaf = item(
        format!("disk:{}", d.number),
        Kind::Disk,
        format!("Disco {} · {}", d.number, d.title()),
        lines,
        format!(
            "{}\n{}{}",
            disk_dev.map(|x| x.name.as_str()).unwrap_or_default(),
            d.instance_id,
            port.map(|p| format!("\nporta PCIe {p}")).unwrap_or_default()
        ),
        edge,
    );
    leaf.node.warn = warn;
    leaf.node.disk = Some(d.number);
    leaf.node.letter = d.letters.first().copied();
    path.push(leaf);
    path
}

/// Costruisce il grafo dell'inventario: tutti i dischi, e i volumi senza un disco fisico.
pub fn build(inv: &Inventory) -> Graph {
    let cpu = cpu_name();
    let mut g = Graph::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut add = |g: &mut Graph, parent: Option<usize>, it: Item| -> usize {
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
    for d in &inv.disks {
        let mut parent = None;
        for it in path_of(d, &cpu) {
            parent = Some(add(&mut g, parent, it));
        }
    }
    let loose: Vec<_> = inv.volumes.iter().filter(|v| v.disks.is_empty()).collect();
    if !loose.is_empty() {
        let root = add(
            &mut g,
            None,
            item(
                "volumi".into(),
                Kind::Virtual,
                "Senza disco fisico".into(),
                vec!["cifrati, rete, cloud…".into()],
                "Volumi che Windows non collega a un disco: contenitori cifrati, unità di rete o di cloud, \
                 dischi virtuali di altri programmi."
                    .into(),
                String::new(),
            ),
        );
        for v in loose {
            let mut it = item(
                format!("vol:{}", v.letter),
                Kind::Volume,
                format!("{}: {}", v.letter, if v.label.is_empty() { "Senza nome" } else { &v.label }),
                vec![format!("{} · {}", if v.fs.is_empty() { "?" } else { &v.fs }, human_bytes(v.total))],
                format!("Volume {}:", v.letter),
                String::new(),
            );
            it.node.letter = Some(v.letter);
            add(&mut g, Some(root), it);
        }
    }
    // Il processore prima dei dischi virtuali e dei volumi senza disco.
    g.roots.sort_by_key(|&r| g.nodes[r].kind != Kind::Cpu);
    g
}

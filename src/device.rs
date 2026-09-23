use crate::{
    error::{Error, Result},
    protocol::{FeatureIo, Protocol},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
compile_error!("hidraw ioctl encoding is currently supported only on Linux x86_64/aarch64");

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub path: PathBuf,
    pub selector: String,
    pub name: String,
    pub bus: u32,
    pub vendor_id: String,
    pub product_id: String,
    pub physical_path: PathBuf,
    pub support: &'static str,
    #[serde(skip)]
    sys_device: PathBuf,
    #[serde(skip)]
    devnum: u64,
}

fn attribute<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        line.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
    })
}

fn hid_id(text: &str) -> Result<(u32, u32, u32)> {
    let parts = attribute(text, "HID_ID")
        .ok_or_else(|| Error::protocol("missing HID_ID in sysfs"))?
        .split(':')
        .map(|s| u32::from_str_radix(s, 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::protocol("invalid HID_ID in sysfs"))?;
    if parts.len() != 3 {
        return Err(Error::protocol("invalid HID_ID in sysfs"));
    }
    Ok((parts[0], parts[1], parts[2]))
}

fn known_model(bus: u32, vendor: u32, product: u32, name: &str) -> bool {
    (bus, vendor, product) == (0x18, 0x04f3, 0x323b) && name.starts_with("ELAN2703:")
}

pub fn discover() -> Result<Vec<Device>> {
    discover_at(Path::new("/sys/class/hidraw"))
}

fn discover_at(root: &Path) -> Result<Vec<Device>> {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(root.display(), e)),
    };
    let mut devices = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::io("enumerate hidraw", e))?;
        let class_path = entry.path();
        let sys_device = fs::canonicalize(class_path.join("device"))
            .map_err(|e| Error::io("resolve HID sysfs device", e))?;
        let text = fs::read_to_string(sys_device.join("uevent"))
            .map_err(|e| Error::io("read HID identity", e))?;
        let (bus, vendor, product) = hid_id(&text)?;
        let name = attribute(&text, "HID_NAME").unwrap_or("").to_owned();
        // The HID instance suffix changes on rebind. Its physical parent does not.
        let physical_path = sys_device
            .parent()
            .ok_or_else(|| Error::protocol("HID device has no physical parent"))?
            .to_path_buf();
        let selector = format!(
            "sysfs:{}",
            Path::new("/")
                .join(physical_path.strip_prefix("/sys").unwrap_or(&physical_path))
                .display()
        );
        let dev = fs::read_to_string(class_path.join("dev"))
            .map_err(|e| Error::io("read hidraw device number", e))?;
        let (major, minor) = dev
            .trim()
            .split_once(':')
            .ok_or_else(|| Error::protocol("invalid sysfs device number"))?;
        let major = major
            .parse::<u32>()
            .map_err(|_| Error::protocol("invalid device major"))?;
        let minor = minor
            .parse::<u32>()
            .map_err(|_| Error::protocol("invalid device minor"))?;
        devices.push(Device {
            path: Path::new("/dev").join(entry.file_name()),
            selector,
            name: name.clone(),
            bus,
            vendor_id: format!("{vendor:04x}"),
            product_id: format!("{product:04x}"),
            physical_path,
            support: if known_model(bus, vendor, product, &name) {
                "identity_check_required"
            } else {
                "unsupported"
            },
            sys_device,
            devnum: libc::makedev(major, minor),
        });
    }
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(devices)
}

pub struct Hidraw {
    file: File,
    // An exclusive advisory lock on the physical sysfs directory covers all its
    // hidraw interfaces and all users; no writable lockfile or root-only setup.
    _lock: File,
}

fn lock(file: &File) -> Result<()> {
    // SAFETY: valid owned descriptor; flock has no pointer arguments.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::WouldBlock {
            return Err(Error::new(
                1,
                "busy",
                "another elan-haptune process holds this device; retry when it exits",
            ));
        }
        return Err(Error::io("lock physical device", e));
    }
    Ok(())
}

#[repr(C)]
#[derive(Default)]
struct RawInfo {
    bus: u32,
    vendor: i16,
    product: i16,
}

impl Hidraw {
    fn open(device: &Device) -> Result<Self> {
        if device.support == "unsupported" {
            return Err(Error::unsupported(format!(
                "{} is outside the ELAN2703 I2C profile",
                device.path.display()
            )));
        }
        let lock_file = File::open(&device.physical_path)
            .map_err(|e| Error::io("open physical device for locking", e))?;
        lock(&lock_file)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&device.path)
            .map_err(|e| Error::io(device.path.display(), e))?;
        let metadata = file
            .metadata()
            .map_err(|e| Error::io("stat hidraw descriptor", e))?;
        if !metadata.file_type().is_char_device() || metadata.rdev() != device.devnum {
            return Err(Error::unsupported("device node changed during selection"));
        }
        let current = fs::canonicalize(
            Path::new("/sys/dev/char")
                .join(format!(
                    "{}:{}",
                    libc::major(device.devnum),
                    libc::minor(device.devnum)
                ))
                .join("device"),
        )
        .map_err(|e| Error::io("recheck HID sysfs identity", e))?;
        if current != device.sys_device {
            return Err(Error::unsupported("HID device changed during selection"));
        }
        let mut info = RawInfo::default();
        // SAFETY: HIDIOCGRAWINFO writes exactly the repr(C) Linux hidraw_devinfo.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x80084803 as libc::c_ulong, &mut info) } < 0 {
            return Err(Error::io("HIDIOCGRAWINFO", io::Error::last_os_error()));
        }
        if (info.bus, info.vendor as u16, info.product as u16) != (0x18, 0x04f3, 0x323b) {
            return Err(Error::unsupported(
                "opened device has an unexpected bus/VID/PID",
            ));
        }
        Ok(Self {
            file,
            _lock: lock_file,
        })
    }
    fn feature(&mut self, request: libc::c_ulong, report: &mut [u8; 5]) -> Result<usize> {
        // SAFETY: requests encode a five-byte buffer; it remains live and writable.
        let count = unsafe { libc::ioctl(self.file.as_raw_fd(), request, report.as_mut_ptr()) };
        if count < 0 {
            return Err(Error::io(
                "hidraw feature report",
                io::Error::last_os_error(),
            ));
        }
        Ok(count as usize)
    }
}

impl FeatureIo for Hidraw {
    fn set_feature(&mut self, report: &mut [u8; 5]) -> Result<usize> {
        self.feature(0xc0054806, report)
    }
    fn get_feature(&mut self, report: &mut [u8; 5]) -> Result<usize> {
        self.feature(0xc0054807, report)
    }
}

fn candidates(devices: Vec<Device>, selector: Option<&str>) -> Result<Vec<Device>> {
    if let Some(selector) = selector {
        let matching: Vec<_> = if selector.starts_with("sysfs:") {
            devices
                .into_iter()
                .filter(|d| d.selector == selector)
                .collect()
        } else if Path::new(selector).is_absolute() {
            let path = fs::canonicalize(selector).map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    Error::new(3, "no_device", format!("device path not found: {selector}"))
                } else {
                    Error::io(selector, e)
                }
            })?;
            devices.into_iter().filter(|d| d.path == path).collect()
        } else {
            return Err(Error::invalid(
                "--device must be an absolute hidraw path or a sysfs: selector from list",
            ));
        };
        if matching.is_empty() {
            return Err(Error::new(
                3,
                "no_device",
                "no hidraw device matches the selector",
            ));
        }
        if matching.iter().all(|d| d.support == "unsupported") {
            return Err(Error::unsupported(
                "selected device has no supported profile",
            ));
        }
        return Ok(matching
            .into_iter()
            .filter(|d| d.support != "unsupported")
            .collect());
    }
    Ok(devices
        .into_iter()
        .filter(|d| d.support != "unsupported")
        .collect())
}

pub fn select(selector: Option<&str>) -> Result<(Device, Protocol<Hidraw>)> {
    select_from(discover()?, selector, |device| {
        Ok(Protocol(Hidraw::open(device)?))
    })
}

fn select_from<T: FeatureIo>(
    devices: Vec<Device>,
    selector: Option<&str>,
    mut open: impl FnMut(&Device) -> Result<Protocol<T>>,
) -> Result<(Device, Protocol<T>)> {
    let devices = candidates(devices, selector)?;
    let mut compatible = BTreeMap::new();
    let mut rejected = None;
    for device in devices {
        if compatible.contains_key(&device.selector) {
            return Err(Error::new(
                4,
                "ambiguous",
                "multiple candidate interfaces on one physical device; select an explicit hidraw path",
            ));
        }
        let mut io = open(&device)?;
        match io.check_identity() {
            Ok(()) => {
                compatible.insert(device.selector.clone(), (device, io));
            }
            Err(e) if e.exit_code == 5 => {
                rejected = Some(e);
            }
            Err(e) => return Err(e), // Never silently ignore an inaccessible candidate.
        }
    }
    match compatible.len() {
        0 => Err(if selector.is_some() {
            rejected
                .unwrap_or_else(|| Error::new(3, "no_device", "no known-compatible device found"))
        } else {
            Error::new(3, "no_device", "no known-compatible device found")
        }),
        1 => Ok(compatible.into_values().next().expect("one entry")),
        _ => Err(Error::new(
            4,
            "ambiguous",
            "multiple known-compatible physical devices; use --device",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Pad;

    fn candidate(id: &str) -> Device {
        Device {
            path: format!("/dev/{id}").into(),
            selector: format!("sysfs:/devices/{id}"),
            name: "ELAN2703:00 04F3:323B".into(),
            bus: 0x18,
            vendor_id: "04f3".into(),
            product_id: "323b".into(),
            physical_path: format!("/sys/devices/{id}").into(),
            support: "identity_check_required",
            sys_device: PathBuf::new(),
            devnum: 0,
        }
    }
    fn open_simulator(_: &Device) -> Result<Protocol<Pad>> {
        Ok(Protocol(Pad::new([150, 125, 60, 0x100])))
    }

    #[test]
    fn selection_distinguishes_no_match_ambiguity_and_explicit_unsupported() {
        let code = |result: Result<(Device, Protocol<Pad>)>| result.err().unwrap().exit_code;
        assert_eq!(code(select_from(vec![], None, open_simulator)), 3);
        assert_eq!(
            code(select_from(
                vec![candidate("a"), candidate("b")],
                None,
                open_simulator
            )),
            4
        );
        let (selected, _) = select_from(
            vec![candidate("a"), candidate("b")],
            Some("sysfs:/devices/b"),
            open_simulator,
        )
        .unwrap();
        assert_eq!(selected.selector, "sysfs:/devices/b");
        let mut unknown = candidate("a");
        unknown.support = "unsupported";
        assert_eq!(
            code(select_from(
                vec![unknown],
                Some("sysfs:/devices/a"),
                |_| panic!("unknown devices must never be queried")
            )),
            5
        );
        assert_eq!(
            code(select_from(
                vec![candidate("a"), candidate("a")],
                None,
                open_simulator
            )),
            4
        );
    }

    #[test]
    fn selection_checks_protocol_identity_and_never_hides_permission_or_read_failure() {
        let bad_identity = |_: &Device| {
            let mut pad = Pad::new([150, 125, 60, 0x100]);
            pad.registers.insert(0x0101, 0x000e);
            Ok(Protocol(pad))
        };
        assert_eq!(
            select_from(vec![candidate("a")], None, bad_identity)
                .err()
                .unwrap()
                .exit_code,
            3
        );
        assert_eq!(
            select_from(vec![candidate("a")], Some("sysfs:/devices/a"), bad_identity)
                .err()
                .unwrap()
                .exit_code,
            5
        );
        for kind in ["permission", "protocol"] {
            let error = select_from(vec![candidate("a"), candidate("b")], None, |d| {
                if d.selector.ends_with('b') {
                    Err(Error::new(1, kind, "injected failure"))
                } else {
                    open_simulator(d)
                }
            })
            .err()
            .unwrap();
            assert_eq!(error.kind, kind);
            assert_eq!(error.exit_code, 1);
        }
    }
    #[test]
    fn vendor_id_alone_never_means_supported() {
        assert!(known_model(0x18, 0x04f3, 0x323b, "ELAN2703:00 04F3:323B"));
        for (bus, vid, pid, name) in [
            (3, 0x04f3, 0x323b, "ELAN2703:00"),
            (0x18, 0x04f3, 0x2f75, "ELAN2513:00"),
            (0x18, 0x1234, 0x323b, "ELAN2703:00"),
            (0x18, 0x04f3, 0x323b, "Unknown"),
        ] {
            assert!(!known_model(bus, vid, pid, name));
        }
        assert_eq!(
            hid_id("HID_ID=0018:000004F3:0000323B\n").unwrap(),
            (24, 1267, 12859)
        );
        assert!(hid_id("HID_ID=1:2").is_err());
        assert!(hid_id("HID_ID=1:2:garbage").is_err());
    }
    #[test]
    fn directory_lock_serializes_independent_opens_and_releases_on_drop() {
        let path =
            std::env::temp_dir().join(format!("elan-haptune-lock-test-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        let first = File::open(&path).unwrap();
        let second = File::open(&path).unwrap();
        lock(&first).unwrap();
        assert_eq!(lock(&second).unwrap_err().kind, "busy");
        drop(first);
        lock(&second).unwrap();
        drop(second);
        fs::remove_dir(path).unwrap();
    }
}

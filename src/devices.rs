//! Drives for the sidebar: listed with lsblk, mounted and ejected through udisksctl.

use eframe::egui;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    /// Block device, e.g. /dev/sdc1.
    pub dev: PathBuf,
    /// Whole disk it lives on, powered off on eject.
    pub disk: PathBuf,
    pub fstype: String,
    pub mount: Option<PathBuf>,
    /// Plugged in over USB and the like: offer eject.
    pub removable: bool,
}

impl Device {
    /// The sidebar path: the mount point, or the block device while unmounted.
    pub fn path(&self) -> &Path {
        self.mount.as_deref().unwrap_or(&self.dev)
    }
}

/// Filesystems that are never mounted as a folder.
const NOT_MOUNTABLE: &[&str] = &[
    "swap",
    "crypto_LUKS",
    "LVM2_member",
    "linux_raid_member",
    "zfs_member",
    "bcache",
    "BitLocker",
];

/// EFI system, Microsoft reserved, Windows recovery and BIOS boot partitions.
const SYSTEM_PARTS: &[&str] = &[
    "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
    "e3c9e316-0b5c-4db8-817d-f92df00215ae",
    "de94bba4-06d1-4d40-a16a-bfd50179d6ac",
    "21686148-6449-6e6f-744e-656564454649",
    "0xef",
    "0x27",
];

fn in_media_dir(mnt: &str) -> bool {
    ["/mnt", "/media", "/run/media"]
        .iter()
        .any(|d| mnt.starts_with(d))
}

pub fn scan() -> Vec<Device> {
    let out = Command::new("lsblk")
        .args([
            "-P",
            "-b",
            "-o",
            "PATH,PKNAME,TYPE,FSTYPE,LABEL,MOUNTPOINT,HOTPLUG,PARTTYPE,SIZE",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_lsblk(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

fn parse_lsblk(text: &str) -> Vec<Device> {
    let rows: Vec<Vec<(String, String)>> = text.lines().map(parse_pairs).collect();
    let get = |row: &Vec<(String, String)>, k: &str| {
        row.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let mut devices = Vec::new();
    for row in &rows {
        let (path, kind, fstype, mount) = (
            get(row, "PATH"),
            get(row, "TYPE"),
            get(row, "FSTYPE"),
            get(row, "MOUNTPOINT"),
        );
        let pkname = get(row, "PKNAME");
        let disk = if kind == "disk" {
            path.clone()
        } else {
            format!("/dev/{pkname}")
        };
        let removable = get(row, "HOTPLUG") == "1";
        if !mount.is_empty() {
            // Mounted elsewhere (/, /boot, /home…) is the system, not a drive.
            if !in_media_dir(&mount) {
                continue;
            }
            let name = Path::new(&mount)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(mount.clone());
            devices.push(Device {
                name,
                dev: path.into(),
                disk: disk.into(),
                fstype,
                mount: Some(mount.into()),
                removable,
            });
            continue;
        }
        let has_children = rows.iter().any(|r| {
            get(r, "PKNAME")
                == Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
        });
        let mountable = matches!(kind.as_str(), "part" | "disk")
            && !(kind == "disk" && has_children)
            && !fstype.is_empty()
            && !NOT_MOUNTABLE.contains(&fstype.as_str())
            && !SYSTEM_PARTS.contains(&get(row, "PARTTYPE").to_lowercase().as_str());
        if !mountable {
            continue;
        }
        let label = get(row, "LABEL");
        let name = if label.is_empty() {
            let size: u64 = get(row, "SIZE").parse().unwrap_or(0);
            format!("{} Volume", crate::util::human_size(size))
        } else {
            label
        };
        devices.push(Device {
            name,
            dev: path.into(),
            disk: disk.into(),
            fstype,
            mount: None,
            removable,
        });
    }
    devices
}

/// One line of `lsblk -P`: KEY="value" pairs, with \xNN escapes in values.
fn parse_pairs(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].trim().to_string();
        let after = &rest[eq + 2..];
        let Some(end) = after.find('"') else { break };
        out.push((key, unescape(&after[..end])));
        rest = &after[end + 1..];
    }
    out
}

fn unescape(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1] == b'x'
            && let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16)
        {
            bytes.push(v);
            i += 4;
            continue;
        }
        bytes.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Rescans when the mount table or the set of block devices changes.
pub fn watch(ctx: egui::Context) -> Receiver<Vec<Device>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let signature = || {
            let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
            let mut blocks: Vec<String> = std::fs::read_dir("/sys/class/block")
                .map(|d| {
                    d.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            blocks.sort();
            (mounts, blocks)
        };
        let mut last = signature();
        // A new disk shows up before udev has probed its label; look again shortly after.
        let mut recheck = 0;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let now = signature();
            if now != last {
                last = now;
                recheck = 2;
            } else if recheck == 0 {
                continue;
            }
            recheck -= 1;
            if tx.send(scan()).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    rx
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DevOp {
    /// Mount, then open it in the active tab.
    Mount,
    Unmount,
    Eject,
}

pub struct DevResult {
    pub op: DevOp,
    pub dev: PathBuf,
    pub name: String,
    pub result: Result<Option<PathBuf>, String>,
    /// Mounted, but read-only because the drive was marked dirty.
    pub read_only: bool,
}

/// Runs the operation off the UI thread; polkit may ask for a password.
pub fn run(
    op: DevOp,
    device: &Device,
    siblings: Vec<PathBuf>,
    tx: Sender<DevResult>,
    ctx: egui::Context,
) {
    let device = device.clone();
    std::thread::spawn(move || {
        let dev = device.dev.to_string_lossy().into_owned();
        let mut read_only = false;
        let result = match op {
            DevOp::Mount => match udisks(&["mount", "-b", &dev]) {
                Ok(out) => Ok(mounted_at(&out)),
                // NTFS left dirty (unplugged without ejecting) only mounts read-only.
                Err(_) if device.fstype == "ntfs" => {
                    read_only = true;
                    udisks(&["mount", "-b", &dev, "-o", "ro"]).map(|out| mounted_at(&out))
                }
                Err(e) => Err(e),
            },
            DevOp::Unmount => udisks(&["unmount", "-b", &dev]).map(|_| None),
            DevOp::Eject => siblings
                .iter()
                .try_for_each(|p| udisks(&["unmount", "-b", &p.to_string_lossy()]).map(|_| ()))
                .and_then(|_| udisks(&["power-off", "-b", &device.disk.to_string_lossy()]))
                .map(|_| None),
        };
        let _ = tx.send(DevResult {
            op,
            dev: device.dev,
            name: device.name,
            result,
            read_only,
        });
        ctx.request_repaint();
    });
}

fn udisks(args: &[&str]) -> Result<String, String> {
    let out = Command::new("udisksctl")
        .args(args)
        .output()
        .map_err(|e| format!("udisksctl: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(clean_error(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// "Mounted /dev/sdc1 at /run/media/vp/ssd t5 Portable" → the mount point.
fn mounted_at(out: &str) -> Option<PathBuf> {
    let (_, at) = out.trim().split_once(" at ")?;
    Some(PathBuf::from(at.trim_end_matches('.')))
}

/// udisks errors come wrapped in D-Bus names; keep the readable part.
fn clean_error(err: &str) -> String {
    let line = err
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("failed")
        .trim();
    if line.contains("target is busy") {
        return "it is in use (close files and terminals open on it)".into();
    }
    if line.contains("NotAuthorized") {
        return "not authorized".into();
    }
    let msg = line.rsplit_once("Error.").map(|(_, m)| m).unwrap_or(line);
    let msg = msg.split_once(": ").map(|(_, m)| m).unwrap_or(msg);
    msg.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSBLK: &str = r#"PATH="/dev/sda" PKNAME="" TYPE="disk" FSTYPE="" LABEL="" MOUNTPOINT="" HOTPLUG="0" PARTTYPE="" SIZE="500107862016"
PATH="/dev/sda1" PKNAME="sda" TYPE="part" FSTYPE="ntfs" LABEL="SSD2" MOUNTPOINT="/run/media/vp/SSD2" HOTPLUG="0" PARTTYPE="0x7" SIZE="496410324992"
PATH="/dev/sdc" PKNAME="" TYPE="disk" FSTYPE="" LABEL="" MOUNTPOINT="" HOTPLUG="1" PARTTYPE="" SIZE="500107862016"
PATH="/dev/sdc1" PKNAME="sdc" TYPE="part" FSTYPE="ntfs" LABEL="ssd\x20t5\x20Portable" MOUNTPOINT="" HOTPLUG="1" PARTTYPE="0x7" SIZE="500105740288"
PATH="/dev/sdd" PKNAME="" TYPE="disk" FSTYPE="iso9660" LABEL="OMARCHY" MOUNTPOINT="" HOTPLUG="1" PARTTYPE="" SIZE="64000000000"
PATH="/dev/sdd1" PKNAME="sdd" TYPE="part" FSTYPE="iso9660" LABEL="OMARCHY" MOUNTPOINT="/run/media/vp/OMARCHY" HOTPLUG="1" PARTTYPE="0x0" SIZE="6000000000"
PATH="/dev/sdd2" PKNAME="sdd" TYPE="part" FSTYPE="vfat" LABEL="ARCHISO_EFI" MOUNTPOINT="" HOTPLUG="1" PARTTYPE="0xef" SIZE="24000000"
PATH="/dev/zram0" PKNAME="" TYPE="disk" FSTYPE="swap" LABEL="zram0" MOUNTPOINT="[SWAP]" HOTPLUG="0" PARTTYPE="" SIZE="33000000000"
PATH="/dev/nvme1n1p1" PKNAME="nvme1n1" TYPE="part" FSTYPE="vfat" LABEL="" MOUNTPOINT="/boot" HOTPLUG="0" PARTTYPE="c12a7328-f81f-11d2-ba4b-00a0c93ec93b" SIZE="2000000000"
PATH="/dev/nvme1n1p2" PKNAME="nvme1n1" TYPE="part" FSTYPE="crypto_LUKS" LABEL="" MOUNTPOINT="" HOTPLUG="0" PARTTYPE="4f68bce3-e8cd-4db1-96e7-fbcaf984b709" SIZE="998000000000"
PATH="/dev/mapper/root" PKNAME="nvme1n1p2" TYPE="crypt" FSTYPE="btrfs" LABEL="" MOUNTPOINT="/" HOTPLUG="0" PARTTYPE="" SIZE="998000000000"
PATH="/dev/sde1" PKNAME="sde" TYPE="part" FSTYPE="exfat" LABEL="" MOUNTPOINT="" HOTPLUG="1" PARTTYPE="0x7" SIZE="32000000000""#;

    #[test]
    fn lists_drives_mounted_and_not() {
        let d = parse_lsblk(LSBLK);
        let names: Vec<(&str, bool)> = d
            .iter()
            .map(|d| (d.name.as_str(), d.mount.is_some()))
            .collect();
        assert_eq!(
            names,
            [
                ("SSD2", true),
                ("ssd t5 Portable", false),
                ("OMARCHY", true),
                ("29.8 GB Volume", false)
            ]
        );
        let t5 = &d[1];
        assert_eq!(t5.dev, PathBuf::from("/dev/sdc1"));
        assert_eq!(t5.disk, PathBuf::from("/dev/sdc"));
        assert!(t5.removable && !d[0].removable);
    }

    #[test]
    fn reads_udisks_output() {
        assert_eq!(
            mounted_at("Mounted /dev/sdc1 at /run/media/vp/ssd t5 Portable\n"),
            Some(PathBuf::from("/run/media/vp/ssd t5 Portable"))
        );
        assert_eq!(
            clean_error(
                "Error mounting /dev/sdc1: GDBus.Error:org.freedesktop.UDisks2.Error.Failed: Error mounting /dev/sdc1 at /run/media/vp/x: wrong fs type"
            ),
            "Error mounting /dev/sdc1 at /run/media/vp/x: wrong fs type"
        );
        assert_eq!(
            clean_error(
                "Error unmounting /dev/sdc1: GDBus.Error:org.freedesktop.UDisks2.Error.DeviceBusy: target is busy"
            ),
            "it is in use (close files and terminals open on it)"
        );
    }
}

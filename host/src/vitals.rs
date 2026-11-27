//! Hardware facts and on-demand load sampling in the process that owns signaling.

use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use sysinfo::{CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, System};
use wire::signal::Signal;
use wire::types::{DiskUsage, Machine, Usage};

use crate::wake::Wake;

pub(crate) fn machine() -> Option<Machine> {
    let mut system = System::new().ok()?;
    system.refresh_cpu_list(CpuRefreshKind::nothing());
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());

    Some(Machine {
        cpu: system.cpus().first()?.brand().trim().to_string(),
        cores: System::physical_core_count().unwrap_or(0) as u32,
        threads: system.cpus().len() as u32,
        memory: system.total_memory(),
    })
}

pub(crate) struct Sample {
    cpu: f64,
    memory: Usage,
    disks: Vec<DiskUsage>,
}

impl Sample {
    pub(crate) fn request(reply: Wake<Option<Self>>) -> std::io::Result<()> {
        thread::Builder::new()
            .name("vitals".into())
            .spawn(move || {
                let _ = reply.send(Self::read());
            })?;
        Ok(())
    }

    fn read() -> Option<Self> {
        let mut system = System::new().ok()?;
        system.refresh_cpu_usage();
        thread::sleep(Duration::from_millis(250).max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL));
        system.refresh_cpu_usage();
        system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());

        Some(Self {
            cpu: cpu_percent(system.global_cpu_usage()),
            memory: Usage {
                used: system.used_memory(),
                total: system.total_memory(),
            },
            disks: disks(),
        })
    }

    pub(crate) fn message(self, rtt: Option<u64>) -> Signal {
        Signal::Vitals {
            cpu: self.cpu,
            memory: self.memory,
            disks: self.disks,
            rtt,
        }
    }
}

fn cpu_percent(usage: f32) -> f64 {
    (f64::from(usage).clamp(0.0, 100.0) * 10.0).round() / 10.0
}

const APFS_DATA: &str = "/System/Volumes/Data";

fn disk_name(removable: bool, fs: &str, mount: &str, source: &str) -> Option<String> {
    if removable
        || matches!(
            fs,
            "" | "tmpfs"
                | "devtmpfs"
                | "ramfs"
                | "overlay"
                | "squashfs"
                | "rootfs"
                | "proc"
                | "sysfs"
                | "devfs"
                | "cgroup"
                | "cgroup2"
                | "debugfs"
                | "tracefs"
                | "nfs"
                | "nfs4"
                | "cifs"
                | "smb3"
                | "smbfs"
                | "autofs"
                | "9p"
                | "ceph"
                | "glusterfs"
                | "virtiofs"
                | "afs"
                | "davfs"
        )
        || (fs == "fuse" || fs.starts_with("fuse.")) && !source.starts_with("/dev/")
        || source.starts_with("/dev/loop")
    {
        return None;
    }
    if fs == "apfs" && mount.starts_with("/System/Volumes/") {
        return (mount == APFS_DATA).then(|| "/".into());
    }
    if let [drive, b':', b'\\' | b'/'] = mount.as_bytes() {
        if drive.is_ascii_alphabetic() {
            return Some(format!("{}:", drive.to_ascii_uppercase() as char));
        }
    }
    mount.starts_with('/').then(|| mount.into())
}

fn disks() -> Vec<DiskUsage> {
    let Ok(mut disks) = Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing()) else {
        return Vec::new();
    };
    let mut volumes = BTreeMap::new();
    let storage = DiskRefreshKind::nothing().with_storage();
    for disk in &mut disks {
        let mount = disk.mount_point().to_string_lossy();
        let startup_data = mount == APFS_DATA;
        let Some(name) = disk_name(
            disk.is_removable(),
            &disk.file_system().to_string_lossy(),
            &mount,
            &disk.name().to_string_lossy(),
        ) else {
            continue;
        };
        if !disk.refresh_specifics(storage) || disk.total_space() == 0 {
            continue;
        }
        let total = disk.total_space();
        let entry = DiskUsage {
            name: name.clone(),
            used: total.saturating_sub(disk.available_space()),
            total,
        };
        // APFS's writable Data volume represents the startup disk when both mounts are reported.
        if startup_data {
            volumes.insert(name, entry);
        } else {
            volumes.entry(name).or_insert(entry);
        }
    }
    volumes.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_local_mounts_have_user_facing_names() {
        for (removable, fs, mount, source, expected) in [
            (false, "NTFS", r"C:\", "Windows", Some("C:")),
            (false, "NTFS", r"D:\", "Data", Some("D:")),
            (true, "exfat", r"E:\", "USB", None),
            (false, "apfs", "/", "Macintosh HD", Some("/")),
            (false, "apfs", APFS_DATA, "Data", Some("/")),
            (false, "apfs", "/System/Volumes/VM", "VM", None),
            (
                false,
                "apfs",
                "/Volumes/Archive",
                "Archive",
                Some("/Volumes/Archive"),
            ),
            (false, "ext4", "/", "/dev/nvme0n1p2", Some("/")),
            (false, "xfs", "/home", "/dev/mapper/home", Some("/home")),
            (false, "btrfs", "/mnt/image", "/dev/loop0", None),
            (false, "tmpfs", "/run", "tmpfs", None),
            (false, "overlay", "/", "overlay", None),
            (false, "squashfs", "/snap/app", "/dev/loop0", None),
            (false, "devtmpfs", "/dev", "udev", None),
            (false, "nfs4", "/mnt/share", "host:/share", None),
            (false, "fuse.sshfs", "/mnt/share", "host:/share", None),
            (false, "fuse.s3fs", "/mnt/bucket", "bucket", None),
            (
                false,
                "fuseblk",
                "/mnt/data",
                "/dev/sda1",
                Some("/mnt/data"),
            ),
        ] {
            assert_eq!(
                disk_name(removable, fs, mount, source).as_deref(),
                expected,
                "{fs} at {mount}"
            );
        }
    }

    #[test]
    fn cpu_is_bounded_and_rounded_to_one_decimal() {
        for (raw, expected) in [
            (-1.0, 0.0),
            (0.0, 0.0),
            (12.34, 12.3),
            (12.36, 12.4),
            (101.0, 100.0),
        ] {
            assert_eq!(cpu_percent(raw), expected, "{raw}");
        }
    }
}

//! Safely ejecting a drive.
//!
//! - Windows: find the disk behind the drive letter and ask Plug and Play to
//!   eject its parent device (`CM_Request_Device_EjectW`). Windows flushes the
//!   volume and refuses (a "veto") if a program still has a file open.
//! - macOS: `diskutil eject <mount>`.
//! - Linux: `udisksctl unmount -b <dev>`, then `udisksctl power-off -b <dev>`.

use crate::drives::DriveInfo;
use crate::{Error, Result};

/// Ejects the drive. Every file the app wrote has already been fsynced.
pub fn eject(drive: &DriveInfo) -> Result<()> {
    imp::eject(drive)
}

#[cfg(windows)]
mod imp {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Parent, CM_Request_Device_EjectW, CR_SUCCESS, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, PNP_VETO_TYPE,
        PNP_VetoTypeUnknown, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        GUID_DEVINTERFACE_DISK, IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER,
    };

    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateFileW and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open_device(path: &[u16]) -> Option<Handle> {
        // SAFETY: `path` is NUL-terminated; no access rights are requested,
        // which is enough for IOCTL_STORAGE_GET_DEVICE_NUMBER.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE).then_some(Handle(h))
    }

    fn device_number(handle: &Handle) -> Option<u32> {
        let mut number = STORAGE_DEVICE_NUMBER {
            DeviceType: 0,
            DeviceNumber: 0,
            PartitionNumber: 0,
        };
        let mut returned = 0u32;
        // SAFETY: output buffer is a correctly sized STORAGE_DEVICE_NUMBER.
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                null(),
                0,
                (&mut number as *mut STORAGE_DEVICE_NUMBER).cast(),
                size_of::<STORAGE_DEVICE_NUMBER>() as u32,
                &mut returned,
                null_mut(),
            )
        };
        (ok != 0).then_some(number.DeviceNumber)
    }

    /// The device-instance handle of the disk with this device number.
    fn disk_devinst(target: u32) -> Option<u32> {
        // SAFETY: standard SetupAPI enumeration; every out-structure has its
        // cbSize set and the detail buffer is u32-aligned and large enough.
        unsafe {
            let set = SetupDiGetClassDevsW(
                &GUID_DEVINTERFACE_DISK,
                null(),
                null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            );
            if set == -1 {
                return None;
            }
            let mut found = None;
            let mut index = 0;
            loop {
                let mut iface: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
                iface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
                if SetupDiEnumDeviceInterfaces(set, null(), &GUID_DEVINTERFACE_DISK, index, &mut iface) == 0 {
                    break;
                }
                index += 1;
                let mut needed = 0u32;
                SetupDiGetDeviceInterfaceDetailW(set, &iface, null_mut(), 0, &mut needed, null_mut());
                if needed == 0 {
                    continue;
                }
                let mut buf = vec![0u32; (needed as usize).div_ceil(4) + 1];
                let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
                (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                let mut info: SP_DEVINFO_DATA = std::mem::zeroed();
                info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
                if SetupDiGetDeviceInterfaceDetailW(set, &iface, detail, needed, null_mut(), &mut info) == 0 {
                    continue;
                }
                let path_ptr = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
                let mut len = 0;
                while *path_ptr.add(len) != 0 {
                    len += 1;
                }
                let path: Vec<u16> = std::slice::from_raw_parts(path_ptr, len + 1).to_vec();
                if let Some(h) = open_device(&path)
                    && device_number(&h) == Some(target)
                {
                    found = Some(info.DevInst);
                    break;
                }
            }
            SetupDiDestroyDeviceInfoList(set);
            found
        }
    }

    pub fn eject(drive: &DriveInfo) -> Result<()> {
        let letter = drive
            .mount_point
            .chars()
            .next()
            .filter(|c| c.is_ascii_alphabetic() && drive.mount_point.get(1..2) == Some(":"))
            .ok_or_else(|| {
                Error::Eject(format!(
                    "{} isn't a drive letter, so it can't be ejected here",
                    drive.mount_point
                ))
            })?;

        let volume = open_device(&wide(&format!(r"\\.\{letter}:")))
            .ok_or_else(|| Error::Eject(format!("Couldn't open drive {letter}:. Is it still plugged in?")))?;
        let number = device_number(&volume)
            .ok_or_else(|| Error::Eject(format!("Couldn't identify the disk behind {letter}:")))?;
        drop(volume);

        let devinst =
            disk_devinst(number).ok_or_else(|| Error::Eject(format!("Couldn't find the device for {letter}:")))?;
        let mut parent = 0u32;
        // SAFETY: plain out-parameter.
        let target = if unsafe { CM_Get_Parent(&mut parent, devinst, 0) } == CR_SUCCESS {
            parent
        } else {
            devinst
        };

        let mut last_veto = String::new();
        for _ in 0..3 {
            let mut veto: PNP_VETO_TYPE = PNP_VetoTypeUnknown;
            let mut name = [0u16; 260];
            // SAFETY: `name` holds 260 wide chars as declared.
            let rc = unsafe { CM_Request_Device_EjectW(target, &mut veto, name.as_mut_ptr(), name.len() as u32, 0) };
            if rc == CR_SUCCESS && veto == PNP_VetoTypeUnknown {
                return Ok(());
            }
            let end = name.iter().position(|&c| c == 0).unwrap_or(0);
            last_veto = String::from_utf16_lossy(&name[..end]);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Err(Error::Eject(format!(
            "Windows couldn't eject {letter}: because a program is still using it{}. \
             Close any windows or files open on the drive, then press Eject again.",
            if last_veto.is_empty() {
                String::new()
            } else {
                format!(" ({last_veto})")
            }
        )))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::process::Command;

    use super::*;

    pub fn eject(drive: &DriveInfo) -> Result<()> {
        let out = Command::new("diskutil")
            .args(["eject", &drive.mount_point])
            .output()
            .map_err(|e| Error::Eject(format!("Couldn't run diskutil: {e}")))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(Error::Eject(format!(
                "macOS couldn't eject the drive: {}. Close any files open on it and press Eject again.",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::process::Command;

    use super::*;

    /// The block device mounted at `mount`, from /proc/self/mounts.
    fn device_for(mount: &str) -> Option<String> {
        let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
        mounts.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            let dev = parts.next()?;
            let at = parts.next()?.replace("\\040", " ");
            (at == mount && dev.starts_with("/dev/")).then(|| dev.to_string())
        })
    }

    fn udisksctl(args: &[&str]) -> std::result::Result<(), String> {
        let out = Command::new("udisksctl")
            .args(args)
            .output()
            .map_err(|e| format!("couldn't run udisksctl ({e}); is udisks2 installed?"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub fn eject(drive: &DriveInfo) -> Result<()> {
        let dev = drive
            .device
            .clone()
            .filter(|d| d.starts_with("/dev/"))
            .or_else(|| device_for(&drive.mount_point))
            .ok_or_else(|| Error::Eject(format!("Couldn't find the device mounted at {}", drive.mount_point)))?;
        unsafe { libc::sync() };
        udisksctl(&["unmount", "-b", &dev]).map_err(|e| {
            Error::Eject(format!(
                "Step 1 of 2 (unmount) failed: {e}. Close any files open on the drive and press Eject again."
            ))
        })?;
        udisksctl(&["power-off", "-b", &dev]).map_err(|e| {
            Error::Eject(format!(
                "The drive is unmounted and safe to remove, but step 2 of 2 (power off) failed: {e}"
            ))
        })
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub fn eject(_drive: &DriveInfo) -> Result<()> {
        Err(Error::Eject("Eject isn't supported on this system".into()))
    }
}

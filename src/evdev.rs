//! Just enough evdev: find the Wacom pad and pen, grab or filter them, read
//! their events.

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::mem;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const BTN_0: u16 = 0x100;
pub const BTN_TOOL_RUBBER: u16 = 0x141;
pub const BTN_STYLUS: u16 = 0x14b;
pub const REL_WHEEL: u16 = 0x08;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_WHEEL: u16 = 0x08;
pub const ABS_MISC: u16 = 0x28;

const WACOM_VENDOR: &str = "056a";
const KEY_CNT: usize = 0x300;
/// _IOW('E', 0x90, int): exclusive access, so no one else (Hyprland) gets the events.
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
/// _IOW('E', 0x93, struct input_mask): per-client event filter.
const EVIOCSMASK: libc::c_ulong = 0x4010_4593;
/// _IOR('E', 0x40 + axis, struct input_absinfo).
const EVIOCGABS_BASE: libc::c_ulong = 0x8018_4540;
/// EVIOCGBIT(EV_REL, 8): which relative axes the device has.
const EVIOCGBIT_REL: libc::c_ulong = 0x8008_4522;
const EVENT_SIZE: usize = mem::size_of::<libc::input_event>();

pub struct Event {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// The kernel's struct input_mask.
#[repr(C)]
struct InputMask {
    kind: u32,
    codes_size: u32,
    codes_ptr: u64,
}

pub struct Device {
    reader: BufReader<File>,
}

impl Device {
    pub fn open(path: &Path) -> io::Result<Self> {
        // The kernel only hands out whole events, so the buffer stays aligned to them.
        Ok(Self {
            reader: BufReader::with_capacity(64 * EVENT_SIZE, File::open(path)?),
        })
    }

    /// The grab lasts until the device is closed, i.e. until this process exits.
    pub fn grab(&self) -> io::Result<()> {
        // SAFETY: EVIOCGRAB takes its argument by value and the fd is open.
        check(unsafe { libc::ioctl(self.fd(), EVIOCGRAB as _, 1 as libc::c_int) })
    }

    /// Only deliver EV_KEY events with these codes. The kernel drops packets
    /// that end up empty, so everything else (motion, pressure) stops waking us.
    pub fn only_keys(&self, codes: impl IntoIterator<Item = u16>) -> io::Result<()> {
        // For EV_SYN the mask selects event types rather than codes.
        self.set_mask(EV_SYN, &[1 << EV_SYN | 1 << EV_KEY])?;
        let mut keys = [0u64; KEY_CNT / 64];
        for code in codes {
            keys[usize::from(code) / 64] |= 1 << (code % 64);
        }
        self.set_mask(EV_KEY, &keys)
    }

    /// Width and height of the sensing area in mm.
    pub fn size_mm(&self) -> io::Result<(f64, f64)> {
        let mm = |axis| -> io::Result<f64> {
            let info = self.absinfo(axis)?;
            if info.resolution <= 0 {
                return Err(io::Error::other("no resolution reported"));
            }
            Ok(f64::from(info.maximum - info.minimum) / f64::from(info.resolution))
        };
        Ok((mm(ABS_X)?, mm(ABS_Y)?))
    }

    /// Whether the device has this relative axis, e.g. REL_WHEEL for a dial.
    pub fn has_rel(&self, axis: u16) -> io::Result<bool> {
        let mut bits = 0u64;
        // SAFETY: the kernel writes at most the 8 bytes the request size allows.
        check(unsafe { libc::ioctl(self.fd(), EVIOCGBIT_REL as _, &mut bits) })?;
        Ok(bits & 1 << axis != 0)
    }

    pub fn next_event(&mut self) -> io::Result<Event> {
        let mut buf = [0u8; EVENT_SIZE];
        self.reader.read_exact(&mut buf)?;
        // SAFETY: input_event is plain old data and buf holds exactly one.
        let ev: libc::input_event = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
        Ok(Event {
            kind: ev.type_,
            code: ev.code,
            value: ev.value,
        })
    }

    fn set_mask(&self, kind: u16, bits: &[u64]) -> io::Result<()> {
        let mask = InputMask {
            kind: kind.into(),
            codes_size: mem::size_of_val(bits) as u32,
            codes_ptr: bits.as_ptr() as u64,
        };
        // SAFETY: mask and the bitmap it points to outlive the call.
        check(unsafe { libc::ioctl(self.fd(), EVIOCSMASK as _, &mask) })
    }

    fn absinfo(&self, axis: u16) -> io::Result<libc::input_absinfo> {
        // SAFETY: input_absinfo is plain old data; the kernel fills it in.
        let mut info: libc::input_absinfo = unsafe { mem::zeroed() };
        let request = EVIOCGABS_BASE + libc::c_ulong::from(axis);
        check(unsafe { libc::ioctl(self.fd(), request as _, &mut info) })?;
        Ok(info)
    }

    fn fd(&self) -> RawFd {
        self.reader.get_ref().as_raw_fd()
    }
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The first input device with Wacom's vendor id whose name ends in
/// " {kind}", e.g. "Wacom Intuos Pro S Pad" for kind "Pad".
pub fn find_wacom(kind: &str) -> Option<PathBuf> {
    let suffix = format!(" {kind}");
    let mut found: Vec<PathBuf> = fs::read_dir("/sys/class/input")
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("event"))
        .filter(|entry| {
            let device = entry.path().join("device");
            let read = |attr: &str| fs::read_to_string(device.join(attr)).unwrap_or_default();
            read("id/vendor").trim() == WACOM_VENDOR && read("name").trim_end().ends_with(&suffix)
        })
        .map(|entry| Path::new("/dev/input").join(entry.file_name()))
        .collect();
    found.sort();
    found.into_iter().next()
}

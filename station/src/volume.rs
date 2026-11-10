//! The volume knob: ALSA control «Master Playback Volume» (one value 0..255), created by `hardvold`, which maps it
//! onto the SY6045S amplifier (a level N of 0..10 is N × 25.5). Read and written by name through the
//! control device's ioctls (no amixer process per change), and changes are watched as control events.
//! A knob with 2 values is softvol's, not hardvold's: the `soft_volume` PCM was opened and replaced it — then
//! writes are refused (they would reach nothing).
//! The level is ours to set — the buttons and the server's requests go through `Knob::set` — but the firmware
//! writes the knob too: the level Alice was last left at, more than once while it starts after every reboot. The watcher undoes
//! what we did not set and keeps our level in a file for the next start.
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};

pub const DEVICE: &str = "/dev/snd/controlC0";
pub const NAME: &str = "Master Playback Volume";
const IFACE_MIXER: i32 = 2;
/// What this program last set the knob to (−1: nothing yet): by it the watcher tells our writes from the firmware's.
static OURS: AtomicI32 = AtomicI32::new(-1);
/// The same, kept for the next start.
const KEPT: &str = "/data/ranunculus/volume";

#[repr(C)]
#[derive(Clone, Copy)]
struct ElemId {
    numid: u32,
    iface: i32,
    device: u32,
    subdevice: u32,
    name: [u8; 44],
    index: u32,
}

#[repr(C)]
struct ElemInfo {
    id: ElemId,
    typ: i32,
    access: u32,
    count: u32,
    owner: i32,
    value: [i64; 16], // union: integer {min, max, step} …
    dimen: u64,
    reserved: [u8; 56],
}

#[repr(C)]
struct ElemValue {
    id: ElemId,
    indirect: u32,
    _pad: u32,
    value: [i64; 128], // union: integer.value[128] …
    tstamp: [i64; 2],
    reserved: [u8; 112],
}

const fn iowr(nr: u32, size: usize) -> u32 {
    (3 << 30) | ((size as u32) << 16) | (0x55 << 8) | nr
}
const ELEM_INFO: u32 = iowr(0x11, 272);
const ELEM_READ: u32 = iowr(0x12, 1224);
const ELEM_WRITE: u32 = iowr(0x13, 1224);
const SUBSCRIBE_EVENTS: u32 = iowr(0x16, 4);

fn id() -> ElemId {
    let mut name = [0u8; 44];
    name[..NAME.len()].copy_from_slice(NAME.as_bytes());
    ElemId { numid: 0, iface: IFACE_MIXER, device: 0, subdevice: 0, name, index: 0 }
}

pub struct Knob {
    f: File,
}

impl Knob {
    pub fn open() -> std::io::Result<Knob> {
        assert_eq!(std::mem::size_of::<ElemInfo>(), 272);
        assert_eq!(std::mem::size_of::<ElemValue>(), 1224);
        Ok(Knob { f: OpenOptions::new().read(true).write(true).open(DEVICE)? })
    }

    fn info(&self) -> std::io::Result<(u32, i64, i64)> {
        let mut i: ElemInfo = unsafe { std::mem::zeroed() };
        i.id = id();
        if unsafe { libc::ioctl(self.f.as_raw_fd(), ELEM_INFO as _, &mut i as *mut ElemInfo) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((i.count, i.value[0], i.value[1]))
    }

    /// 0..255, or an error (no such knob, or softvol's 2-value one).
    pub fn get(&self) -> std::io::Result<u8> {
        let (count, _, _) = self.info()?;
        let mut v: ElemValue = unsafe { std::mem::zeroed() };
        v.id = id();
        if unsafe { libc::ioctl(self.f.as_raw_fd(), ELEM_READ as _, &mut v as *mut ElemValue) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if count != 1 {
            return Err(std::io::Error::other(format!(
                "«{NAME}» has {count} values: softvol's, not hardvold's (something opened the soft_volume PCM)"
            )));
        }
        Ok(v.value[0].clamp(0, 255) as u8)
    }

    pub fn set(&self, level: u8) -> std::io::Result<()> {
        let (count, min, max) = self.info()?;
        if count != 1 {
            return Err(std::io::Error::other(format!(
                "«{NAME}» has {count} values: softvol's, not hardvold's — refusing to write"
            )));
        }
        let mut v: ElemValue = unsafe { std::mem::zeroed() };
        v.id = id();
        v.value[0] = (level as i64).clamp(min, max);
        // said before it is written: the watcher hears of the change at once
        OURS.store(v.value[0] as i32, Ordering::SeqCst);
        if unsafe { libc::ioctl(self.f.as_raw_fd(), ELEM_WRITE as _, &mut v as *mut ElemValue) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

fn kept(path: &Path) -> Option<u8> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Written whole or not at all: a station is switched off by pulling its plug.
fn keep(path: &Path, level: u8) -> std::io::Result<()> {
    let new = path.with_extension("new");
    let mut f = File::create(&new)?;
    f.write_all(level.to_string().as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&new, path)
}

/// The knob shows `shown`, and `ours` is the last level we set (none yet on a station just set up, whose level is
/// the one to keep). → the level to put back, if this one is the firmware's.
fn undone(ours: Option<u8>, shown: u8) -> Option<u8> {
    ours.filter(|o| *o != shown)
}

/// Blocking, forever (its own thread): calls `changed(level)` whenever the knob is set to another level of ours — by
/// the buttons, the server — and puts ours back after the firmware's writes. Reopens the device on errors.
pub fn watch(changed: impl Fn(u8)) {
    if let Some(level) = kept(Path::new(KEPT)) {
        let _ = OURS.compare_exchange(-1, level as i32, Ordering::SeqCst, Ordering::SeqCst);
    }
    let mut last = None;
    loop {
        if let Err(e) = watch_once(&changed, &mut last) {
            eprintln!("{} volume: watch: {e}", crate::stamp());
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

/// The knob shows `level`: undone if it is the firmware's (ours then comes as the next change), else told and kept.
fn seen(knob: &Knob, level: u8, last: &mut Option<u8>, changed: &impl Fn(u8)) {
    let ours = OURS.load(Ordering::SeqCst);
    if let Some(back) = undone(u8::try_from(ours).ok(), level) {
        match knob.set(back) {
            Ok(()) => return eprintln!("{} volume: the firmware's {level} put back to {back}", crate::stamp()),
            Err(e) => eprintln!("{} volume: the firmware's {level} stays: {e}", crate::stamp()),
        }
    }
    // a station just set up, or a level that could not be undone: ours from now on, unless another was set meanwhile
    let _ = OURS.compare_exchange(ours, level as i32, Ordering::SeqCst, Ordering::SeqCst);
    if *last != Some(level) {
        *last = Some(level);
        changed(level);
        if let Err(e) = keep(Path::new(KEPT), level) {
            eprintln!("{} volume: {KEPT}: {e}", crate::stamp());
        }
    }
}

fn watch_once(changed: &impl Fn(u8), last: &mut Option<u8>) -> std::io::Result<()> {
    let knob = Knob::open()?;
    let mut ev = OpenOptions::new().read(true).open(DEVICE)?;
    let mut on: libc::c_int = 1;
    if unsafe { libc::ioctl(ev.as_raw_fd(), SUBSCRIBE_EVENTS as _, &mut on as *mut libc::c_int) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if let Ok(l) = knob.get() {
        seen(&knob, l, last, changed);
    }
    let mut buf = [0u8; 72 * 16];
    loop {
        let n = ev.read(&mut buf)?;
        // struct snd_ctl_event: int type; { unsigned mask; struct snd_ctl_elem_id id; } — 72 bytes each
        let ours = buf[..n].as_chunks::<72>().0.iter().any(|e| {
            let typ = i32::from_le_bytes(e[0..4].try_into().unwrap());
            let mask = u32::from_le_bytes(e[4..8].try_into().unwrap());
            let name = &e[8 + 16..8 + 16 + 44];
            typ == 0 && mask & 1 != 0 && name.starts_with(NAME.as_bytes()) && name[NAME.len()] == 0
        });
        if ours {
            if let Ok(l) = knob.get() {
                seen(&knob, l, last, changed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_firmwares_level_is_told_from_ours() {
        assert_eq!(undone(Some(102), 179), Some(102), "written by the firmware at its start");
        assert_eq!(undone(Some(102), 102), None);
        assert_eq!(undone(None, 179), None, "a station just set up: its level is the one to keep");
    }

    #[test]
    fn the_level_is_kept_for_the_next_start() {
        let path = std::env::temp_dir().join(format!("station-volume-{}", std::process::id()));
        assert_eq!(kept(&path), None);
        keep(&path, 102).unwrap();
        keep(&path, 77).unwrap();
        assert_eq!((kept(&path), path.with_extension("new").exists()), (Some(77), false));
        std::fs::write(&path, "").unwrap();
        assert_eq!(kept(&path), None, "cut short by a pulled plug: as if never kept");
        std::fs::remove_file(&path).unwrap();
    }
}

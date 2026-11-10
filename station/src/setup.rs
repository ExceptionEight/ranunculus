//! A Yandex Station 2 set up for Ranunculus, and put back: `install`, `uninstall`, `boot`, `status`, `supervise`.
//!
//! Yandex's software stays as it is on its read-only system. What must not run is switched off by small files of
//! ours in /data, mounted over the firmware's at every boot (without our boot hook, a reboot is the stock station):
//! - launch.json: Yandex's launcher starts only `maind --platformd` — the audio front end (echo cancellation, the
//!   volume, the panel's driver), a process Alice shares; Yandex's player, telemetry and log shipping do not start;
//! - the spotters' flags.txt: the activation words, the commands heard without a wake word («пауза», «громче»…)
//!   and the interruption get thresholds no sound reaches, so Alice neither wakes nor reacts;
//! - the firmware's own flag that makes it leave the buttons alone (the client takes them).
//!
//! Each is made here, on the station, from the firmware's own files: none is shipped with Ranunculus.
//! The client is kept running by a supervisor (`boot` starts it): a crash is a restart, not silence.
use std::ffi::CString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where everything is: the real places on a station, or a folder standing in for its root (tests).
pub struct Station {
    root: PathBuf,
}

/// A threshold a spotter never reaches (its scores are 0…1).
const NEVER: &str = "1.5";
/// A Station 2 as its device tree names it, and as its firmware names the host.
const MODEL: &str = "Yandex Station Midi";
const HOSTNAME: &str = "Yandex-Station-gen2";
/// the firmware this was made and tried on (/etc/os-release BUILD_ID)
const KNOWN_BUILD: &str = "4778263334";
const HOOK: &str =
    "#!/bin/sh\n# Ranunculus: Alice switched off and the client started before Yandex's launcher (S99quasar).\n\
# Put there by `ranunculus-station install`; `ranunculus-station uninstall` removes it.\n\
[ \"$1\" = start ] && [ -x /data/ranunculus/ranunculus-station ] && /data/ranunculus/ranunculus-station boot\nexit 0\n";
/// the log in RAM is cut once it is this long
const LOG_MAX: u64 = 4 << 20;

/// Yandex's launch.json with only the tasks that run `--platformd`.
pub fn trimmed_launch(stock: &str) -> Result<String, String> {
    let mut v: serde_json::Value = serde_json::from_str(stock).map_err(|e| format!("launch.json: {e}"))?;
    let mut kept = 0;
    for node in v["nodes"].as_array_mut().ok_or("launch.json: no nodes")? {
        if let Some(tasks) = node["tasks"].as_array_mut() {
            tasks.retain(|t| t["arg"].as_array().is_some_and(|a| a.iter().any(|x| x == "--platformd")));
            kept += tasks.len();
        }
    }
    if kept != 1 {
        return Err(format!("launch.json: {kept} tasks run --platformd, expected one"));
    }
    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
}

/// A spotter's flags.txt with every threshold out of reach; None: it has none (not a spotter that reacts).
pub fn off_flags(flags: &str) -> Option<String> {
    let mut found = false;
    let lines: Vec<String> = flags
        .lines()
        .map(|line| match line.split_once(' ') {
            Some((key @ ("thresholds" | "subthresholds"), values)) => {
                found |= key == "thresholds";
                format!("{key} {}", values.trim().split(':').map(|_| NEVER).collect::<Vec<_>>().join(":"))
            }
            _ => line.to_string(),
        })
        .collect();
    found.then(|| lines.join("\n") + "\n")
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, text: &str, mode: u32) -> Result<(), String> {
    let e = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(e)?;
    }
    std::fs::write(path, text).map_err(e)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(e)
}

fn c(path: &Path) -> CString {
    CString::new(path.as_os_str().as_encoded_bytes()).expect("a path without NUL")
}

impl Station {
    pub fn real() -> Station {
        Station { root: PathBuf::from("/") }
    }

    fn at(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    fn home(&self) -> PathBuf {
        self.at("/data/ranunculus")
    }

    fn quasar(&self) -> PathBuf {
        self.at("/system/vendor/quasar")
    }

    fn overlay(&self) -> PathBuf {
        self.home().join("overlay")
    }

    fn hook(&self) -> PathBuf {
        self.at("/etc/init.d/S98ranunculus")
    }

    fn button_flag(&self) -> PathBuf {
        self.at("/data/quasar/data/button_click_prohibited")
    }

    fn run_dir(&self) -> PathBuf {
        self.at("/tmp/ranunculus")
    }

    /// The firmware's files that get one of ours over them: (in the firmware, relative to it).
    fn targets(&self) -> Result<Vec<(PathBuf, PathBuf)>, String> {
        let quasar = self.quasar();
        let mut out = vec![(quasar.join("launch.json"), PathBuf::from("launch.json"))];
        // a spotter is a folder with a flags.txt, one or two levels down (activation/alisa, command)
        let models = quasar.join("spotter_models");
        let mut folders = vec![];
        for one in std::fs::read_dir(&models).map_err(|e| format!("{}: {e}", models.display()))?.flatten() {
            folders.push(one.path());
            folders.extend(std::fs::read_dir(one.path()).into_iter().flatten().flatten().map(|d| d.path()));
        }
        folders.sort();
        for folder in folders {
            let flags = folder.join("flags.txt");
            if flags.is_file() {
                out.push((flags.clone(), flags.strip_prefix(&quasar).expect("under the firmware").to_path_buf()));
            }
        }
        Ok(out)
    }

    /// Our files and the firmware's they go over, as `install` left them in the overlay folder.
    fn overlays(&self) -> Vec<(PathBuf, PathBuf)> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                match e.path() {
                    p if p.is_dir() => walk(&p, out),
                    p => out.push(p),
                }
            }
        }
        let (overlay, mut files) = (self.overlay(), vec![]);
        walk(&overlay, &mut files);
        files.sort();
        files
            .into_iter()
            .map(|f| (f.clone(), self.quasar().join(f.strip_prefix(&overlay).expect("under the overlay"))))
            .collect()
    }

    fn mounted(&self, target: &Path) -> bool {
        read(&self.at("/proc/mounts")).is_ok_and(|m| m.lines().any(|l| l.split(' ').nth(1) == target.to_str()))
    }

    /// Something of another setup mounted over a file of the firmware, or over a folder it is in.
    fn covered_by_another(&self, target: &Path) -> bool {
        let (quasar, ours) = (self.quasar(), self.overlays());
        read(&self.at("/proc/mounts")).is_ok_and(|m| {
            m.lines().filter_map(|l| l.split(' ').nth(1)).map(Path::new).any(|at| {
                at.starts_with(&quasar) && at != quasar && target.starts_with(at) && !ours.iter().any(|(_, t)| t == at)
            })
        })
    }

    fn unmount(&self, target: &Path) -> Result<(), String> {
        for _ in 0..8 {
            if !self.mounted(target) {
                return Ok(());
            }
            // SAFETY: a NUL-terminated path; MNT_DETACH lets go of a file a running process still holds
            if unsafe { libc::umount2(c(target).as_ptr(), libc::MNT_DETACH) } != 0 {
                return Err(format!("unmount {}: {}", target.display(), std::io::Error::last_os_error()));
            }
        }
        Err(format!("{}: still mounted over", target.display()))
    }

    /// The root file system writable while `f` runs, then as it was (the firmware keeps it read-only).
    fn with_writable_root<T>(&self, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let read_only = read(&self.at("/proc/mounts")).is_ok_and(|m| {
            m.lines()
                .rfind(|l| l.split(' ').nth(1) == Some("/"))
                .is_some_and(|l| l.split(' ').nth(3).is_some_and(|o| o.split(',').any(|x| x == "ro")))
        });
        let remount = |flags: libc::c_ulong, how: &str| -> Result<(), String> {
            // SAFETY: NUL-terminated strings; a remount takes no source or type
            match unsafe {
                libc::mount(
                    std::ptr::null(),
                    c(Path::new("/")).as_ptr(),
                    std::ptr::null(),
                    libc::MS_REMOUNT | flags,
                    std::ptr::null(),
                )
            } {
                0 => Ok(()),
                _ => Err(format!("the root file system {how}: {}", std::io::Error::last_os_error())),
            }
        };
        if read_only {
            remount(0, "does not become writable")?;
        }
        let done = f();
        if read_only {
            remount(libc::MS_RDONLY, "does not go back to read-only")?;
        }
        done
    }

    /// Everything a station needs, made anew from its firmware (safe to run again). The client's own files — this
    /// program and station.toml — are put into /data/ranunculus beforehand (station/install.sh does).
    pub fn install(&self) -> Result<(), String> {
        let model = read(&self.at("/proc/device-tree/model")).unwrap_or_default();
        let model = model.trim_end_matches('\0').trim();
        if model != MODEL && read(&self.at("/etc/hostname")).unwrap_or_default().trim() != HOSTNAME {
            return Err(format!("this is «{model}», not a {MODEL} (Yandex Station 2): nothing done"));
        }
        let build = read(&self.at("/etc/os-release"))
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("BUILD_ID=").map(String::from))
            .unwrap_or_default();
        if build != KNOWN_BUILD {
            println!("note: the firmware's build is «{build}»; this was tried on {KNOWN_BUILD}");
        }
        crate::config::Config::load(self.home().join("station.toml").to_str().unwrap_or_default())?;
        let targets = self.targets()?;
        // the firmware's own files are read: ours from an earlier install come off first; anyone else's stop us
        for (target, _) in &targets {
            if self.covered_by_another(target) {
                return Err(format!(
                    "{}: something else is mounted over it — undo that setup first (a reboot without its hook)",
                    target.display()
                ));
            }
            self.unmount(target)?;
        }
        let overlay = self.overlay();
        let _ = std::fs::remove_dir_all(&overlay);
        let mut spotters = 0;
        for (target, relative) in &targets {
            let ours = if relative == Path::new("launch.json") {
                Some(trimmed_launch(&read(target)?)?)
            } else {
                off_flags(&read(target)?).inspect(|_| spotters += 1)
            };
            if let Some(text) = ours {
                write(&overlay.join(relative), &text, 0o644)?;
            }
        }
        if spotters == 0 {
            return Err(format!(
                "{}: no spotter found in this firmware — nothing done",
                self.quasar().join("spotter_models").display()
            ));
        }
        println!("Alice's launcher: only the audio front end will start");
        println!("Alice's spotters switched off: {spotters}");
        let flag = self.button_flag();
        if flag.parent().is_some_and(Path::is_dir) && !flag.exists() {
            write(&flag, "", 0o644)?;
        }
        let hook = self.hook();
        if read(&hook).ok().as_deref() != Some(HOOK) {
            self.with_writable_root(|| write(&hook, HOOK, 0o755))?;
        }
        println!("autostart: {}", hook.display());
        println!("installed: reboot the station to start");
        Ok(())
    }

    /// At boot, before Yandex's launcher: our files over the firmware's, then the client under its supervisor.
    pub fn boot(&self) -> Result<(), String> {
        for (ours, theirs) in self.overlays() {
            if theirs.exists() && !self.mounted(&theirs) {
                // SAFETY: NUL-terminated paths; a bind mount takes no type or options
                if unsafe {
                    libc::mount(
                        c(&ours).as_ptr(),
                        c(&theirs).as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND,
                        std::ptr::null(),
                    )
                } != 0
                {
                    return Err(format!("mount over {}: {}", theirs.display(), std::io::Error::last_os_error()));
                }
            }
        }
        self.start()
    }

    fn pid(&self, name: &str) -> Option<i32> {
        let pid: i32 = read(&self.run_dir().join(name)).ok()?.trim().parse().ok()?;
        // SAFETY: signal 0 only asks whether the process is there
        (unsafe { libc::kill(pid, 0) } == 0).then_some(pid)
    }

    /// The supervisor in the background, unless it runs: its own session, the log in RAM (no flash wear).
    pub fn start(&self) -> Result<(), String> {
        if self.pid("supervisor.pid").is_some() {
            return Ok(());
        }
        let dir = self.run_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let log = dir.join("station.log");
        let _ = std::fs::rename(&log, dir.join("station.log.1"));
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|e| format!("{}: {e}", log.display()))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cmd = std::process::Command::new(me);
        cmd.arg("supervise").current_dir(self.home()).stdin(std::process::Stdio::null()).stdout(out).stderr(err);
        // SAFETY: setsid is async-signal-safe; the child leaves the caller's session (an SSH login, the boot script)
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().map(|_| ()).map_err(|e| format!("the supervisor did not start: {e}"))
    }

    /// The supervisor and the client stopped (the client blanks the panel on its way out).
    pub fn stop(&self) {
        let Some(pid) = self.pid("supervisor.pid") else { return };
        // SAFETY: a signal to a process of ours
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let t = Instant::now();
        while self.pid("supervisor.pid").is_some() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Runs the client and starts it again when it ends, a little later each time it ends at once.
    pub fn supervise(&self) -> i32 {
        let dir = self.run_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("supervisor.pid"), std::process::id().to_string());
        let log = dir.join("station.log");
        let me = std::env::current_exe().expect("this program's path");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a tokio runtime");
        rt.block_on(async {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = signal(SignalKind::terminate()).expect("a signal handler");
            let mut wait = 1;
            loop {
                let started = Instant::now();
                let mut child = match tokio::process::Command::new(&me).arg("run").spawn() {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("{} supervisor: the client did not start: {e}", crate::stamp());
                        return 1;
                    }
                };
                let mut tick = tokio::time::interval(Duration::from_secs(600));
                let ended = loop {
                    tokio::select! {
                        status = child.wait() => break Some(status),
                        _ = term.recv() => break None,
                        _ = tick.tick() => {
                            // the log lives in RAM: its older half goes once it is long
                            if std::fs::metadata(&log).is_ok_and(|m| m.len() > LOG_MAX) {
                                let _ = std::fs::copy(&log, dir.join("station.log.1"));
                                let _ = std::fs::OpenOptions::new().write(true).truncate(true).open(&log);
                            }
                        }
                    }
                };
                let Some(status) = ended else {
                    if let Some(pid) = child.id() {
                        // SAFETY: a signal to our own child
                        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                    let _ = std::fs::remove_file(dir.join("supervisor.pid"));
                    return 0;
                };
                if started.elapsed() > Duration::from_secs(60) {
                    wait = 1;
                }
                eprintln!(
                    "{} supervisor: the client ended ({}), again in {wait} s",
                    crate::stamp(),
                    status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string())
                );
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                    _ = term.recv() => {
                        let _ = std::fs::remove_file(dir.join("supervisor.pid"));
                        return 0;
                    }
                }
                wait = (wait * 2).min(30);
            }
        })
    }

    /// The station as it was before `install`: after the next reboot Alice is back. The client's folder goes too.
    pub fn uninstall(&self) -> Result<(), String> {
        self.stop();
        for (_, theirs) in self.overlays() {
            self.unmount(&theirs)?;
        }
        let hook = self.hook();
        if hook.exists() {
            self.with_writable_root(|| std::fs::remove_file(&hook).map_err(|e| format!("{}: {e}", hook.display())))?;
        }
        let _ = std::fs::remove_file(self.button_flag());
        std::fs::remove_dir_all(self.home()).map_err(|e| format!("{}: {e}", self.home().display()))?;
        println!("removed: reboot the station, and it is Alice's again");
        Ok(())
    }

    /// What is in place, line by line. → whether all of it is.
    pub fn status(&self) -> bool {
        let overlays = self.overlays();
        let over = overlays.iter().filter(|(_, theirs)| self.mounted(theirs)).count();
        let lines = [
            (self.hook().exists(), "autostart".to_string()),
            (
                !overlays.is_empty() && over == overlays.len(),
                format!("Alice switched off ({over} of {} files in place)", overlays.len()),
            ),
            (self.pid("supervisor.pid").is_some(), "the client's supervisor".to_string()),
        ];
        for (ok, what) in &lines {
            println!("{} {what}", if *ok { "ok  " } else { "NO  " });
        }
        lines.iter().all(|(ok, _)| *ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A launch.json of the launcher's shape (made up: not Yandex's file).
    const LAUNCH: &str = r#"{"nodes": [{"name": "main", "root": ".", "tasks": [
        {"name": "player", "path": "/system/vendor/quasar/maind", "arg": ["--mediad"], "order": 3},
        {"name": "maind", "path": "/system/vendor/quasar/maind", "arg": ["--platformd"], "envs": ["A=1"], "order": 1},
        {"name": "logs", "path": "/system/vendor/quasar/maind", "arg": ["--fluent-bitd"], "order": 0}]}]}"#;

    #[test]
    fn the_launcher_keeps_the_audio_front_end_only() {
        let v: serde_json::Value = serde_json::from_str(&trimmed_launch(LAUNCH).unwrap()).unwrap();
        let tasks = v["nodes"][0]["tasks"].as_array().unwrap();
        assert_eq!(
            (tasks.len(), tasks[0]["name"].as_str(), tasks[0]["envs"][0].as_str()),
            (1, Some("maind"), Some("A=1"))
        );
        assert_eq!(v["nodes"][0]["name"], "main", "the rest of the file as it was");
        assert!(
            trimmed_launch(r#"{"nodes": [{"tasks": [{"arg": ["--mediad"]}]}]}"#).is_err(),
            "no audio front end: not a launch.json we know"
        );
        assert!(trimmed_launch("not json").is_err());
    }

    #[test]
    fn spotters_get_thresholds_out_of_reach() {
        let flags =
            "am-type nnet\ncommands 1:2:3\nsubthresholds 0.4:0.2:0.03\nthresholds 0.45:0.25:0.03\nversion some-model\n";
        assert_eq!(
            off_flags(flags).unwrap(),
            "am-type nnet\ncommands 1:2:3\nsubthresholds 1.5:1.5:1.5\nthresholds 1.5:1.5:1.5\nversion some-model\n"
        );
        assert_eq!(off_flags("commands 1\nthresholds 0.84\n").unwrap(), "commands 1\nthresholds 1.5\n");
        // the voice detector has a `threshold` of another kind: it is no spotter, and stays as it is
        assert!(off_flags("spotter-type seam-vad-decoder\nthreshold 0.9\n").is_none());
    }

    /// A folder laid out as a station's root, with made-up firmware files.
    fn station(name: &str) -> Station {
        let root = std::env::temp_dir().join(format!("station-setup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Station { root };
        write(&s.at("/proc/device-tree/model"), "Yandex Station Midi\0", 0o644).unwrap();
        write(&s.at("/proc/mounts"), "ubi0:rootfs / ubifs rw,relatime 0 0\n", 0o644).unwrap();
        write(&s.quasar().join("launch.json"), LAUNCH, 0o644).unwrap();
        for (model, flags) in [
            ("activation/alisa", "commands 1\nsubthresholds 0.4\nthresholds 0.84\n"),
            ("command", "thresholds 0.4:0.2\n"),
            ("vad", "threshold 0.9\n"),
        ] {
            write(&s.quasar().join("spotter_models").join(model).join("flags.txt"), flags, 0o644).unwrap();
        }
        std::fs::create_dir_all(s.at("/data/quasar/data")).unwrap();
        std::fs::create_dir_all(s.at("/etc/init.d")).unwrap();
        write(&s.home().join("station.toml"), "server = \"wss://h:1/station\"\ntoken = \"0123456789abcdef0\"\n", 0o600)
            .unwrap();
        s
    }

    #[test]
    fn install_makes_everything_from_the_firmware_and_uninstall_takes_it_away() {
        let s = station("install");
        s.install().unwrap();
        let ours: Vec<String> = s
            .overlays()
            .iter()
            .map(|(o, _)| o.strip_prefix(s.overlay()).unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            ours,
            ["launch.json", "spotter_models/activation/alisa/flags.txt", "spotter_models/command/flags.txt"],
            "the voice detector is left alone"
        );
        assert_eq!(read(&s.overlay().join("spotter_models/command/flags.txt")).unwrap(), "thresholds 1.5:1.5\n");
        assert_eq!(
            read(&s.quasar().join("spotter_models/command/flags.txt")).unwrap(),
            "thresholds 0.4:0.2\n",
            "the firmware's own file is not touched"
        );
        assert!(s.button_flag().exists());
        assert_eq!(
            (read(&s.hook()).unwrap(), std::fs::metadata(s.hook()).unwrap().permissions().mode() & 0o777),
            (HOOK.to_string(), 0o755)
        );
        // again: the same, nothing doubled
        s.install().unwrap();
        assert_eq!(s.overlays().len(), 3);
        s.uninstall().unwrap();
        assert!(!s.hook().exists() && !s.button_flag().exists() && !s.home().exists());
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn install_refuses_what_it_does_not_know() {
        let s = station("refuse");
        write(&s.at("/proc/device-tree/model"), "Some Other Speaker\0", 0o644).unwrap();
        assert!(s.install().unwrap_err().contains("not a Yandex Station Midi"));
        write(&s.at("/proc/device-tree/model"), "Yandex Station Midi\0", 0o644).unwrap();
        // a file of someone else's setup mounted over the firmware's: left alone, said
        let launch = s.quasar().join("launch.json");
        write(
            &s.at("/proc/mounts"),
            &format!("ubi0:rootfs / ubifs rw 0 0\n/dev/ubi1_0 {} ubifs rw 0 0\n", launch.display()),
            0o644,
        )
        .unwrap();
        assert!(s.install().unwrap_err().contains("something else is mounted"));
        // …or over the folder of a spotter
        let folder = s.quasar().join("spotter_models/activation/alisa");
        write(
            &s.at("/proc/mounts"),
            &format!("ubi0:rootfs / ubifs rw 0 0\n/dev/ubi1_0 {} ubifs rw 0 0\n", folder.display()),
            0o644,
        )
        .unwrap();
        assert!(s.install().unwrap_err().contains("activation/alisa/flags.txt: something else is mounted"));
        assert!(!s.hook().exists() && !s.overlay().exists());
        // without its config the client could not start: said before anything is changed
        write(&s.at("/proc/mounts"), "ubi0:rootfs / ubifs rw 0 0\n", 0o644).unwrap();
        std::fs::remove_file(s.home().join("station.toml")).unwrap();
        assert!(s.install().is_err() && !s.hook().exists());
        let _ = std::fs::remove_dir_all(&s.root);
    }
}

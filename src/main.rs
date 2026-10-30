//! rnote-pad: Wacom tablet pad and pen buttons as Rnote shortcuts, and the
//! tablet mapped onto the Rnote window.
//!
//! The pad is grabbed exclusively, so Hyprland never sees it. That matters:
//! Hyprland 0.56 broadcasts pad buttons to every client, and GTK 4.24 crashes
//! in any client that isn't focused. Presses become key chords sent through
//! Hyprland, and only while a window of the configured class is focused, so
//! the pad does nothing anywhere else. The pen's side buttons are only read;
//! a libinput quirk keeps them from reaching apps.

mod config;
mod evdev;
mod follow;
mod hyprland;
mod pen;
mod ring;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::{mem, ptr};

use config::{Action, Config, Shortcut};
use follow::Follower;
use hyprland::Outcome;
use ring::{Dial, Ring};

/// Overlapping synthetic chords can leave keys stuck in Hyprland; keep them apart.
const MIN_GAP: Duration = Duration::from_millis(60);

const USAGE: &str =
    "usage: rnote-pad [-v|--verbose] [-c|--config FILE] [--send SHORTCUT | /dev/input/eventN]";

struct Args {
    verbose: bool,
    config: Option<PathBuf>,
    device: Option<PathBuf>,
    /// Send this once, as a pad press would, and exit.
    send: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        verbose: false,
        config: None,
        device: None,
        send: None,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("-v" | "--verbose") => args.verbose = true,
            Some("-c" | "--config") => {
                args.config = Some(it.next().ok_or("--config needs a file")?.into());
            }
            Some("--send") => {
                let shortcut = it.next().ok_or("--send needs a shortcut")?;
                args.send = Some(shortcut.into_string().map_err(|_| "--send: not UTF-8")?);
            }
            Some("-h" | "--help") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ if args.device.is_none() => args.device = Some(arg.into()),
            _ => return Err(USAGE.into()),
        }
    }
    Ok(args)
}

struct Sender {
    class: String,
    verbose: bool,
    last: Option<Instant>,
    /// The last Rnote tool picked through rnote-pad. Tools picked in Rnote
    /// itself go unnoticed.
    tool: Option<Shortcut>,
}

impl Sender {
    fn new(class: String, verbose: bool) -> Self {
        Self {
            class,
            verbose,
            last: None,
            tool: None,
        }
    }

    fn press(&mut self, action: &Action) {
        let shortcut = match action {
            Action::Send(shortcut) => shortcut,
            Action::Switch(first, second) if self.tool.as_ref() == Some(first) => second,
            Action::Switch(first, _) => first,
        };
        self.send(shortcut);
    }

    fn send(&mut self, shortcut: &Shortcut) {
        if let Some(wait) = self.last.and_then(|t| MIN_GAP.checked_sub(t.elapsed())) {
            thread::sleep(wait);
        }
        self.last = Some(Instant::now());
        match hyprland::send_if_focused(&self.class, shortcut) {
            Ok(outcome) => {
                if matches!(outcome, Outcome::Sent) && shortcut.picks_tool() {
                    self.tool = Some(shortcut.clone());
                }
                if self.verbose {
                    match outcome {
                        Outcome::Sent => eprintln!("  -> sent {shortcut}"),
                        Outcome::Skipped(focused) => {
                            eprintln!("  -> skipped {shortcut}, {focused} has focus")
                        }
                    }
                }
            }
            Err(e) => eprintln!("rnote-pad: sending {shortcut}: {e}"),
        }
    }

    /// Sends one of two shortcuts `steps` times, `clockwise` for positive steps.
    fn turn(
        &mut self,
        steps: i32,
        clockwise: &Option<Shortcut>,
        counterclockwise: &Option<Shortcut>,
    ) {
        let shortcut = if steps > 0 {
            clockwise
        } else {
            counterclockwise
        };
        if let Some(shortcut) = shortcut {
            for _ in 0..steps.unsigned_abs() {
                self.send(shortcut);
            }
        }
    }
}

/// Blocks SIGTERM and SIGINT in this thread and every thread spawned after
/// it, so they can be taken with sigwait instead of a signal handler.
fn block_exit_signals() -> libc::sigset_t {
    // SAFETY: plain libc signal-set calls on a local set.
    unsafe {
        let mut set: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, ptr::null_mut());
        set
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    let mut config = match Config::load(args.config.as_deref()) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("rnote-pad: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(text) = args.send {
        let shortcut = match Shortcut::try_from(text) {
            Ok(shortcut) => shortcut,
            Err(e) => {
                eprintln!("rnote-pad: {e}");
                return ExitCode::FAILURE;
            }
        };
        Sender::new(config.window_class, true).send(&shortcut);
        return ExitCode::SUCCESS;
    }
    let Some(path) = args.device.or_else(|| evdev::find_wacom("Pad")) else {
        // Not an error: udev starts the service again when a pad connects.
        eprintln!("rnote-pad: no Wacom pad connected");
        return ExitCode::SUCCESS;
    };
    let mut pad = match evdev::Device::open(&path).and_then(|pad| pad.grab().map(|()| pad)) {
        Ok(pad) => pad,
        Err(e) => {
            eprintln!("rnote-pad: cannot grab {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    // Intuos Pro 3rd gen: ExpressKeys round a dial, and a Pro Pen 3.
    let has_dial = pad.has_rel(evdev::REL_WHEEL).unwrap_or(false);
    eprintln!(
        "rnote-pad: grabbed {} (a pad with a {}), config from {}, shortcuts go to {}",
        path.display(),
        if has_dial { "dial" } else { "touch ring" },
        config.source,
        config.window_class
    );

    let follower = Arc::new(Mutex::new(Follower::new(config.window_class.clone())));
    let signals = block_exit_signals();
    {
        let follower = follower.clone();
        thread::spawn(move || {
            let mut signal = 0;
            // SAFETY: waits on the set blocked above.
            unsafe { libc::sigwait(&signals, &mut signal) };
            follower.lock().unwrap().release();
            std::process::exit(0);
        });
    }
    let sender = Arc::new(Mutex::new(Sender::new(
        config.window_class.clone(),
        args.verbose,
    )));
    if config.follow_window {
        let events = follower.clone();
        thread::spawn(move || follow::watch_hyprland(events));
    }
    let following = config.follow_window.then(|| follower.clone());
    // The [pen] section is for the Pro Pen 3; the 2nd gen's pen has a real eraser end.
    let pen_buttons = has_dial.then(|| pen::Buttons {
        config: mem::take(&mut config.pen),
        sender: sender.clone(),
    });
    if following.is_some() || pen_buttons.is_some() {
        thread::spawn(move || pen::watch(following, pen_buttons));
    }

    let mut ring = Ring::new(config.ring.step);
    let mut dial = Dial::new(config.dial.step);
    let result = loop {
        let ev = match pad.next_event() {
            Ok(ev) => ev,
            Err(e) if e.raw_os_error() == Some(libc::ENODEV) => {
                eprintln!("rnote-pad: pad disconnected");
                break ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("rnote-pad: reading {}: {e}", path.display());
                break ExitCode::FAILURE;
            }
        };
        if args.verbose && ev.kind != evdev::EV_SYN {
            eprintln!(
                "event type={:#x} code={:#x} value={}",
                ev.kind, ev.code, ev.value
            );
        }
        match (ev.kind, ev.code) {
            (evdev::EV_KEY, code) if ev.value == 1 => {
                let shortcut = code.checked_sub(evdev::BTN_0).and_then(|b| {
                    if has_dial {
                        config.dial.button(b)
                    } else {
                        config.buttons.get(b)
                    }
                });
                if let Some(shortcut) = shortcut {
                    sender.lock().unwrap().send(shortcut);
                }
            }
            (evdev::EV_ABS, evdev::ABS_WHEEL) => {
                let steps = ring.update(ev.value);
                sender.lock().unwrap().turn(
                    steps,
                    &config.ring.clockwise,
                    &config.ring.counterclockwise,
                );
            }
            (evdev::EV_REL, evdev::REL_WHEEL) => {
                let steps = dial.update(ev.value);
                sender.lock().unwrap().turn(
                    steps,
                    &config.dial.clockwise,
                    &config.dial.counterclockwise,
                );
            }
            // Nothing on the pad is touched any more.
            (evdev::EV_ABS, evdev::ABS_MISC) if ev.value == 0 => ring.release(),
            _ => {}
        }
    };
    follower.lock().unwrap().release();
    result
}

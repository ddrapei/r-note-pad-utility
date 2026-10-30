//! The pen, read but never grabbed: picking it up brings Rnote into view and
//! maps the tablet onto it (see follow.rs), and its side buttons become
//! shortcuts.

use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::Sender;
use crate::config::PenConfig;
use crate::evdev;
use crate::follow::Follower;

/// "Pen in hand": BTN_TOOL_PEN, BTN_TOOL_RUBBER (eraser end) up to BTN_TOOL_AIRBRUSH.
const PEN_TOOLS: RangeInclusive<u16> = 0x140..=0x144;

pub struct Buttons {
    pub config: PenConfig,
    pub sender: Arc<Mutex<Sender>>,
}

/// Returns if the pen can't be read or disconnects.
pub fn watch(mut follower: Option<Arc<Mutex<Follower>>>, buttons: Option<Buttons>) {
    // udev may create the pen's node a moment after the pad's.
    let path = (0..10).find_map(|attempt| {
        if attempt > 0 {
            thread::sleep(Duration::from_secs(1));
        }
        evdev::find_wacom("Pen")
    });
    let Some(path) = path else {
        eprintln!(
            "rnote-pad: no Wacom pen found, so the tablet won't follow Rnote and pen buttons do nothing"
        );
        return;
    };
    let mut pen = match evdev::Device::open(&path) {
        Ok(pen) => pen,
        Err(e) => {
            eprintln!(
                "rnote-pad: cannot read {}: {e} (is 70-rnote-pad.rules up to date?)",
                path.display()
            );
            return;
        }
    };
    if let Some(following) = &follower {
        match pen.size_mm() {
            Ok(size) => following.lock().unwrap().start(size),
            Err(e) => {
                eprintln!(
                    "rnote-pad: {}: {e}, so the tablet won't follow Rnote",
                    path.display()
                );
                follower = None;
            }
        }
    }
    if let Err(e) = pen.only_keys(PEN_TOOLS.chain([evdev::BTN_STYLUS])) {
        // Still works, it just wakes up for every pen movement.
        eprintln!("rnote-pad: filtering pen events: {e}");
    }
    while let Ok(ev) = pen.next_event() {
        if ev.kind != evdev::EV_KEY || ev.value != 1 {
            continue;
        }
        let button = match (&buttons, ev.code) {
            (Some(buttons), evdev::BTN_STYLUS) => Some((buttons, &buttons.config.lower)),
            // Not a real eraser end: the Pro Pen 3 has none.
            (Some(buttons), evdev::BTN_TOOL_RUBBER) => Some((buttons, &buttons.config.upper)),
            _ => None,
        };
        if let Some((buttons, action)) = button {
            if let Some(action) = action {
                buttons.sender.lock().unwrap().press(action);
            }
        } else if PEN_TOOLS.contains(&ev.code)
            && let Some(follower) = &follower
        {
            follower.lock().unwrap().refresh(true);
        }
    }
}

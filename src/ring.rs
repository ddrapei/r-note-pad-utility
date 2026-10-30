//! Touch ring and dial to steps. The ring reports an absolute finger
//! position, 0-71 growing clockwise, and 0 once the finger lifts; turning it
//! means accumulating the deltas between reports. The dial simply reports
//! clicks.

const POSITIONS: i32 = 72;
/// A bigger jump between reports means the finger was put down somewhere else.
const MAX_JUMP: i32 = 12;

pub struct Ring {
    step: i32,
    last: Option<i32>,
    acc: i32,
}

impl Ring {
    /// `step`: ring positions per shortcut.
    pub fn new(step: u8) -> Self {
        Self {
            step: step.into(),
            last: None,
            acc: 0,
        }
    }

    /// Feeds one ABS_WHEEL value; returns how many shortcuts to send,
    /// positive for clockwise.
    pub fn update(&mut self, value: i32) -> i32 {
        // 0 means "lifted" as well as a real position that turns cross, so
        // skip it; lifting is detected via ABS_MISC instead (see release).
        if value == 0 {
            return 0;
        }
        let Some(last) = self.last.replace(value) else {
            return 0;
        };
        let mut delta = (value - last).rem_euclid(POSITIONS);
        if delta > POSITIONS / 2 {
            delta -= POSITIONS;
        }
        if delta.abs() > MAX_JUMP {
            self.acc = 0;
            return 0;
        }
        self.acc += delta;
        let steps = self.acc / self.step;
        self.acc -= steps * self.step;
        steps
    }

    pub fn release(&mut self) {
        self.last = None;
        self.acc = 0;
    }
}

pub struct Dial {
    step: i32,
    acc: i32,
}

impl Dial {
    /// `step`: dial clicks per shortcut.
    pub fn new(step: u8) -> Self {
        Self {
            step: step.into(),
            acc: 0,
        }
    }

    /// Feeds one REL_WHEEL value; returns how many shortcuts to send,
    /// positive for clockwise.
    pub fn update(&mut self, value: i32) -> i32 {
        // Clockwise clicks arrive as -1 (measured on the Intuos Pro S, 3rd gen).
        self.acc -= value;
        let steps = self.acc / self.step;
        self.acc -= steps * self.step;
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dial_clicks() {
        let mut dial = Dial::new(1);
        assert_eq!(dial.update(-1), 1);
        assert_eq!(dial.update(2), -2);
        let mut dial = Dial::new(2);
        assert_eq!([-1, -1, -1].map(|v| dial.update(v)), [0, 1, 0]);
        // The click left over from turning clockwise is used up first.
        assert_eq!([1, 1, 1].map(|v| dial.update(v)), [0, 0, -1]);
    }

    fn feed(ring: &mut Ring, values: &[i32]) -> i32 {
        values.iter().map(|&v| ring.update(v)).sum()
    }

    #[test]
    fn clockwise_and_back() {
        let mut ring = Ring::new(4);
        assert_eq!(feed(&mut ring, &[10, 12, 14, 16, 18]), 2);
        assert_eq!(feed(&mut ring, &[14, 10]), -2);
    }

    #[test]
    fn wraps_past_the_top() {
        let mut ring = Ring::new(4);
        assert_eq!(feed(&mut ring, &[68, 70, 1, 4]), 2);
    }

    #[test]
    fn touching_down_elsewhere_is_not_a_turn() {
        let mut ring = Ring::new(4);
        assert_eq!(feed(&mut ring, &[10, 40, 42, 44]), 1);
    }

    #[test]
    fn crossing_zero_keeps_counting() {
        // Recorded from the tablet: a counter-clockwise swipe through the top.
        let mut ring = Ring::new(4);
        assert_eq!(feed(&mut ring, &[4, 3, 1, 0, 71, 68, 66]), -2);
    }

    #[test]
    fn lifting_resets() {
        let mut ring = Ring::new(4);
        assert_eq!(feed(&mut ring, &[10, 13, 0]), 0);
        ring.release();
        assert_eq!(feed(&mut ring, &[14, 15]), 0);
    }
}

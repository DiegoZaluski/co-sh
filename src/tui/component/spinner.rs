pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Braille spinner: the frame table is shared module-wide, while
/// `frame`/`tick_counter` hold per-owner animation state. `Clone`/`Debug`
/// so it can live inside the cloned/loggable dialog instances.
#[derive(Debug, Clone)]
pub struct SpinnerState {
    pub frame: usize,
    pub tick_counter: u32,
}

impl SpinnerState {
    pub const fn new() -> Self {
        Self {
            frame: 0,
            tick_counter: 0,
        }
    }

    pub const fn advance(&mut self) {
        self.tick_counter += 1;
        if self.tick_counter >= 2 {
            self.tick_counter = 0;
            self.frame = (self.frame + 1) % SPINNER_FRAMES.len();
        }
    }

    pub fn current_char(&self) -> char {
        SPINNER_FRAMES[self.frame]
    }
}

impl Default for SpinnerState {
    fn default() -> Self {
        Self::new()
    }
}

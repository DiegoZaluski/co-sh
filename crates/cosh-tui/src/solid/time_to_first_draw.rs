pub struct TimeToFirstDrawRenderable;

impl TimeToFirstDrawRenderable {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for TimeToFirstDrawRenderable {
    fn default() -> Self {
        Self::new()
    }
}

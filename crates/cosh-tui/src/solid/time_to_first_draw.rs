pub struct TimeToFirstDrawRenderable;

impl TimeToFirstDrawRenderable {
    #[must_use]
    pub fn new() -> Self {
        TimeToFirstDrawRenderable
    }
}

impl Default for TimeToFirstDrawRenderable {
    fn default() -> Self {
        Self::new()
    }
}

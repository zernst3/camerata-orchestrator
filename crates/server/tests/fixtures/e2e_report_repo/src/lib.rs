// Innocuous file: no deterministic-floor violation here, so the e2e test can
// assert that a clean file contributes zero findings alongside the three
// deliberately-planted violations in the sibling files.
pub fn noop() {}

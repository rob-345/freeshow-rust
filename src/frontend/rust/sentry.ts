// Electron's renderer SDK requires an Electron preload/main process. Retain the
// shared frontend without importing that SDK into the Rust desktop bundle.
// Native crash reporting is pending and explicitly covered by the parity plan.
export function init(_options: unknown) {}

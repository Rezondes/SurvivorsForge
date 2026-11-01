//! Runs the scanner on a thread of its own, with the stack it needs (`SCAN_STACK_BYTES`). A panic
//! in it, say from a zip nobody thought of, ends that thread and comes back as an error: the API
//! and the worker go on.

use forge_scan::Report;

/// `work` on its own thread; `Err` when it panicked.
pub fn isolated<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    std::thread::Builder::new()
        .name("forge-scan".into())
        .stack_size(forge_scan::SCAN_STACK_BYTES)
        .spawn(work)
        .map_err(|e| format!("cannot start the scanner: {e}"))?
        .join()
        .map_err(|_| "the scanner crashed on this zip".to_string())
}

/// `forge_scan::scan`, isolated.
pub fn run_scan(zip: impl AsRef<[u8]> + Send + 'static, own: Vec<String>, foreign: Vec<String>) -> Result<Report, String> {
    isolated(move || forge_scan::scan(zip.as_ref(), &own, &foreign))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_comes_back_as_an_error() {
        let result: Result<(), String> = isolated(|| panic!("boom"));
        assert_eq!(result, Err("the scanner crashed on this zip".to_string()));
        assert_eq!(isolated(|| 41 + 1), Ok(42));
    }
}

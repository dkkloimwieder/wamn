// Allocates toward a target (default 512 MiB, past the 256 MiB engine
// ceiling) in 8 MiB steps, touching every page so linear memory actually
// grows. Under a correctly enforced cap this traps mid-loop; it must never
// print the final line. The memory-budget bench phase drives it with:
//   MEMHOG_TARGET_MIB   — override the target
//   MEMHOG_REPORT_PATH  — file (in a mounted volume) rewritten with the
//                         running total after every step; the last value on
//                         the host is the high-water the enforcement allowed
fn main() {
    const STEP: usize = 8 * 1024 * 1024;
    let target: usize = std::env::var("MEMHOG_TARGET_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
        * 1024
        * 1024;
    let report = std::env::var("MEMHOG_REPORT_PATH").ok();
    let mut hoard: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;
    while total < target {
        let mut chunk = vec![0u8; STEP];
        for i in (0..chunk.len()).step_by(4096) {
            chunk[i] = 1;
        }
        hoard.push(chunk);
        total += STEP;
        // stderr: the host's default WasiCtx inherits stderr but not stdout.
        eprintln!("memhog: {} MiB", total / (1024 * 1024));
        if let Some(path) = &report {
            let _ = std::fs::write(path, format!("{}", total / (1024 * 1024)));
        }
    }
    eprintln!(
        "memhog: reached {} MiB without being killed (CAP FAILED)",
        total / (1024 * 1024)
    );
}

// Allocates past the 256 MiB per-component cap in 8 MiB steps, touching
// every page so linear memory actually grows. Under a correctly enforced
// cap this traps mid-loop; it must never print the final line.
fn main() {
    const STEP: usize = 8 * 1024 * 1024;
    const TARGET: usize = 512 * 1024 * 1024;
    let mut hoard: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;
    while total < TARGET {
        let mut chunk = vec![0u8; STEP];
        for i in (0..chunk.len()).step_by(4096) {
            chunk[i] = 1;
        }
        hoard.push(chunk);
        total += STEP;
        // stderr: the host's default WasiCtx inherits stderr but not stdout.
        eprintln!("memhog: {} MiB", total / (1024 * 1024));
    }
    eprintln!("memhog: reached {} MiB without being killed (CAP FAILED)", total / (1024 * 1024));
}

//! Epoch-kill target: spins forever doing side-effect-free work. The only
//! way this component stops is an external interrupt — trapping it via an
//! epoch deadline is exactly the wamn-4p3 acceptance demo.

fn main() {
    // Host WasiCtx inherits stderr only.
    eprintln!("busyloop: entering infinite loop");
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    loop {
        // splitmix64 permutation: cheap, unoptimizable-away via black_box,
        // and the loop backedge carries wasmtime's epoch check.
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        std::hint::black_box(z ^ (z >> 31));
    }
}

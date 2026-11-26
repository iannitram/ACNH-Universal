//! Time the recompile stage on a synthetic module (no game data): generated AArch64 functions
//! with a fixed instruction mix, analysed and emitted on one thread.
//!
//! usage: cargo run --release -p acnh-recomp --example recomp-bench -- [functions] [repeats]

use acnh_recomp::{analyse::analyse, emit::Emitter, Module};
use std::time::Instant;

const BODY: &[u32] = &[
    0xD10083FF, // sub sp, sp, #0x20
    0xA9017BFD, // stp x29, x30, [sp, #0x10]
    0x910043FD, // add x29, sp, #0x10
    0xF9400408, // ldr x8, [x0, #8]
    0x8B010109, // add x9, x8, x1
    0xF100153F, // cmp x9, #5
    0x54000040, // b.eq +8
    0xB9000409, // str w9, [x0, #4]
    0xD2800000, // mov x0, #0
    0x1E612800, // fadd d0, d0, d1
    0x3DC00000, // ldr q0, [x0]
    0xF9400808, // ldr x8, [x0, #0x10]
    0x9B087D29, // mul x9, x9, x8
    0xA9417BFD, // ldp x29, x30, [sp, #0x10]
    0x910083FF, // add sp, sp, #0x20
];
const RET: u32 = 0xD65F03C0;

fn module(functions: usize) -> Module {
    let per = BODY.len() + 2;
    let mut words = Vec::with_capacity(functions * per);
    for k in 0..functions {
        words.extend_from_slice(BODY);
        // bl to the next function, so every function is an entry.
        let next = if k + 1 < functions { 2 } else { 1 };
        words.push(0x9400_0000 | next);
        words.push(RET);
    }
    let image: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let size = image.len() as u64;
    Module {
        name: "bench".into(),
        image,
        text: (0, size),
        ..Module::default()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let functions: usize = args.next().map_or(Ok(20_000), |a| a.parse())?;
    let repeats: usize = args.next().map_or(Ok(3), |a| a.parse())?;
    let module = module(functions);
    let insns = module.image.len() / 4;
    for run in 0..repeats {
        let t = Instant::now();
        let analysis = analyse(&module, &mut |_| {}, &[])?;
        let analysed = t.elapsed();
        let t = Instant::now();
        let mut emitter = Emitter::new(&module, &analysis, "bench")?;
        let mut bytes = 0usize;
        for f in &analysis.functions {
            bytes += emitter.function(f)?.len();
        }
        let emitted = t.elapsed();
        println!(
            "run {run}: {} functions, {insns} instructions: analyse {:.3}s, emit {:.3}s ({} handled, {bytes} bytes)",
            analysis.functions.len(),
            analysed.as_secs_f64(),
            emitted.as_secs_f64(),
            emitter.coverage.handled,
        );
    }
    Ok(())
}

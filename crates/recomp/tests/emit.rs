use acnh_recomp::{
    analyse::{Analysis, Function},
    emit::{Emitter, Error},
    Module,
};

fn function(start: u64, end: u64) -> Function {
    Function {
        start,
        end,
        fde: false,
        name: String::new(),
    }
}

#[test]
fn integer_aliases_and_memory_addresses() {
    let module = Module::default();
    let analysis = Analysis::default();
    let emitter = Emitter::new(&module, &analysis, "fixture").unwrap();
    for (word, expected) in [
        (0xaa1f03e0, "c.x[0] = uint64_t(0ull);"),
        (0x9100027f, "c.x[31] = uint64_t(c.x[19]);"),
        (
            0x8b34cbe2,
            "c.x[2] = uint64_t(uint64_t(c.x[31]) + uint64_t(uint64_t(int64_t(int32_t(uint32_t(c.x[20])))) << 2));",
        ),
        (
            0x389f9284,
            "{ uint64_t a = c.x[20] + 0xfffffffffffffff9ull; c.x[4] = uint64_t(uint64_t(int64_t(nx::ld<int8_t>(a)))); }",
        ),
        (
            0x28c11a85,
            "{ uint64_t a = c.x[20]; auto v0 = nx::ld<uint32_t>(a + 0); auto v1 = nx::ld<uint32_t>(a + 4); c.x[20] += 0x8ull; c.x[5] = uint32_t(v0); c.x[6] = uint32_t(v1); }",
        ),
        (0xd53b4204, "c.x[4] = uint64_t(nx::get_nzcv(c));"),
    ] {
        assert_eq!(
            emitter.instruction(&function(0, 4), 0, word).unwrap(),
            expected,
            "{word:#010x}"
        );
    }
    assert_eq!(emitter.instruction(&function(0, 4), 0, 0x1e622820).unwrap(), "{ double r_ = double((c.v[1].f64[0]) + (c.v[2].f64[0])); nx::V128 t_ = nx::vzero(); t_.f64[0] = r_; c.v[0] = t_; }");
}

#[test]
fn branches_track_loop_writes_and_synchronize_on_calls() {
    let words: [u32; 6] = [
        0x91000400, // add x0, x0, #1
        0x71000421, // subs w1, w1, #1
        0x54ffffc1, // b.ne 0
        0x94000002, // bl 20
        0xd65f03c0, // ret
        0xd65f03c0, // ret
    ];
    let module = Module {
        image: words.into_iter().flat_map(u32::to_le_bytes).collect(),
        ..Module::default()
    };
    let analysis = Analysis {
        functions: vec![function(0, 20), function(20, 24)],
        ..Analysis::default()
    };
    let mut emitter = Emitter::new(&module, &analysis, "fixture").unwrap();
    let output = emitter.function(&analysis.functions[0]).unwrap();
    assert!(output.contains("L_0:;\n"));
    assert!(output.contains("if (!fl_.zf) goto L_0;"));
    assert!(output.contains(
        "{ c.x[0] = x0; c.x[1] = x1; c.x[30] = x30; c.nf = fl_.nf; c.zf = fl_.zf; c.cf = fl_.cf; c.vf = fl_.vf; fixture_00000014(c);"
    ));
    assert!(output.contains("{ return; } // ret "));
    assert_eq!(emitter.coverage.handled, 5);

    emitter.options.cache.enabled = false;
    let uncached = emitter.function(&analysis.functions[0]).unwrap();
    assert!(uncached.contains("if (!c.zf) goto L_0;"));
    assert!(!uncached.contains("nx::Flags fl_"));
}

#[test]
fn invalid_function_ranges_return_errors() {
    let module = Module::default();
    let analysis = Analysis::default();
    let mut emitter = Emitter::new(&module, &analysis, "fixture").unwrap();
    assert!(matches!(
        emitter.function(&function(1, 4)),
        Err(Error::FunctionRange { start: 1, end: 4 })
    ));
}

/// 64-bit lanes (`.2d` and scalar `d`) have no wider native type, so saturating and rounding forms must go through
/// 128-bit intermediates; the Unicorn corpus in `tests/recomp_emit.py` checks the values, this pins the emitted shape.
#[test]
fn sixty_four_bit_lane_saturation_and_rounding_use_wide_arithmetic() {
    let module = Module::default();
    let analysis = Analysis::default();
    let emitter = Emitter::new(&module, &analysis, "fixture").unwrap();
    let emit = |word: u32| {
        emitter
            .instruction(&function(0, 4), 0, word)
            .unwrap_or_else(|e| panic!("{word:#010x}: {e:?}"))
    };
    for (word, fragment) in [
        // sqadd / sqsub, vector and scalar
        (0x4EE20C20, "nx::sat128<int64_t>(__int128(S0.s64[k]) + __int128(S1.s64[k]))"),
        (0x5EE20C20, "nx::sat128<int64_t>(__int128(S0.s64[k]) + __int128(S1.s64[k]))"),
        (0x4EE22C20, "nx::sat128<int64_t>(__int128(S0.s64[k]) - __int128(S1.s64[k]))"),
        (0x5EE22C20, "nx::sat128<int64_t>(__int128(S0.s64[k]) - __int128(S1.s64[k]))"),
        // uqadd saturates on unsigned wrap
        (0x6EE20C20, "S0.u64[k] + S1.u64[k] < S0.u64[k] ? ~0ull"),
        (0x7EE20C20, "S0.u64[k] + S1.u64[k] < S0.u64[k] ? ~0ull"),
        // sqabs / sqneg
        (0x4EE07820, "nx::sat128<int64_t>(S.s64[k] < 0 ? -__int128(S.s64[k]) : __int128(S.s64[k]))"),
        (0x6EE07820, "nx::sat128<int64_t>(-__int128(S.s64[k]))"),
        (0x7EE07820, "nx::sat128<int64_t>(-__int128(S.s64[k]))"),
        // neg / abs wrap in the unsigned type
        (0x6EE0B820, "uint64_t(0) - uint64_t(S.s64[k])"),
        (0x6EA0B820, "uint32_t(0) - uint32_t(S.s32[k])"),
        (0x4EE0B820, "S.s64[k] < 0 ? uint64_t(0) - uint64_t(S.s64[k]) : uint64_t(S.s64[k])"),
        // rounding shifts, including the legal #64
        (0x4F402420, "(int64_t(S.s64[k]) + (__int128(1) << (64 - 1))) >> 64"),
        (0x6F402420, "(uint64_t(S.u64[k]) + ((unsigned __int128)(1) << (64 - 1))) >> 64"),
        (0x7F402420, "(uint64_t(S.u64[k]) + ((unsigned __int128)(1) << (64 - 1))) >> 64"),
        // accumulating forms wrap in the unsigned type
        (0x4F403420, "uint64_t(r.s64[k]) + uint64_t((int64_t(S.s64[k]) + (__int128(1) << (64 - 1))) >> 64)"),
        (0x6F403420, "r.u64[k] + uint64_t((uint64_t(S.u64[k]) + ((unsigned __int128)(1) << (64 - 1))) >> 64)"),
        (0x4F7E1420, "uint64_t(r.s64[k]) + uint64_t(int64_t(S.s64[k]) >> (2 >= 64 ? 64 - 1 : 2))"),
    ] {
        let out = emit(word);
        assert!(out.contains(fragment), "{word:#010x}: {fragment} not in {out}");
    }
}

#[test]
fn narrow_lane_saturation_is_unchanged() {
    let module = Module::default();
    let analysis = Analysis::default();
    let emitter = Emitter::new(&module, &analysis, "fixture").unwrap();
    // sqadd v0.4s, v1.4s, v2.4s keeps the 64-bit intermediate; it is exact for 32-bit lanes.
    let out = emitter.instruction(&function(0, 4), 0, 0x4EA20C20).unwrap();
    assert!(
        out.contains("nx::sat<int32_t>(int64_t(S0.s32[k]) + int64_t(S1.s32[k]))"),
        "{out}"
    );
    assert!(!out.contains("__int128"), "{out}");
}

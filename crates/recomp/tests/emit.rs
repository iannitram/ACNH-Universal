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

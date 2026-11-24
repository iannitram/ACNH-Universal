//! Synthetic compatibility fixtures for the Rust port of message reflow.
use acnh_core::gamefolder::GameFolder;
use acnh_core::msg::reflow_archive;
use acnh_core::platform::Os;
use acnh_core::prepare::{prepare_with, ProcessRunner, Request};
use acnh_core::recipe::Hub;
use acnh_core::version::Version;
use std::fs;

#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

impl Order {
    fn u16(self, value: u16) -> [u8; 2] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }

    fn u32(self, value: usize) -> [u8; 4] {
        let value = u32::try_from(value).unwrap();
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }

    fn text(self, units: impl IntoIterator<Item = u16>) -> Vec<u8> {
        units.into_iter().flat_map(|unit| self.u16(unit)).collect()
    }
}

fn section(data: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8], order: Order) {
    data.extend_from_slice(tag);
    data.extend(order.u32(payload.len()));
    data.extend([0; 8]);
    data.extend_from_slice(payload);
    data.resize(data.len().next_multiple_of(16), 0xab);
}

fn msbt(order: Order, messages: &[Vec<u16>], padded: bool) -> Vec<u8> {
    let mut data = vec![0; 32];
    data[..8].copy_from_slice(b"MsgStdBn");
    data[8..10].copy_from_slice(&order.u16(0xfeff));
    data[12] = 1;
    data[14..16].copy_from_slice(&order.u16(if padded { 3 } else { 2 }));
    section(&mut data, b"LBL1", b"Synthetic labels", order);
    let mut payload = vec![0; 4 + messages.len() * 4];
    payload[..4].copy_from_slice(&order.u32(messages.len()));
    for (index, message) in messages.iter().enumerate() {
        let offset = order.u32(payload.len());
        payload[4 + index * 4..8 + index * 4].copy_from_slice(&offset);
        payload.extend(order.text(message.iter().copied().chain([0])));
    }
    section(&mut data, b"TXT2", &payload, order);
    if padded {
        section(&mut data, b"ATR1", b"Synthetic attributes", order);
    } else {
        data.truncate(data.len() - (16 - payload.len() % 16) % 16);
    }
    let total = order.u32(data.len());
    data[18..22].copy_from_slice(&total);
    data
}

fn sarc(order: Order, members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut names = Vec::new();
    let mut data = b"SARC".to_vec();
    data.extend(order.u16(20));
    data.extend(order.u16(0xfeff));
    data.extend([0; 8]);
    data.extend(order.u16(0x100));
    data.extend([0; 2]);
    data.extend(b"SFAT");
    data.extend(order.u16(12));
    data.extend(order.u16(members.len().try_into().unwrap()));
    data.extend(order.u32(101));
    for (name, content) in members {
        data.extend(order.u32(0));
        data.extend(order.u32(0x0100_0000 | (names.len() / 4)));
        names.extend(name.as_bytes());
        names.push(0);
        names.resize(names.len().next_multiple_of(4), 0);
        body.resize(body.len().next_multiple_of(16), 0);
        data.extend(order.u32(body.len()));
        body.extend_from_slice(content);
        data.extend(order.u32(body.len()));
    }
    data.extend(b"SFNT");
    data.extend(order.u16(8));
    data.extend([0; 2]);
    data.extend(names);
    data.resize(data.len().next_multiple_of(32), 0);
    let offset = order.u32(data.len());
    data[12..16].copy_from_slice(&offset);
    data.extend(body);
    let total = order.u32(data.len());
    data[8..12].copy_from_slice(&total);
    data
}

#[test]
fn synthetic_archives_reflow_matching_messages_and_keep_unrelated() {
    let plain = "one".encode_utf16().collect();
    let wrapping = "abcdefghij abcdefghij abcdefghij\nnext words."
        .encode_utf16()
        .collect();
    let mut pages: Vec<_> = "abc def\nghi jkl".encode_utf16().collect();
    pages.extend([0x0e, 0, 4, 0]);
    pages.extend("new\npage".encode_utf16());
    let mut tags: Vec<_> = "abcdefghij ".encode_utf16().collect();
    tags.extend([0x0e, 110, 0, 0, 32, 0x0e, 90, 2, 0]);
    tags.extend("abcdefghij\ncontinues".encode_utf16());
    let width_boundary = "aaaaaaaaa aaaaaaaaa".encode_utf16().collect();
    let messages = [plain, wrapping, pages, tags, width_boundary];
    for (name, order, padded) in [
        ("Talk_test.sarc.zs", Order::Little, true),
        ("Tutorials_test.sarc.zs", Order::Big, false),
        ("Mail_test.sarc.zs", Order::Big, true),
        ("System_test.sarc.zs", Order::Little, true),
    ] {
        let message = msbt(order, &messages, padded);
        let archive = sarc(
            order,
            &[
                ("LifeSupport/test.msbt", &message),
                ("Other/test.msbt", &message),
                ("unrelated", b"unchanged"),
            ],
        );
        let measured = |unit| Ok(if unit == 32 { 1600 } else { 6416 });
        let (output, rust_changes) = reflow_archive(name, &archive, measured).unwrap().unwrap();
        assert!(rust_changes > 0, "{name}");
        let parsed = acnh_core::sarc::parse(&output).unwrap();
        let members = parsed.members().unwrap();
        assert_eq!(members.len(), 3, "{name}");
        assert_ne!(members[0].data, message.as_slice(), "{name}");
        if name.starts_with("System_") {
            assert_eq!(members[1].data, message.as_slice(), "{name}");
        } else {
            assert_ne!(members[1].data, message.as_slice(), "{name}");
        }
        assert_eq!(members[2].data, b"unchanged", "{name}");
    }
}

#[test]
fn unrelated_archives_are_not_opened() {
    assert!(
        reflow_archive("Menus_test.sarc.zs", b"not an archive", |_| unreachable!())
            .unwrap()
            .is_none()
    );
}

#[test]
fn late_setup_and_a_fully_cached_retry_consume_pending_reflow() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = root.join("build-out");
    let game = root.join("fake-game");
    let (target, hub, exe) = if cfg!(windows) {
        (Os::Windows, Hub::Desktop, "acnh.exe")
    } else {
        (Os::Linux, Hub::StrixHalo, "acnh")
    };
    let req = Request {
        game: GameFolder {
            root: game.clone(),
            exefs: game.join("exefs"),
            romfs: game.join("romfs"),
            aoc: vec![],
            from_importer: false,
            version: Version::V3_0_2,
        },
        out: out.clone(),
        target,
        jobs: 0,
        source: None,
        fan_mods: Default::default(),
        delivery: None,
        install: None,
    };
    for name in [
        ".prepare-tools.json",
        "version.ini",
        "analysis/main.bin",
        "analysis/sdk.bin",
        "analysis/subsdk0.bin",
        "gen/main/table.cpp",
        "gen/sdk/table.cpp",
        "gen/subsdk0/table.cpp",
        "spirv/summary.txt",
        "fonts/glyphs_xbox/console/romfs/Font/ScalableFont.sarc.zs",
    ] {
        let file = out.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "synthetic").unwrap();
    }
    let build_exe = out.join("build").join(exe);
    fs::create_dir_all(build_exe.parent().unwrap()).unwrap();
    fs::write(&build_exe, "synthetic").unwrap();
    fs::write(out.join(".fan-mod-settings"), "false false\n").unwrap();
    fs::write(
        out.join("gen/.recomp-hash"),
        format!("{}\n", acnh_core::prepare::recomp_hash(root).unwrap()),
    )
    .unwrap();
    let mut runner = ProcessRunner::new(root.to_path_buf(), 0);

    let pending = out.join("mods/mock/.msg-reflow-pending");
    fs::create_dir_all(pending.parent().unwrap().join("romfs")).unwrap();
    fs::create_dir(&pending).unwrap();
    prepare_with(&req, hub, &mut runner, root, &mut |_| {
        panic!("all stages are cached")
    })
    .unwrap();
    assert!(!pending.exists());

    // A previous install may have moved some ready files before hitting an error.
    let ready = out.join("mods/mock/.msg-reflow-ready");
    let messages = out.join("mods/mock/romfs/Message");
    fs::create_dir(&ready).unwrap();
    fs::create_dir(&messages).unwrap();
    fs::write(ready.join("01.fixture"), "rewritten first").unwrap();
    fs::write(ready.join("02.fixture"), "rewritten second").unwrap();
    fs::create_dir(messages.join("02.fixture")).unwrap();
    let error = prepare_with(&req, hub, &mut runner, root, &mut |_| {
        panic!("all stages are cached")
    })
    .unwrap_err();
    assert!(error.to_string().contains("stage game"));
    assert_eq!(
        fs::read_to_string(messages.join("01.fixture")).unwrap(),
        "rewritten first"
    );
    assert!(!ready.join("01.fixture").exists());
    assert!(ready.join("02.fixture").exists());
    fs::remove_dir(messages.join("02.fixture")).unwrap();
    fs::write(messages.join("01.fixture"), "first left alone").unwrap();
    prepare_with(&req, hub, &mut runner, root, &mut |_| {
        panic!("all stages are cached")
    })
    .unwrap();
    assert_eq!(
        fs::read_to_string(messages.join("01.fixture")).unwrap(),
        "first left alone"
    );
    assert_eq!(
        fs::read_to_string(messages.join("02.fixture")).unwrap(),
        "rewritten second"
    );
    assert!(!ready.exists());
}

// Tests for assemble (issue 1.22b): the self-contained player of each hub, from a fake build
// folder. The binaries are minimal ELF, PE and Mach-O files written here, so every layout is
// checked on every runner; on macOS the Mac app is also made from binaries `cc` builds, with
// the real `install_name_tool` and `codesign`, and checked with `plutil`, `codesign` and
// `otool`.
use acnh_core::assemble::{assemble_with, mach_links, player, Plan, Tools};
use acnh_core::bundle::{is_forbidden, manifest};
use acnh_core::error::{Error, Result};
use acnh_core::platform::Os;
use std::fs;
use std::path::{Path, PathBuf};

const ARM64: u32 = 0x0100_000c;
const X86_64: u32 = 0x0100_0007;
const FRAMEWORKS: &str = "@executable_path/../Frameworks/";

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acnh-assemble-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn put(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn set(bytes: &mut [u8], at: usize, value: usize, len: usize) {
    bytes[at..at + len].copy_from_slice(&(value as u64).to_le_bytes()[..len]);
}

/// A minimal 64-bit ELF file that needs `needed`, with `run_path` as its `DT_RUNPATH`.
fn elf(needed: &[&str], run_path: Option<&str>) -> Vec<u8> {
    let mut strings = vec![0u8];
    let mut entries = Vec::new();
    for (tag, name) in needed
        .iter()
        .map(|n| (1, *n))
        .chain(run_path.map(|r| (29, r)))
    {
        entries.push((tag, strings.len()));
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
    }
    // The header, two program headers (the loaded segment at 0x1000, the dynamic section),
    // the dynamic entries, then their strings.
    let dynamic = 64 + 2 * 56;
    let table = dynamic + (entries.len() + 2) * 16;
    entries.push((5, 0x1000 + table));
    entries.push((0, 0));
    let mut bytes = vec![0u8; dynamic];
    bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
    set(&mut bytes, 0x20, 64, 8);
    set(&mut bytes, 0x36, 56, 2);
    set(&mut bytes, 0x38, 2, 2);
    for (at, kind, offset, size) in [
        (64, 1, 0, table + strings.len()),
        (120, 2, dynamic, entries.len() * 16),
    ] {
        set(&mut bytes, at, kind, 4);
        set(&mut bytes, at + 8, offset, 8);
        set(&mut bytes, at + 16, 0x1000 + offset, 8);
        set(&mut bytes, at + 32, size, 8);
    }
    for (tag, value) in entries {
        bytes.extend_from_slice(&(tag as u64).to_le_bytes());
        bytes.extend_from_slice(&(value as u64).to_le_bytes());
    }
    bytes.extend_from_slice(&strings);
    bytes
}

/// A minimal 64-bit PE file that imports `imports`.
fn pe(imports: &[&str]) -> Vec<u8> {
    // The DOS header, the PE header at 0x40, a 240-byte optional header at 0x58, one section
    // header at 328, and that section's data (loaded at 0x2000) at 368: the import table.
    let mut bytes = vec![0u8; 368];
    bytes[..2].copy_from_slice(b"MZ");
    set(&mut bytes, 0x3c, 0x40, 4);
    bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
    set(&mut bytes, 0x46, 1, 2);
    set(&mut bytes, 0x54, 240, 2);
    set(&mut bytes, 0x58, 0x20b, 2);
    set(&mut bytes, 0x58 + 108, 16, 4);
    set(&mut bytes, 0x58 + 120, 0x2000, 4);
    let table = (imports.len() + 1) * 20;
    let mut names = Vec::new();
    for name in imports {
        let mut descriptor = [0u8; 20];
        set(&mut descriptor, 12, 0x2000 + table + names.len(), 4);
        bytes.extend_from_slice(&descriptor);
        names.extend_from_slice(name.as_bytes());
        names.push(0);
    }
    bytes.extend_from_slice(&[0u8; 20]);
    bytes.extend_from_slice(&names);
    let size = bytes.len() - 368;
    set(&mut bytes, 328 + 8, size, 4);
    set(&mut bytes, 328 + 12, 0x2000, 4);
    set(&mut bytes, 328 + 16, size, 4);
    set(&mut bytes, 328 + 20, 368, 4);
    bytes
}

/// A minimal 64-bit Mach-O file for `cpu` that links `links`.
fn macho(cpu: u32, links: &[String]) -> Vec<u8> {
    let mut commands = Vec::new();
    for link in links {
        let mut command = vec![0u8; (24 + link.len() + 8) / 8 * 8];
        let size = command.len();
        set(&mut command, 0, 0xc, 4);
        set(&mut command, 4, size, 4);
        set(&mut command, 8, 24, 4);
        command[24..24 + link.len()].copy_from_slice(link.as_bytes());
        commands.extend_from_slice(&command);
    }
    let mut bytes = vec![0u8; 32];
    bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    set(&mut bytes, 4, cpu as usize, 4);
    set(&mut bytes, 12, 2, 4);
    set(&mut bytes, 16, links.len(), 4);
    set(&mut bytes, 20, commands.len(), 4);
    bytes.extend_from_slice(&commands);
    bytes
}

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// Stand-ins for `patchelf`, `install_name_tool` and `codesign`. `rename` rewrites the Mach-O
/// files written here as `install_name_tool -change` would.
#[derive(Default)]
struct Fake {
    rpaths: Vec<(String, String)>,
    ids: Vec<(String, String)>,
    signed: Vec<String>,
}

impl Tools for Fake {
    fn set_rpath(&mut self, file: &Path, rpath: &str) -> Result<()> {
        self.rpaths.push((name(file), rpath.to_owned()));
        Ok(())
    }

    fn rename(
        &mut self,
        file: &Path,
        id: Option<&str>,
        changes: &[(String, String)],
    ) -> Result<()> {
        if let Some(id) = id {
            self.ids.push((name(file), id.to_owned()));
        }
        let links: Vec<String> = mach_links("fixture", &fs::read(file)?)?
            .into_iter()
            .map(|link| match changes.iter().find(|(old, _)| *old == link) {
                Some((_, new)) => new.clone(),
                None => link,
            })
            .collect();
        fs::write(file, macho(ARM64, &links))?;
        Ok(())
    }

    fn sign(&mut self, path: &Path) -> Result<()> {
        self.signed.push(name(path));
        Ok(())
    }
}

fn plan(os: Os, dir: &Path, libs: &[PathBuf]) -> Plan {
    Plan {
        os,
        build: dir.join("out").join("build"),
        install: dir.join("install"),
        libs: libs.to_vec(),
        system: Some(dir.join("System32")),
        launcher: dir.join("target").join("release").join("acnh-app"),
        source: dir.join("source"),
        version: "9.9.9".to_owned(),
    }
}

/// What `bundle::manifest` seals of an install: its files, none of them forbidden.
fn sealed(install: &Path, os: Os) -> Vec<String> {
    let bundle = manifest(install, os, "3.0.2", "0123abc").unwrap();
    let paths: Vec<String> = bundle.files.into_iter().map(|file| file.path).collect();
    for path in &paths {
        assert!(!is_forbidden(path), "{path}");
    }
    paths
}

fn build_error(result: Result<PathBuf>) -> String {
    match result {
        Err(Error::Build(message)) => message,
        other => panic!("{other:?}"),
    }
}

#[test]
fn linux_layout_is_the_player_and_the_libraries_a_stock_system_may_lack() {
    let dir = tmp("linux");
    let system = dir.join("usr-lib");
    let plan = plan(Os::Linux, &dir, std::slice::from_ref(&system));
    let needed = [
        "libSDL3.so.0",
        "libvpx.so.9",
        "libvorbis.so.0",
        "libvulkan.so.1",
        "libc.so.6",
    ];
    put(
        &plan.build.join("acnh"),
        &elf(&needed, Some("$ORIGIN/sdl:/nowhere")),
    );
    put(
        &plan.build.join("sdl").join("libSDL3.so.0"),
        &elf(&["libc.so.6"], None),
    );
    put(&plan.build.join("CMakeCache.txt"), b"cache");
    put(&plan.build.join("gen_main.o"), b"object");
    put(&plan.build.join("libacnh_game.so"), b"game library");
    put(&system.join("libvpx.so.9"), &elf(&[], None));
    put(&system.join("libvorbis.so.0"), &elf(&["libogg.so.0"], None));
    put(&system.join("libogg.so.0"), &elf(&[], None));
    put(&system.join("libvulkan.so.1"), &elf(&[], None));
    put(&system.join("libc.so.6"), &elf(&[], None));
    // What belongs to the device stays, and is not part of the build.
    put(&plan.install.join("root").join("config.ini"), b"mine");

    let mut fake = Fake::default();
    let made = assemble_with(&plan, &mut fake).unwrap();
    assert_eq!(made, plan.install.join("acnh"));
    let expected = [
        "acnh",
        "libSDL3.so.0",
        "libogg.so.0",
        "libvorbis.so.0",
        "libvpx.so.9",
    ];
    assert_eq!(sealed(&plan.install, Os::Linux), expected);
    assert!(plan.install.join("root").join("config.ini").is_file());
    // Each of them finds the others beside it.
    let mut rpaths = fake.rpaths.clone();
    rpaths.sort();
    let origin: Vec<_> = expected
        .iter()
        .map(|file| (file.to_string(), "$ORIGIN".to_owned()))
        .collect();
    assert_eq!(rpaths, origin);

    // A library that is nowhere is named.
    fs::remove_file(system.join("libogg.so.0")).unwrap();
    let message = build_error(assemble_with(&plan, &mut Fake::default()));
    assert!(message.contains("libogg.so.0"), "{message}");
}

#[test]
fn linux_player_that_needs_nothing_extra_is_left_as_built() {
    let dir = tmp("linux-plain");
    let plan = plan(Os::Linux, &dir, &[]);
    put(&plan.build.join("acnh"), &elf(&["libc.so.6"], None));
    let mut fake = Fake::default();
    assemble_with(&plan, &mut fake).unwrap();
    assert_eq!(sealed(&plan.install, Os::Linux), ["acnh"]);
    assert!(fake.rpaths.is_empty());
}

#[test]
fn windows_layout_is_the_player_with_its_dlls_beside_it() {
    let dir = tmp("windows");
    let redist = dir.join("Microsoft.VC143.CRT");
    let plan = plan(Os::Windows, &dir, std::slice::from_ref(&redist));
    let imports = [
        "KERNEL32.dll",
        "SDL3.dll",
        "VCRUNTIME140.dll",
        "api-ms-win-crt-runtime-l1-1-0.dll",
        "dbghelp.dll",
    ];
    put(&plan.build.join("acnh.exe"), &pe(&imports));
    put(&plan.build.join("SDL3.dll"), &pe(&["USER32.dll"]));
    put(&plan.build.join("nvngx_dlss.dll"), &pe(&[]));
    put(&plan.build.join("acnh.pdb"), b"symbols");
    put(&plan.build.join("build.ninja"), b"rules");
    put(&plan.build.join("gen").join("main.obj"), b"object");
    put(&redist.join("vcruntime140.dll"), &pe(&["kernel32.dll"]));
    put(&dir.join("System32").join("dbghelp.dll"), b"Windows's own");

    let made = assemble_with(&plan, &mut Fake::default()).unwrap();
    assert_eq!(made, plan.install.join("acnh.exe"));
    assert_eq!(
        sealed(&plan.install, Os::Windows),
        ["SDL3.dll", "acnh.exe", "nvngx_dlss.dll", "vcruntime140.dll"]
    );

    // An import that is neither beside the player nor Windows's own is named.
    put(&plan.build.join("acnh.exe"), &pe(&["missing_thing.dll"]));
    let message = build_error(assemble_with(&plan, &mut Fake::default()));
    assert!(message.contains("missing_thing.dll"), "{message}");
}

#[test]
fn phones_have_their_own_packages() {
    let dir = tmp("phones");
    for os in [Os::Ios, Os::Android] {
        let result = assemble_with(&plan(os, &dir, &[]), &mut Fake::default());
        assert!(matches!(result, Err(Error::Unsupported(_))), "{result:?}");
    }
}

#[test]
fn only_an_arm64_mach_o_passes() {
    let links = vec![
        "/usr/lib/libSystem.B.dylib".to_owned(),
        "/opt/homebrew/opt/sdl3/lib/libSDL3.0.dylib".to_owned(),
    ];
    assert_eq!(mach_links("acnh", &macho(ARM64, &links)).unwrap(), links);

    let refused = |bytes: &[u8]| match mach_links("acnh", bytes) {
        Err(Error::Build(message)) => message,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        refused(&macho(X86_64, &links)),
        "acnh is built for Intel (x86-64): the Mac app is arm64 only (Apple silicon, no Rosetta)"
    );
    // A universal file: big-endian magic, then its two slices.
    let fat = [0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2, 1, 0, 0, 7, 0, 0, 0, 3];
    assert_eq!(
        refused(&fat),
        "acnh is a universal (fat) binary: the Mac app is arm64 only (Apple silicon, no Rosetta)"
    );
    assert!(refused(b"#!/bin/sh\n").contains("not a 64-bit Mach-O"));
    assert!(refused(&macho(ARM64, &links)[..40]).contains("damaged"));
}

#[test]
fn the_mac_player_is_the_one_beside_the_launcher_in_the_contained_app() {
    let dir = tmp("player");
    let install = dir.join("install");
    let macos = dir
        .join("ACNH-Universal.app")
        .join("Contents")
        .join("MacOS");
    put(&macos.join("acnh-app"), b"launcher");
    put(&macos.join("acnh"), b"player");
    assert_eq!(
        player(Os::Mac, &macos.join("acnh-app"), &install),
        macos.join("acnh")
    );
    // A launcher with no player beside it (the release's app, a checkout) runs the one in
    // the app under the install folder.
    let bare = dir.join("target").join("release").join("acnh-app");
    put(&bare, b"launcher");
    assert_eq!(
        player(Os::Mac, &bare, &install),
        install
            .join("ACNH-Universal.app")
            .join("Contents")
            .join("MacOS")
            .join("acnh")
    );
    assert_eq!(
        player(Os::Linux, &macos.join("acnh-app"), &install),
        install.join("acnh")
    );
    assert_eq!(
        player(Os::Windows, &bare, &install),
        install.join("acnh.exe")
    );
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_release_fills_the_same_info_plist() {
    let workflow = fs::read_to_string(repo().join(".github/workflows/release.yml")).unwrap();
    assert_eq!(workflow.matches("<plist").count(), 0);
    assert!(workflow.contains("assets/mac/Info.plist.in"));
    let template = fs::read_to_string(repo().join("assets/mac/Info.plist.in")).unwrap();
    assert!(template.contains("<string>@VERSION@</string>"));
    assert!(template.contains("<string>acnh-app</string>"));
}

// ---- the Mac app -----------------------------------------------------------------------------

/// Makes a binary at a path that links the given Homebrew libraries (by their paths).
type Make = dyn Fn(&Path, &[&Path]);

fn fake_binary(path: &Path, links: &[&Path]) {
    let mut links: Vec<String> = links.iter().map(|l| l.display().to_string()).collect();
    links.push("/usr/lib/libSystem.B.dylib".to_owned());
    put(path, &macho(ARM64, &links));
}

/// A fake build folder, Homebrew prefix, launcher bundle, checkout and source in `dir`.
/// `from_bundle`: the launcher runs from its release bundle, else from `target/release/`.
fn mac_plan(dir: &Path, make: &Make, real_homebrew: Option<&Path>, from_bundle: bool) -> Plan {
    let brew = dir.join("brew");
    let mut plan = plan(Os::Mac, dir, std::slice::from_ref(&brew));
    let ogg = brew.join("opt/libogg/lib/libogg.0.dylib");
    let sdl = brew.join("lib/libSDL3.0.dylib");
    make(&ogg, &[]);
    make(&sdl, &[&ogg]);
    match real_homebrew {
        Some(prefix) => plan.libs.push(prefix.to_path_buf()),
        None => {
            make(&brew.join("opt/vulkan-loader/lib/libvulkan.1.dylib"), &[]);
            make(&brew.join("opt/molten-vk/lib/libMoltenVK.dylib"), &[]);
        }
    }
    make(&plan.build.join("acnh"), &[&sdl]);
    put(&plan.build.join("CMakeCache.txt"), b"cache");
    put(&plan.build.join("gen_main.o"), b"object");

    let template = fs::read(repo().join("assets/mac/Info.plist.in")).unwrap();
    let folder = if from_bundle {
        let contents = dir.join("release/ACNH Universal.app/Contents");
        let plist = String::from_utf8(template).unwrap();
        put(
            &contents.join("Info.plist"),
            plist.replace("@VERSION@", "1.0.0").as_bytes(),
        );
        put(&contents.join("Resources/acnhrecomp.icns"), b"icon");
        put(&contents.join("Resources/acnhrecomp.png"), b"picture");
        contents.join("MacOS")
    } else {
        put(&plan.source.join("assets/mac/Info.plist.in"), &template);
        put(&plan.source.join("assets/acnhrecomp.icns"), b"icon");
        put(&plan.source.join("assets/acnhrecomp.png"), b"picture");
        dir.join("target/release")
    };
    make(&folder.join("acnh-app"), &[]);
    make(&folder.join("acnh-universal"), &[]);
    plan.launcher = folder.join("acnh-app");
    plan
}

fn walk(root: &Path, folder: &Path, files: &mut Vec<String>) {
    for entry in fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(root, &path, files);
        } else {
            let relative = path.strip_prefix(root).unwrap();
            let parts: Vec<_> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect();
            files.push(parts.join("/"));
        }
    }
}

const BINARIES: [&str; 7] = [
    "Contents/Frameworks/libMoltenVK.dylib",
    "Contents/Frameworks/libSDL3.0.dylib",
    "Contents/Frameworks/libogg.0.dylib",
    "Contents/Frameworks/libvulkan.1.dylib",
    "Contents/MacOS/acnh",
    "Contents/MacOS/acnh-app",
    "Contents/MacOS/acnh-universal",
];

/// The one contained app: its files at their paths, nothing else in the install folder, the
/// driver file pointing at Frameworks, and every binary linking only the system and
/// Frameworks. Returns its `Info.plist`.
fn check_app(plan: &Plan, made: &Path) -> String {
    let app = plan.install.join("ACNH-Universal.app");
    assert_eq!(made, app);
    let mut top = Vec::new();
    for entry in fs::read_dir(&plan.install).unwrap() {
        top.push(name(&entry.unwrap().path()));
    }
    assert_eq!(top, ["ACNH-Universal.app"]);

    let mut files = Vec::new();
    walk(&app, &app, &mut files);
    files.retain(|file| !file.starts_with("Contents/_CodeSignature/")); // codesign's seal
    files.sort();
    let mut expected = BINARIES.to_vec();
    expected.extend([
        "Contents/Info.plist",
        "Contents/Resources/MoltenVK_icd.json",
        "Contents/Resources/acnhrecomp.icns",
        "Contents/Resources/acnhrecomp.png",
    ]);
    expected.sort();
    assert_eq!(files, expected);

    let driver = fs::read_to_string(app.join("Contents/Resources/MoltenVK_icd.json")).unwrap();
    assert!(driver.contains("\"library_path\": \"../Frameworks/libMoltenVK.dylib\""));
    for binary in BINARIES {
        for link in mach_links(binary, &fs::read(app.join(binary)).unwrap()).unwrap() {
            assert!(
                ["/usr/lib/", "/System/", FRAMEWORKS]
                    .iter()
                    .any(|allowed| link.starts_with(allowed)),
                "{binary} links {link}"
            );
        }
    }
    let player = mach_links("acnh", &fs::read(app.join("Contents/MacOS/acnh")).unwrap()).unwrap();
    assert!(player.contains(&format!("{FRAMEWORKS}libSDL3.0.dylib")));
    fs::read_to_string(app.join("Contents/Info.plist")).unwrap()
}

#[test]
fn mac_layout_is_one_contained_app_from_the_launcher_bundle() {
    let dir = tmp("mac-bundle");
    let plan = mac_plan(&dir, &fake_binary, None, true);
    // An app from an earlier prepare is replaced whole.
    put(
        &plan.install.join("ACNH-Universal.app/Contents/old"),
        b"stale",
    );
    let mut fake = Fake::default();
    let made = assemble_with(&plan, &mut fake).unwrap();
    let plist = check_app(&plan, &made);
    assert!(plist.contains("<string>1.0.0</string>"), "{plist}");

    // Each library is named from Frameworks; each binary is signed, the bundle last (its seal
    // covers the launcher, the bundle's own executable).
    let mut ids = fake.ids.clone();
    ids.sort();
    let libraries = [
        "libMoltenVK.dylib",
        "libSDL3.0.dylib",
        "libogg.0.dylib",
        "libvulkan.1.dylib",
    ];
    let named: Vec<_> = libraries
        .iter()
        .map(|file| (file.to_string(), format!("{FRAMEWORKS}{file}")))
        .collect();
    assert_eq!(ids, named);
    assert_eq!(fake.signed.last().unwrap(), "ACNH-Universal.app");
    let mut signed = fake.signed[..fake.signed.len() - 1].to_vec();
    signed.sort();
    let mut expected = libraries.to_vec();
    expected.extend(["acnh", "acnh-universal"]);
    expected.sort();
    assert_eq!(signed, expected);
}

#[test]
fn mac_never_copies_the_game_of_the_delivered_app_it_runs_from() {
    let dir = tmp("mac-bundle-game");
    let plan = mac_plan(&dir, &fake_binary, None, true);
    let game = dir.join("release/ACNH Universal.app/Contents/Resources/game");
    put(&game.join("analysis/main.bin"), b"");
    put(&game.join("romfs.pak"), b"");
    let mut fake = Fake::default();
    let made = assemble_with(&plan, &mut fake).unwrap();
    assert!(!made.join("Contents/Resources/game").exists());
    assert!(game.join("analysis/main.bin").is_file());
}

#[test]
fn mac_layout_is_the_same_app_from_a_checkout() {
    let dir = tmp("mac-checkout");
    let plan = mac_plan(&dir, &fake_binary, None, false);
    let made = assemble_with(&plan, &mut Fake::default()).unwrap();
    let plist = check_app(&plan, &made);
    assert!(plist.contains("<string>9.9.9</string>"), "{plist}");
    assert!(!plist.contains("@VERSION@"));
}

#[test]
fn mac_app_is_refused_inside_a_git_work_tree() {
    let dir = tmp("mac-git");
    let mut plan = mac_plan(&dir, &fake_binary, None, true);
    let clone = dir.join("clone");
    fs::create_dir_all(clone.join(".git")).unwrap();
    plan.install = clone.join("private").join("install");
    let message = build_error(assemble_with(&plan, &mut Fake::default()));
    assert!(message.contains("never written inside a git work tree"));
    assert!(!plan.install.exists());

    // A linked work tree's `.git` is a file.
    let linked = dir.join("linked");
    put(&linked.join(".git"), b"gitdir: elsewhere\n");
    plan.install = linked.join("install");
    let message = build_error(assemble_with(&plan, &mut Fake::default()));
    assert!(message.contains("never written inside a git work tree"));
}

#[test]
fn mac_app_refuses_a_binary_that_is_not_arm64_only() {
    let dir = tmp("mac-intel");
    let plan = mac_plan(&dir, &fake_binary, None, true);
    put(&plan.build.join("acnh"), &macho(X86_64, &[]));
    let message = build_error(assemble_with(&plan, &mut Fake::default()));
    assert!(message.starts_with("acnh is built for Intel (x86-64)"));
}

/// The Mac app made for real: binaries from `cc`, Homebrew's MoltenVK and Vulkan loader
/// (stand-ins where they are not installed; `ACNH_TEST_HOMEBREW_VULKAN` demands them, as
/// the macOS job does), `install_name_tool` and `codesign`.
#[cfg(target_os = "macos")]
mod real {
    use super::*;
    use acnh_core::assemble::HostTools;
    use std::process::Command;

    fn ok(command: &mut Command) -> String {
        let output = command.output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{command:?}: {text}");
        text
    }

    fn cc_binary(path: &Path, links: &[&Path]) {
        let source = path.with_extension("c");
        put(
            &source,
            b"int acnh_fixture(void) { return 0; }\nint main(void) { return 0; }\n",
        );
        let mut cc = Command::new("cc");
        cc.arg("-o").arg(path).arg(&source).args(links);
        cc.arg("-Wl,-headerpad_max_install_names");
        if path.extension().is_some_and(|ext| ext == "dylib") {
            cc.arg("-dynamiclib").arg("-install_name").arg(path);
        }
        ok(&mut cc);
        fs::remove_file(source).unwrap();
    }

    fn homebrew() -> Option<PathBuf> {
        let prefix = std::env::var_os("HOMEBREW_PREFIX")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/opt/homebrew"));
        let installed = prefix.join("opt/molten-vk/lib/libMoltenVK.dylib").is_file()
            && prefix
                .join("opt/vulkan-loader/lib/libvulkan.1.dylib")
                .is_file();
        assert!(
            installed || std::env::var_os("ACNH_TEST_HOMEBREW_VULKAN").is_none(),
            "brew install molten-vk vulkan-loader"
        );
        installed.then_some(prefix)
    }

    fn check_real(name: &str, from_bundle: bool) {
        let dir = tmp(name);
        let plan = mac_plan(&dir, &cc_binary, homebrew().as_deref(), from_bundle);
        let made = assemble_with(&plan, &mut HostTools).unwrap();
        check_app(&plan, &made);

        ok(Command::new("plutil")
            .arg("-lint")
            .arg(made.join("Contents/Info.plist")));
        ok(Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&made));
        for binary in BINARIES {
            let listed = ok(Command::new("otool").arg("-L").arg(made.join(binary)));
            for line in listed.lines().skip(1) {
                let link = line.trim();
                assert!(
                    ["/usr/lib/", "/System/", FRAMEWORKS]
                        .iter()
                        .any(|allowed| link.starts_with(allowed)),
                    "{binary} links {link}"
                );
            }
        }
        // The player starts from inside the app, with its libraries from Frameworks.
        ok(&mut Command::new(made.join("Contents/MacOS/acnh")));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mac_app_from_the_launcher_bundle_is_signed_and_self_contained() {
        check_real("mac-real-bundle", true);
    }

    #[test]
    fn mac_app_from_a_checkout_is_signed_and_self_contained() {
        check_real("mac-real-checkout", false);
    }
}

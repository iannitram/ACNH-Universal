//! Android private-storage game installer (#192).
//!
//! Copies only the six prepared game entries (`analysis/`, `romfs.pak`, `spirv.pak`,
//! `fonts/`, `aoc/` when present, and `libacnh_game.so`) into `<private>/game.partial/`,
//! resuming over matching-size files, atomically renames `game.partial/` to
//! `<private>/game/`, then marks the tree read-only (macOS will not rename a directory without
//! write permission). Writes nowhere outside the given private folder.

use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Final folder name inside the app's private storage.
pub const GAME_DIR: &str = "game";
/// Staging folder name while copying.
pub const PARTIAL_DIR: &str = "game.partial";

const REQUIRED_FILES: [&str; 4] = [
    "analysis/main.bin",
    "romfs.pak",
    "spirv.pak",
    "libacnh_game.so",
];

#[derive(Debug, Clone)]
struct CopyItem {
    src: PathBuf,
    rel: PathBuf,
    size: u64,
}

/// Install the prepared game from `source` into `dest` (inside `private_dir`), checking
/// that `free_bytes` covers the remaining bytes to copy.
pub fn install(source: &Path, private_dir: &Path, dest: &Path, free_bytes: u64) -> Result<PathBuf> {
    let ci = env_nonempty("CI") || env_nonempty("GITHUB_ACTIONS");
    let mut copy = |from: &Path, to: &Path| -> Result<()> {
        fs::copy(from, to)?;
        Ok(())
    };
    install_with(source, private_dir, dest, free_bytes, ci, &mut copy)
}

/// Install with explicit CI flag and file-copy seam for deterministic tests.
pub fn install_with(
    source: &Path,
    private_dir: &Path,
    dest: &Path,
    free_bytes: u64,
    ci: bool,
    copy_file: &mut dyn FnMut(&Path, &Path) -> Result<()>,
) -> Result<PathBuf> {
    check_dest_inside_private(private_dir, dest)?;
    let items = collect_copy_plan(source, ci)?;

    let partial = private_dir.join(PARTIAL_DIR);
    let mut needed_bytes: u64 = 0;
    for item in &items {
        let dst_file = partial.join(&item.rel);
        if !already_copied(&dst_file, item.size) {
            needed_bytes = needed_bytes.saturating_add(item.size);
        }
    }
    if free_bytes < needed_bytes {
        return Err(Error::Io(format!(
            "not enough free space: need {needed_bytes} bytes ({free_bytes} bytes available)"
        )));
    }

    fs::create_dir_all(&partial)?;
    for item in &items {
        let dst_file = partial.join(&item.rel);
        if already_copied(&dst_file, item.size) {
            continue;
        }
        if let Some(parent) = dst_file.parent() {
            fs::create_dir_all(parent)?;
            if parent.exists() {
                set_writable(parent)?;
            }
        }
        if dst_file.exists() {
            set_writable(&dst_file)?;
        }
        copy_file(&item.src, &dst_file)?;
    }

    finalize_partial(&partial, dest)
}

fn finalize_partial(partial: &Path, dest: &Path) -> Result<PathBuf> {
    if dest.exists() {
        remove_tree_writable(dest)?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // Rename first, then make the tree read-only: macOS refuses to rename a directory that has no
    // write permission when its `..` entry must change (EACCES), so a read-only `game.partial/`
    // cannot always be moved into place.
    fs::rename(partial, dest)?;
    make_read_only_recursive(dest)?;
    Ok(dest.to_path_buf())
}

fn env_nonempty(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn check_dest_inside_private(private_dir: &Path, dest: &Path) -> Result<()> {
    let norm_private = normalize_path(private_dir)?;
    let norm_dest = normalize_path(dest)?;
    if !norm_dest.starts_with(&norm_private) || norm_dest == norm_private {
        return Err(Error::Unsupported(format!(
            "destination {} is outside private folder {}",
            dest.display(),
            private_dir.display()
        )));
    }
    if private_dir.exists() {
        let canon_private = fs::canonicalize(private_dir)?;
        let mut probe = private_dir.to_path_buf();
        if let Ok(rel) = norm_dest.strip_prefix(&norm_private) {
            for comp in rel.components() {
                probe.push(comp);
                if probe.exists() {
                    let canon_probe = fs::canonicalize(&probe)?;
                    if !canon_probe.starts_with(&canon_private) {
                        return Err(Error::Unsupported(format!(
                            "destination {} escapes private folder {}",
                            dest.display(),
                            private_dir.display()
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn normalize_path(path: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(Error::Unsupported(format!(
                        "invalid path {}",
                        path.display()
                    )));
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Ok(out)
}

fn resolve_game_lib(source: &Path) -> Option<PathBuf> {
    let direct = source.join("libacnh_game.so");
    if direct.is_file() {
        return Some(direct);
    }
    let importer = source.join("game/android-arm64/libacnh_game.so");
    if importer.is_file() {
        return Some(importer);
    }
    None
}

fn collect_copy_plan(source: &Path, ci: bool) -> Result<Vec<CopyItem>> {
    if !source.is_dir() {
        return Err(Error::BadGameFolder(format!(
            "{} is not a folder",
            source.display()
        )));
    }
    for req in REQUIRED_FILES {
        let ok = if req == "libacnh_game.so" {
            resolve_game_lib(source).is_some()
        } else {
            source.join(req).is_file()
        };
        if !ok {
            return Err(Error::BadGameFolder(format!("{req} is missing")));
        }
    }

    let mut items = Vec::new();
    for dir_name in ["analysis", "fonts", "aoc"] {
        let dir = source.join(dir_name);
        if dir.is_dir() {
            collect_dir(&dir, Path::new(dir_name), &mut items)?;
        }
    }
    for file_name in ["romfs.pak", "spirv.pak"] {
        let src = source.join(file_name);
        let meta = fs::metadata(&src)?;
        items.push(CopyItem {
            src,
            rel: PathBuf::from(file_name),
            size: meta.len(),
        });
    }
    let lib_src = resolve_game_lib(source)
        .ok_or_else(|| Error::BadGameFolder("libacnh_game.so is missing".into()))?;
    let lib_meta = fs::metadata(&lib_src)?;
    items.push(CopyItem {
        src: lib_src,
        rel: PathBuf::from("libacnh_game.so"),
        size: lib_meta.len(),
    });

    items.sort_by(|a, b| a.rel.cmp(&b.rel));

    if ci {
        for item in &items {
            if item.size > 0 {
                return Err(Error::Build(format!(
                    "under CI, game file must be an empty decoy: {}",
                    item.rel.display()
                )));
            }
        }
    }

    Ok(items)
}

fn collect_dir(dir: &Path, rel_prefix: &Path, items: &mut Vec<CopyItem>) -> Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let rel = rel_prefix.join(entry.file_name());
        if file_type.is_dir() {
            collect_dir(&path, &rel, items)?;
        } else if file_type.is_file() {
            let meta = fs::metadata(&path)?;
            items.push(CopyItem {
                src: path,
                rel,
                size: meta.len(),
            });
        }
    }
    Ok(())
}

fn already_copied(dst: &Path, expected_size: u64) -> bool {
    fs::metadata(dst)
        .map(|m| m.is_file() && m.len() == expected_size)
        .unwrap_or(false)
}

fn make_read_only_recursive(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            make_read_only_recursive(&path)?;
        } else {
            set_read_only(&path)?;
        }
    }
    set_read_only(dir)?;
    Ok(())
}

fn set_read_only(path: &Path) -> Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() & !0o222);
    }
    #[cfg(not(unix))]
    {
        perms.set_readonly(true);
    }
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn remove_tree_writable(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    make_writable_recursive(dir)?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

fn make_writable_recursive(dir: &Path) -> Result<()> {
    set_writable(dir)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            make_writable_recursive(&path)?;
        } else {
            set_writable(&path)?;
        }
    }
    Ok(())
}

fn set_writable(path: &Path) -> Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() | 0o200);
    }
    #[cfg(not(unix))]
    {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
    }
    fs::set_permissions(path, perms)?;
    Ok(())
}

/// One file entry in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Prepared Android game manifest (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub build_id: String,
    pub nx_game_abi: String,
    pub files: Vec<ManifestFile>,
}

impl Manifest {
    /// Serialize the manifest as compact JSON.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"version\":");
        out.push_str(&self.version.to_string());
        out.push_str(",\"build_id\":\"");
        push_json_escaped(&mut out, &self.build_id);
        out.push_str("\",\"nx_game_abi\":\"");
        push_json_escaped(&mut out, &self.nx_game_abi);
        out.push_str("\",\"files\":[");
        for (i, file) in self.files.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"path\":\"");
            push_json_escaped(&mut out, &file.path);
            out.push_str("\",\"size\":");
            out.push_str(&file.size.to_string());
            out.push_str(",\"sha256\":\"");
            push_json_escaped(&mut out, &file.sha256.to_ascii_lowercase());
            out.push_str("\"}");
        }
        out.push_str("]}");
        out
    }

    /// Parse a `manifest.json` payload.
    pub fn from_json(raw: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(raw)
            .map_err(|e| Error::BadGameFolder(format!("manifest.json is not valid UTF-8: {e}")))?;
        let mut parser = JsonParser::new(text);
        let root = parser.parse_object()?;
        parser.skip_ws();
        if !parser.is_eof() {
            return Err(Error::BadGameFolder(
                "manifest.json has trailing characters".into(),
            ));
        }

        let version = match root.get("version") {
            Some(JsonVal::Num(n)) => u32::try_from(*n)
                .map_err(|_| Error::BadGameFolder("manifest.json version out of range".into()))?,
            None => 1,
            _ => {
                return Err(Error::BadGameFolder(
                    "manifest.json version must be an integer".into(),
                ))
            }
        };
        let build_id = match root.get("build_id") {
            Some(JsonVal::Str(s)) => s.clone(),
            None => String::new(),
            _ => {
                return Err(Error::BadGameFolder(
                    "manifest.json build_id must be a string".into(),
                ))
            }
        };
        let nx_game_abi = match root.get("nx_game_abi") {
            Some(JsonVal::Str(s)) => s.clone(),
            None => String::new(),
            _ => {
                return Err(Error::BadGameFolder(
                    "manifest.json nx_game_abi must be a string".into(),
                ))
            }
        };
        let files_val = match root.get("files") {
            Some(JsonVal::Arr(arr)) => arr,
            _ => {
                return Err(Error::BadGameFolder(
                    "manifest.json is missing files array".into(),
                ))
            }
        };

        let mut files = Vec::with_capacity(files_val.len());
        for entry in files_val {
            let obj = match entry {
                JsonVal::Obj(map) => map,
                _ => {
                    return Err(Error::BadGameFolder(
                        "manifest.json files entry must be an object".into(),
                    ))
                }
            };
            let path = match obj.get("path") {
                Some(JsonVal::Str(s)) => s.clone(),
                _ => {
                    return Err(Error::BadGameFolder(
                        "manifest.json file entry is missing path".into(),
                    ))
                }
            };
            let size = match obj.get("size") {
                Some(JsonVal::Num(n)) => *n,
                _ => {
                    return Err(Error::BadGameFolder(format!(
                        "manifest.json file '{path}' is missing size"
                    )))
                }
            };
            let sha256 = match obj.get("sha256") {
                Some(JsonVal::Str(s)) => s.to_ascii_lowercase(),
                _ => {
                    return Err(Error::BadGameFolder(format!(
                        "manifest.json file '{path}' is missing sha256"
                    )))
                }
            };
            files.push(ManifestFile { path, size, sha256 });
        }

        Ok(Manifest {
            version,
            build_id,
            nx_game_abi,
            files,
        })
    }

    /// Validate that the manifest matches the expected `build_id` and `NX_GAME_ABI`,
    /// and that every file entry has a safe relative path and valid SHA-256 hex digest.
    pub fn validate(&self, expected_build_id: &str, expected_abi: &str) -> Result<()> {
        if !expected_build_id.is_empty() && self.build_id != expected_build_id {
            return Err(Error::Unsupported(format!(
                "game build id mismatch: expected {expected_build_id}, got {}",
                self.build_id
            )));
        }
        if !expected_abi.is_empty() && self.nx_game_abi != expected_abi {
            return Err(Error::Unsupported(format!(
                "NX_GAME_ABI mismatch: expected {expected_abi}, got {}",
                self.nx_game_abi
            )));
        }
        let mut seen = HashSet::with_capacity(self.files.len());
        for file in &self.files {
            validate_manifest_path(&file.path)?;
            if !seen.insert(file.path.as_str()) {
                return Err(Error::BadGameFolder(format!(
                    "duplicate file in manifest.json: {}",
                    file.path
                )));
            }
            if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::BadGameFolder(format!(
                    "invalid sha256 in manifest.json for {}",
                    file.path
                )));
            }
        }
        Ok(())
    }
}

fn validate_manifest_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains("..")
        || path.contains('\\')
        || path.starts_with("manifest.json")
        || path.starts_with("user/")
        || path.starts_with("root/user/")
    {
        return Err(Error::BadGameFolder(format!(
            "invalid file path in manifest.json: {path}"
        )));
    }
    Ok(())
}

/// Transport for fetching the game manifest and resumable file byte ranges.
pub trait DownloadTransport {
    /// Fetch the raw `manifest.json` bytes.
    fn fetch_manifest(&mut self) -> Result<Vec<u8>>;
    /// Fetch bytes for `path` starting at `offset` (HTTP `Range: bytes=<offset>-`),
    /// writing them into `out` and returning the number of bytes written.
    fn fetch_range(&mut self, path: &str, offset: u64, out: &mut dyn Write) -> Result<u64>;
}

/// Download the prepared game into `<private_dir>/game.partial/`, resume partial files via
/// `DownloadTransport::fetch_range`, verify every file's size and SHA-256, and finalize into
/// `dest` (`<private_dir>/game/`).
pub fn download(
    private_dir: &Path,
    dest: &Path,
    free_bytes: u64,
    expected_build_id: &str,
    expected_abi: &str,
    transport: &mut dyn DownloadTransport,
) -> Result<PathBuf> {
    check_dest_inside_private(private_dir, dest)?;

    let manifest_bytes = transport.fetch_manifest()?;
    let manifest = Manifest::from_json(&manifest_bytes)?;
    manifest.validate(expected_build_id, expected_abi)?;

    let partial = private_dir.join(PARTIAL_DIR);
    let mut needed_bytes: u64 = 0;
    for item in &manifest.files {
        let dst_file = partial.join(&item.path);
        needed_bytes = needed_bytes.saturating_add(remaining_bytes(&dst_file, item)?);
    }
    if free_bytes < needed_bytes {
        return Err(Error::Io(format!(
            "not enough free space: need {needed_bytes} bytes ({free_bytes} bytes available)"
        )));
    }

    fs::create_dir_all(&partial)?;
    for item in &manifest.files {
        download_one(&partial, item, transport)?;
    }

    let manifest_dst = partial.join("manifest.json");
    if manifest_dst.exists() {
        set_writable(&manifest_dst)?;
    }
    fs::write(&manifest_dst, &manifest_bytes)?;

    finalize_partial(&partial, dest)
}

fn remaining_bytes(dst_file: &Path, item: &ManifestFile) -> Result<u64> {
    let Ok(meta) = fs::metadata(dst_file) else {
        return Ok(item.size);
    };
    if !meta.is_file() {
        return Ok(item.size);
    }
    let len = meta.len();
    if len == item.size {
        if sha256_file(dst_file)? == item.sha256 {
            return Ok(0);
        }
        return Ok(item.size);
    }
    if len < item.size {
        return Ok(item.size - len);
    }
    Ok(item.size)
}

fn download_one(
    partial: &Path,
    item: &ManifestFile,
    transport: &mut dyn DownloadTransport,
) -> Result<()> {
    let dst_file = partial.join(&item.path);
    if let Ok(meta) = fs::metadata(&dst_file) {
        if meta.is_file() && meta.len() == item.size && sha256_file(&dst_file)? == item.sha256 {
            return Ok(());
        }
    }

    if let Some(parent) = dst_file.parent() {
        fs::create_dir_all(parent)?;
        set_writable(parent)?;
    }

    for attempt in 0..2 {
        if attempt > 0 && dst_file.exists() {
            set_writable(&dst_file)?;
            fs::remove_file(&dst_file)?;
        }
        if let Ok(meta) = fs::metadata(&dst_file) {
            if !meta.is_file() || meta.len() > item.size {
                set_writable(&dst_file)?;
                fs::remove_file(&dst_file)?;
            }
        }

        if item.size == 0 {
            File::create(&dst_file)?;
        } else {
            let mut have = fs::metadata(&dst_file).map(|m| m.len()).unwrap_or(0);
            while have < item.size {
                if dst_file.exists() {
                    set_writable(&dst_file)?;
                }
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&dst_file)?;
                let written = transport.fetch_range(&item.path, have, &mut file)?;
                file.flush()?;
                let new_len = fs::metadata(&dst_file)?.len();
                if written == 0 && new_len <= have {
                    return Err(Error::Io(format!(
                        "unexpected EOF downloading {} at offset {have}",
                        item.path
                    )));
                }
                have = new_len;
            }
        }

        let actual_len = fs::metadata(&dst_file)?.len();
        if actual_len == item.size && sha256_file(&dst_file)? == item.sha256 {
            return Ok(());
        }
        if attempt == 1 {
            let _ = fs::remove_file(&dst_file);
            return Err(Error::Io(format!(
                "{} failed SHA-256 verification twice",
                item.path
            )));
        }
    }
    unreachable!()
}

/// Compute lowercase hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn push_json_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

enum JsonVal {
    Str(String),
    Num(u64),
    Arr(Vec<JsonVal>),
    Obj(std::collections::HashMap<String, JsonVal>),
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, ch: u8) -> Result<()> {
        self.skip_ws();
        if self.bytes.get(self.pos).copied() == Some(ch) {
            self.pos += 1;
            Ok(())
        } else {
            Err(Error::BadGameFolder(format!(
                "manifest.json syntax error at byte {}",
                self.pos
            )))
        }
    }

    fn parse_val(&mut self) -> Result<JsonVal> {
        match self.peek() {
            Some(b'"') => Ok(JsonVal::Str(self.parse_str()?)),
            Some(b'[') => Ok(JsonVal::Arr(self.parse_array()?)),
            Some(b'{') => Ok(JsonVal::Obj(self.parse_object()?)),
            Some(b'0'..=b'9') => Ok(JsonVal::Num(self.parse_u64()?)),
            _ => Err(Error::BadGameFolder(format!(
                "manifest.json unexpected token at byte {}",
                self.pos
            ))),
        }
    }

    fn parse_object(&mut self) -> Result<std::collections::HashMap<String, JsonVal>> {
        self.expect(b'{')?;
        let mut map = std::collections::HashMap::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(map);
        }
        loop {
            let key = self.parse_str()?;
            self.expect(b':')?;
            let val = self.parse_val()?;
            map.insert(key, val);
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    return Err(Error::BadGameFolder(format!(
                        "manifest.json expected ',' or '}}' at byte {}",
                        self.pos
                    )))
                }
            }
        }
        Ok(map)
    }

    fn parse_array(&mut self) -> Result<Vec<JsonVal>> {
        self.expect(b'[')?;
        let mut arr = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(arr);
        }
        loop {
            arr.push(self.parse_val()?);
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    return Err(Error::BadGameFolder(format!(
                        "manifest.json expected ',' or ']' at byte {}",
                        self.pos
                    )))
                }
            }
        }
        Ok(arr)
    }

    fn parse_u64(&mut self) -> Result<u64> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| Error::BadGameFolder("invalid number in manifest.json".into()))?;
        s.parse::<u64>()
            .map_err(|_| Error::BadGameFolder(format!("invalid integer '{s}' in manifest.json")))
    }

    fn parse_str(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = *self
                        .bytes
                        .get(self.pos)
                        .ok_or_else(|| Error::BadGameFolder("unterminated string".into()))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            if self.pos + 4 > self.bytes.len() {
                                return Err(Error::BadGameFolder("bad \\u escape".into()));
                            }
                            let hex = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
                                .map_err(|_| Error::BadGameFolder("bad \\u escape".into()))?;
                            self.pos += 4;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| Error::BadGameFolder("bad \\u escape".into()))?;
                            let ch = char::from_u32(code)
                                .ok_or_else(|| Error::BadGameFolder("bad \\u codepoint".into()))?;
                            out.push(ch);
                        }
                        _ => {
                            return Err(Error::BadGameFolder(
                                "unsupported escape in manifest.json".into(),
                            ))
                        }
                    }
                }
                _ => {
                    // Decode UTF-8 sequence starting at self.pos - 1.
                    let rest = std::str::from_utf8(&self.bytes[self.pos - 1..])
                        .or_else(|e| {
                            std::str::from_utf8(
                                &self.bytes[self.pos - 1..self.pos - 1 + e.valid_up_to()],
                            )
                        })
                        .map_err(|_| {
                            Error::BadGameFolder("invalid UTF-8 in manifest.json".into())
                        })?;
                    let ch = rest.chars().next().ok_or_else(|| {
                        Error::BadGameFolder("invalid UTF-8 in manifest.json".into())
                    })?;
                    self.pos += ch.len_utf8() - 1;
                    out.push(ch);
                }
            }
        }
        Err(Error::BadGameFolder(
            "unterminated string in manifest.json".into(),
        ))
    }
}

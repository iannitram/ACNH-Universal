//! The cloud-folder stream of a prepared game: Dropbox upload and download over `ureq`.

use super::*;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

const DROPBOX_CONTENT_URL: &str = "https://content.dropboxapi.com/2/files";
const DROPBOX_RPC_URL: &str = "https://api.dropboxapi.com/2/files";
const IO_BUF_SIZE: usize = 1024 * 1024;
const UPLOAD_CHUNK_SIZE: usize = 64 * 1024 * 1024;
const DROPBOX_HASH_BLOCK: usize = 4 * 1024 * 1024;
const NOT_FOUND_MSG: &str = "No prepared game for this platform was found in your cloud folder yet. You can choose a local game folder instead.";

fn dropbox_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(300))
        .timeout_write(Duration::from_secs(300))
        .build()
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
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
    out.push('"');
    out
}

fn extract_json_str(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = body.find(&needle)?;
    let rest = &body[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after_colon = rest[colon + 1..].trim_start();
    let after_quote = after_colon.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    Some(after_quote[..end].to_owned())
}

fn extract_json_u64(body: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"");
    let idx = body.find(&needle)?;
    let rest = &body[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after_colon = rest[colon + 1..].trim_start();
    let digits: String = after_colon
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Streaming [`crate::appgame::DownloadTransport`] against Dropbox's content API.
pub struct DropboxStreamTransport<'a> {
    tokens: &'a save_relay::oauth::TokenSource,
    base: String,
    agent: ureq::Agent,
}

impl<'a> DropboxStreamTransport<'a> {
    pub fn new(tokens: &'a save_relay::oauth::TokenSource, base: impl Into<String>) -> Self {
        Self {
            tokens,
            base: base.into().trim_matches('/').to_owned(),
            agent: dropbox_agent(),
        }
    }

    fn api_path(&self, rel: &str) -> String {
        let rel = rel.trim_start_matches('/');
        if self.base.is_empty() {
            format!("/{rel}")
        } else {
            format!("/{}/{rel}", self.base)
        }
    }

    fn download_request(&self, api_path: &str, offset: u64) -> Result<ureq::Response> {
        let arg = format!("{{\"path\":{}}}", json_escape(api_path));
        let url = format!("{DROPBOX_CONTENT_URL}/download");
        for attempt in 0..2 {
            let token = self.tokens.bearer().map_err(map_err)?;
            let mut req = self
                .agent
                .post(&url)
                .set("Authorization", &format!("Bearer {}", token.expose()))
                .set("Dropbox-API-Arg", &arg);
            if offset > 0 {
                req = req.set("Range", &format!("bytes={offset}-"));
            }
            match req.call() {
                Ok(resp) => return Ok(resp),
                Err(ureq::Error::Status(401, _)) if attempt == 0 => {
                    self.tokens.invalidate();
                }
                Err(ureq::Error::Status(409, resp)) => {
                    let body = resp.into_string().unwrap_or_default();
                    if body.contains("not_found") {
                        return Err(Error::Io(NOT_FOUND_MSG.into()));
                    }
                    return Err(Error::Saves(format!("dropbox download conflict: {body}")));
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let body = resp.into_string().unwrap_or_default();
                    return Err(Error::Saves(format!(
                        "dropbox download failed (HTTP {code}): {body}"
                    )));
                }
                Err(ureq::Error::Transport(t)) => {
                    return Err(Error::Saves(format!("dropbox download error: {t}")));
                }
            }
        }
        Err(Error::RelaySignIn)
    }
}

impl crate::appgame::DownloadTransport for DropboxStreamTransport<'_> {
    fn fetch_manifest(&mut self) -> Result<Vec<u8>> {
        let path = self.api_path("manifest.json");
        let resp = self.download_request(&path, 0)?;
        let mut out = Vec::new();
        resp.into_reader().read_to_end(&mut out)?;
        Ok(out)
    }

    fn fetch_range(&mut self, path: &str, offset: u64, out: &mut dyn Write) -> Result<u64> {
        let api_path = self.api_path(path);
        let resp = self.download_request(&api_path, offset)?;
        let mut reader = resp.into_reader();
        let mut buf = vec![0u8; IO_BUF_SIZE];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            total = total.saturating_add(n as u64);
        }
        Ok(total)
    }
}

struct FileHashes {
    size: u64,
    sha256: String,
    dropbox_content_hash: String,
}

fn hash_file_streaming(path: &Path) -> Result<FileHashes> {
    let mut file = File::open(path)?;
    let mut sha = Sha256::new();
    let mut block_hashes = Vec::new();
    let mut block_hasher = Sha256::new();
    let mut block_pos = 0usize;
    let mut total = 0u64;
    let mut buf = vec![0u8; IO_BUF_SIZE];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        sha.update(&buf[..n]);

        let mut slice = &buf[..n];
        while !slice.is_empty() {
            let take = (DROPBOX_HASH_BLOCK - block_pos).min(slice.len());
            block_hasher.update(&slice[..take]);
            block_pos += take;
            slice = &slice[take..];
            if block_pos == DROPBOX_HASH_BLOCK {
                block_hashes.extend_from_slice(&block_hasher.finalize_reset());
                block_pos = 0;
            }
        }
    }
    if block_pos > 0 || total == 0 {
        block_hashes.extend_from_slice(&block_hasher.finalize());
    }
    let content_digest = Sha256::digest(&block_hashes);
    let to_hex = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
    Ok(FileHashes {
        size: total,
        sha256: to_hex(&sha.finalize()),
        dropbox_content_hash: to_hex(&content_digest),
    })
}

fn dropbox_metadata_matches(
    agent: &ureq::Agent,
    tokens: &save_relay::oauth::TokenSource,
    api_path: &str,
    expected_size: u64,
    expected_content_hash: &str,
) -> Result<bool> {
    let url = format!("{DROPBOX_RPC_URL}/get_metadata");
    let body = format!("{{\"path\":{}}}", json_escape(api_path));
    for attempt in 0..2 {
        let token = tokens.bearer().map_err(map_err)?;
        match agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", token.expose()))
            .set("Content-Type", "application/json")
            .send_string(&body)
        {
            Ok(resp) => {
                let text = resp.into_string().unwrap_or_default();
                let size_ok = extract_json_u64(&text, "size") == Some(expected_size);
                let hash_ok = extract_json_str(&text, "content_hash")
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(expected_content_hash));
                return Ok(size_ok && hash_ok);
            }
            Err(ureq::Error::Status(401, _)) if attempt == 0 => {
                tokens.invalidate();
            }
            Err(ureq::Error::Status(409, _)) => return Ok(false),
            Err(_) => return Ok(false),
        }
    }
    Ok(false)
}

fn send_dropbox_content(
    agent: &ureq::Agent,
    tokens: &save_relay::oauth::TokenSource,
    endpoint: &str,
    api_arg: &str,
    bytes: &[u8],
) -> Result<String> {
    let url = format!("{DROPBOX_CONTENT_URL}/{endpoint}");
    let mut last_err = None;
    for attempt in 0..4 {
        let token = tokens.bearer().map_err(map_err)?;
        match agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", token.expose()))
            .set("Dropbox-API-Arg", api_arg)
            .set("Content-Type", "application/octet-stream")
            .send_bytes(bytes)
        {
            Ok(resp) => return Ok(resp.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(401, _)) if attempt == 0 => {
                tokens.invalidate();
            }
            Err(ureq::Error::Status(code, resp)) if code == 429 || code >= 500 => {
                let body = resp.into_string().unwrap_or_default();
                last_err = Some(format!("HTTP {code}: {body}"));
                std::thread::sleep(Duration::from_secs(1 << attempt));
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                return Err(Error::Saves(format!(
                    "dropbox {endpoint} failed (HTTP {code}): {body}"
                )));
            }
            Err(ureq::Error::Transport(t)) => {
                last_err = Some(t.to_string());
                std::thread::sleep(Duration::from_secs(1 << attempt));
            }
        }
    }
    Err(Error::Saves(format!(
        "dropbox {endpoint} failed after retries: {}",
        last_err.unwrap_or_default()
    )))
}

fn upload_file_to_dropbox(
    agent: &ureq::Agent,
    tokens: &save_relay::oauth::TokenSource,
    api_path: &str,
    local_path: &Path,
    size: u64,
) -> Result<()> {
    if size <= UPLOAD_CHUNK_SIZE as u64 {
        let mut buf = Vec::with_capacity(size as usize);
        File::open(local_path)?.read_to_end(&mut buf)?;
        let arg = format!(
            "{{\"path\":{},\"mode\":\"overwrite\",\"mute\":true}}",
            json_escape(api_path)
        );
        send_dropbox_content(agent, tokens, "upload", &arg, &buf)?;
        return Ok(());
    }

    let mut file = File::open(local_path)?;
    let mut buf = vec![0u8; UPLOAD_CHUNK_SIZE];
    let first_len = read_exact_or_eof(&mut file, &mut buf)?;
    let start_resp = send_dropbox_content(
        agent,
        tokens,
        "upload_session/start",
        "{\"close\":false}",
        &buf[..first_len],
    )?;
    let session_id = extract_json_str(&start_resp, "session_id").ok_or_else(|| {
        Error::Saves(format!(
            "dropbox upload_session/start missing session_id: {start_resp}"
        ))
    })?;
    let mut offset = first_len as u64;

    while offset < size {
        file.seek(SeekFrom::Start(offset))?;
        let n = read_exact_or_eof(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        let next_offset = offset + n as u64;
        if next_offset >= size {
            let arg = format!(
                "{{\"cursor\":{{\"session_id\":{},\"offset\":{offset}}},\"commit\":{{\"path\":{},\"mode\":\"overwrite\",\"mute\":true}}}}",
                json_escape(&session_id),
                json_escape(api_path)
            );
            send_dropbox_content(agent, tokens, "upload_session/finish", &arg, &buf[..n])?;
        } else {
            let arg = format!(
                "{{\"cursor\":{{\"session_id\":{},\"offset\":{offset}}},\"close\":false}}",
                json_escape(&session_id)
            );
            send_dropbox_content(agent, tokens, "upload_session/append_v2", &arg, &buf[..n])?;
        }
        offset = next_offset;
    }
    Ok(())
}

fn read_exact_or_eof(file: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Upload a set of `(relative_path, local_path)` files to `/acnh-universal-game/<os>/`
/// in the user's Dropbox app folder, skipping unchanged files and writing `manifest.json` last.
pub fn upload_game_to_dropbox(
    relay: &SaveRelay,
    os: &str,
    files: &[(String, PathBuf)],
    build_id: &str,
    nx_game_abi: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<crate::appgame::Manifest> {
    // Refused before anything is uploaded: a manifest without a build id and ABI is useless.
    crate::appgame::Manifest::check_stamp(build_id, nx_game_abi)?;
    let tokens = relay
        .dropbox_tokens()
        .ok_or_else(|| Error::Saves("save-relay is not signed in to Dropbox".into()))?;
    let agent = dropbox_agent();
    let base = format!("/acnh-universal-game/{}", os.trim_matches('/'));
    let mut manifest_files = Vec::with_capacity(files.len());

    for (rel, local_path) in files {
        let hashes = hash_file_streaming(local_path)?;
        let api_path = format!("{base}/{}", rel.trim_start_matches('/'));
        if dropbox_metadata_matches(
            &agent,
            tokens,
            &api_path,
            hashes.size,
            &hashes.dropbox_content_hash,
        )? {
            progress(&format!("skipped {rel} ({} bytes, unchanged)", hashes.size));
        } else {
            progress(&format!(
                "uploading {rel} ({:.2} MB)...",
                hashes.size as f64 / 1_000_000.0
            ));
            upload_file_to_dropbox(&agent, tokens, &api_path, local_path, hashes.size)?;
        }
        manifest_files.push(crate::appgame::ManifestFile {
            path: rel.clone(),
            size: hashes.size,
            sha256: hashes.sha256,
        });
    }

    let manifest = crate::appgame::Manifest::new(build_id, nx_game_abi, manifest_files)?;
    let manifest_path = format!("{base}/manifest.json");
    let manifest_arg = format!(
        "{{\"path\":{},\"mode\":\"overwrite\",\"mute\":true}}",
        json_escape(&manifest_path)
    );
    progress("uploading manifest.json...");
    send_dropbox_content(
        &agent,
        tokens,
        "upload",
        &manifest_arg,
        manifest.to_json().as_bytes(),
    )?;
    Ok(manifest)
}

fn walk_dir_sorted(root: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(root)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".DS_Store" {
            continue;
        }
        let path = entry.path();
        let rel = format!("{prefix}/{name_str}");
        if path.is_dir() {
            walk_dir_sorted(&path, &rel, out)?;
        } else if path.is_file() {
            out.push((rel, path));
        }
    }
    Ok(())
}

/// Collect the Linux install (`acnh` + shared libraries) and prepared `root/` assets
/// (`romfs.pak`, `spirv.pak`, `analysis/`, `fonts/`, `aoc/`) for cloud staging.
pub fn collect_linux_cloud_files(
    install_dir: &Path,
    prepared_dir: &Path,
) -> Result<Vec<(String, PathBuf)>> {
    let acnh = install_dir.join("acnh");
    if !acnh.is_file() {
        return Err(Error::BadGameFolder(format!(
            "missing Linux player binary: {}",
            acnh.display()
        )));
    }
    for req in ["analysis/main.bin", "romfs.pak", "spirv.pak"] {
        let p = prepared_dir.join(req);
        if !p.is_file() {
            return Err(Error::BadGameFolder(format!(
                "missing prepared file: {}",
                p.display()
            )));
        }
    }

    let mut files = Vec::new();
    files.push(("acnh".into(), acnh));
    for entry in std::fs::read_dir(install_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("lib") && name_str.contains(".so") && entry.path().is_file() {
            files.push((name_str.into_owned(), entry.path()));
        }
    }
    for pak in ["romfs.pak", "spirv.pak"] {
        files.push((format!("root/{pak}"), prepared_dir.join(pak)));
    }
    for dir in ["analysis", "fonts", "aoc"] {
        walk_dir_sorted(&prepared_dir.join(dir), &format!("root/{dir}"), &mut files)?;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

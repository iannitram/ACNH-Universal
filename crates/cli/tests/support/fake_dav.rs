//! A Nextcloud server in memory, for tests: WebDAV over save-relay's `MemoryRemote`, behind its
//! `Transport`. User `alex`; `set_online(false)` makes every request unreachable.
#![allow(dead_code)]

use save_relay::http::{Request, Response, Transport};
use save_relay::remote::{MemoryRemote, Precondition, Remote};
use save_relay::webdav::{encode_path, encode_segment};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const DAV_PREFIX: &str = "/remote.php/dav/files/alex/";

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map_or(0, |b| u32::from(*b));
        let b2 = chunk.get(2).map_or(0, |b| u32::from(*b));
        let bits = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((bits >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((bits >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((bits >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(bits & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn dav_path(url: &str) -> String {
    let rest = url.split_once(DAV_PREFIX).map_or("", |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    percent_decode(rest)
}

fn error_response(error: save_relay::Error) -> Response {
    match error {
        save_relay::Error::NotFound(_) => Response::new(404, ""),
        save_relay::Error::AuthFailed => Response::new(401, ""),
        save_relay::Error::Changed => Response::new(412, ""),
        save_relay::Error::Server(message) if message == "exists" => Response::new(412, ""),
        _ => Response::new(500, ""),
    }
}

pub struct FakeDav {
    pub remote: MemoryRemote,
    online: AtomicBool,
    auth: String,
    log: Mutex<Vec<Request>>,
}

impl FakeDav {
    pub fn new(password: &str) -> FakeDav {
        FakeDav {
            remote: MemoryRemote::new(),
            online: AtomicBool::new(true),
            auth: format!("Basic {}", base64(format!("alex:{password}").as_bytes())),
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn set_online(&self, online: bool) {
        self.online.store(online, Ordering::SeqCst);
    }

    pub fn file_paths(&self) -> Vec<String> {
        self.remote.file_paths()
    }

    pub fn sent(&self) -> Vec<Request> {
        self.log.lock().unwrap().clone()
    }

    fn propfind(&self, dir: &str) -> save_relay::Result<Response> {
        let entries = match self.remote.list(dir) {
            Ok(entries) => entries,
            Err(save_relay::Error::NotFound(path)) => {
                return Ok(Response::new(404, path));
            }
            Err(error) => return Ok(error_response(error)),
        };
        let own = format!("{DAV_PREFIX}{}", encode_path(dir));
        let own = own.trim_end_matches('/');
        let mut xml =
            String::from(r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:">"#);
        for entry in entries {
            let href = if entry.is_dir {
                format!("{own}/{}/", encode_segment(&entry.name))
            } else {
                format!("{own}/{}", encode_segment(&entry.name))
            };
            xml.push_str("<d:response><d:href>");
            xml.push_str(&href);
            xml.push_str("</d:href><d:propstat><d:prop><d:resourcetype>");
            if entry.is_dir {
                xml.push_str("<d:collection/>");
            }
            xml.push_str("</d:resourcetype>");
            if !entry.is_dir {
                xml.push_str("<d:getcontentlength>");
                xml.push_str(&entry.size.to_string());
                xml.push_str("</d:getcontentlength>");
            }
            if let Some(etag) = entry.etag {
                xml.push_str("<d:getetag>");
                xml.push_str(&etag);
                xml.push_str("</d:getetag>");
            }
            xml.push_str("</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
        }
        xml.push_str("</d:multistatus>");
        Ok(Response::new(207, xml).header("Content-Type", "application/xml"))
    }
}

impl Transport for FakeDav {
    fn send(&self, request: &Request) -> save_relay::Result<Response> {
        self.log.lock().unwrap().push(request.clone());
        if !self.online.load(Ordering::SeqCst) {
            return Err(save_relay::Error::Unreachable(
                "the fake server is off".into(),
            ));
        }
        if request.header_value("Authorization") != Some(self.auth.as_str()) {
            return Ok(Response::new(401, ""));
        }
        let path = dav_path(&request.url);
        match request.method.as_str() {
            "PROPFIND" => self.propfind(&path),
            "GET" => match self.remote.get(&path) {
                Ok((body, etag)) => Ok(Response::new(200, body).header("ETag", &etag)),
                Err(error) => Ok(error_response(error)),
            },
            "PUT" => {
                let pre = match request.header_value("If-None-Match") {
                    Some("*") => Precondition::Absent,
                    Some(etag) => Precondition::Match(etag.to_string()),
                    None => match request.header_value("If-Match") {
                        Some(etag) => Precondition::Match(etag.to_string()),
                        None => Precondition::Any,
                    },
                };
                match self.remote.put(&path, &request.body, &pre) {
                    Ok(etag) => Ok(Response::new(201, "").header("ETag", &etag)),
                    Err(error) => Ok(error_response(error)),
                }
            }
            "MKCOL" => match self.remote.mkcol(&path) {
                Ok(()) => Ok(Response::new(201, "")),
                Err(save_relay::Error::NotFound(_)) => Ok(Response::new(409, "")),
                Err(error) => Ok(error_response(error)),
            },
            "COPY" | "MOVE" => {
                let to = request
                    .header_value("Destination")
                    .map(dav_path)
                    .unwrap_or_default();
                let result = if request.method == "COPY" {
                    self.remote.copy(&path, &to)
                } else {
                    self.remote.move_to(&path, &to)
                };
                match result {
                    Ok(()) => Ok(Response::new(201, "")),
                    Err(save_relay::Error::NotFound(from)) if from == path => {
                        Ok(Response::new(404, ""))
                    }
                    Err(save_relay::Error::NotFound(_)) => Ok(Response::new(409, "")),
                    Err(error) => Ok(error_response(error)),
                }
            }
            "DELETE" => match self.remote.delete(&path) {
                Ok(()) => Ok(Response::new(204, "")),
                Err(error) => Ok(error_response(error)),
            },
            _ => Ok(Response::new(405, "")),
        }
    }
}

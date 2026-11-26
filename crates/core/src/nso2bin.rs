//! An NSO as the flat image the recompiler reads (issue 1.28): read the segment table,
//! LZ4-decompress each segment whose flag says it is compressed, and copy every segment to
//! its memory offset. Replaces upstream's `tools/nso2bin.py`, byte for byte.
//!
//! An NSO starts with `NSO0`. The flags are a u32 at 0x0C; bit `i` means segment `i` is an
//! LZ4 raw block. The three segment entries start at 0x10, each 16 bytes: file offset,
//! memory offset and decompressed size (u32 each); the bss size is at 0x3C; the sizes of the
//! segments as stored in the file are at 0x60 (u32 each). The image ends after the last byte
//! of the last segment, and gaps between segments are zero; bss is not part of the image.
//!
//! LZ4 is decoded with `lz4_flex`, a pure-Rust block decoder: the core keeps its no-C-library,
//! every-target rule (SPEC.md M13) and needs no `unsafe` of its own. The header's size is the
//! decoder's output size, so NSO blocks carry no size prefix. Never logs segment contents.

use crate::error::{Error, Result};
use std::fs;
use std::path::Path;

/// How many segments an NSO has (text, ro, data).
pub const SEGMENTS: usize = 3;

const MAGIC: &[u8; 4] = b"NSO0";
const HEADER_LEN: usize = 0x6C;
const FLAGS: usize = 0x0C;
const SEGMENT_TABLE: usize = 0x10;
const SEGMENT_STRIDE: usize = 0x10;
const BSS_SIZE: usize = 0x3C;
const COMPRESSED_SIZES: usize = 0x60;

/// One NSO segment as it sits in the flat image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Where the segment starts in the image.
    pub memory_offset: u32,
    /// The segment's bytes, decompressed.
    pub bytes: Vec<u8>,
}

/// An NSO's three segments and its bss size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nso {
    pub segments: [Segment; SEGMENTS],
    pub bss_size: u32,
}

impl Nso {
    /// The flat image: zero-filled up to the last byte of the farthest segment, with each
    /// segment copied to its memory offset (the layout `tools/nso2bin.py` writes).
    pub fn image(&self) -> Result<Vec<u8>> {
        let mut end = 0usize;
        for segment in &self.segments {
            let start = segment.memory_offset as usize;
            let stop = start
                .checked_add(segment.bytes.len())
                .ok_or_else(image_too_large)?;
            end = end.max(stop);
        }

        let mut image = vec![0u8; end];
        for segment in &self.segments {
            let start = segment.memory_offset as usize;
            image[start..start + segment.bytes.len()].copy_from_slice(&segment.bytes);
        }
        Ok(image)
    }
}

/// Reads an NSO's segments and bss size. Not an NSO, a header too short for the segment
/// table, a segment that runs past the end of the file, or bad LZ4 data is `Error::NotAcnh`.
pub fn parse(raw: &[u8]) -> Result<Nso> {
    if raw.len() < HEADER_LEN {
        return Err(not_nso(
            "the file is too short to hold an NSO segment table",
        ));
    }
    if &raw[..4] != MAGIC {
        return Err(not_nso("not a Switch executable (NSO)"));
    }

    let flags = u32_at(raw, FLAGS);
    let mut segments: [Segment; SEGMENTS] = std::array::from_fn(|_| Segment {
        memory_offset: 0,
        bytes: Vec::new(),
    });

    for (i, segment) in segments.iter_mut().enumerate() {
        let at = SEGMENT_TABLE + i * SEGMENT_STRIDE;
        let file_offset = u32_at(raw, at) as usize;
        let memory_offset = u32_at(raw, at + 4);
        let size = u32_at(raw, at + 8) as usize;
        let compressed_size = u32_at(raw, COMPRESSED_SIZES + i * 4) as usize;

        let end = file_offset
            .checked_add(compressed_size)
            .ok_or_else(|| not_nso("a segment's file range overflows"))?;
        if end > raw.len() {
            return Err(not_nso(format!(
                "segment {i} needs bytes {file_offset:#x}..{end:#x}, but the file is {:#x} bytes",
                raw.len()
            )));
        }
        let blob = &raw[file_offset..end];

        let bytes = if flags & (1 << i) != 0 {
            let mut bytes = vec![0u8; size];
            let written = lz4_flex::block::decompress_into(blob, &mut bytes).map_err(|error| {
                not_nso(format!("segment {i} is not a valid LZ4 block: {error}"))
            })?;
            if written != size {
                return Err(not_nso(format!(
                    "segment {i} decompressed to {written:#x} bytes, the header says {size:#x}"
                )));
            }
            bytes
        } else {
            blob.to_vec()
        };

        *segment = Segment {
            memory_offset,
            bytes,
        };
    }

    Ok(Nso {
        segments,
        bss_size: u32_at(raw, BSS_SIZE),
    })
}

/// The flat image of `raw`, as `Nso::image`.
pub fn image(raw: &[u8]) -> Result<Vec<u8>> {
    parse(raw)?.image()
}

/// The flat image of the NSO at `input`, written at `output`.
pub fn write(input: &Path, output: &Path) -> Result<()> {
    let raw = fs::read(input)?;
    crate::platform::atomic_write(output, &image(&raw)?)
}

fn u32_at(raw: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
}

fn not_nso(message: impl Into<String>) -> Error {
    Error::NotAcnh(message.into())
}

fn image_too_large() -> Error {
    not_nso("the NSO's segments are too large to lay out in memory")
}

//! Reflow optional translation dialogue during Prepare (1.30).
//!
//! Ported from `tools/msg_rewrap.py`. The text is measured in 1/64-pixel units at
//! the original 100-pixel font size. Control tags are kept with adjacent words;
//! explicit page breaks, unrelated sections and archives are preserved.

use crate::error::{Error, Result};
use crate::sarc::{self, Endian};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

type Line = Vec<Item>;

#[derive(Clone, PartialEq, Eq)]
enum Item {
    Unit(u16),
    Tag(Vec<u16>),
}

impl Item {
    fn is_space(&self) -> bool {
        matches!(self, Self::Unit(32))
    }
}

#[derive(Clone, Copy)]
enum Rule {
    Wrap(i64),
    Fit(i64),
}

fn rule(archive: &str, member: &str) -> Option<Rule> {
    let pixels = 64;
    if archive.starts_with("Talk") || archive.starts_with("Tutorials_") {
        Some(Rule::Wrap(2020 * pixels))
    } else if archive.starts_with("Mail_") {
        Some(Rule::Wrap(1910 * pixels))
    } else if archive.starts_with("System_") && member.starts_with("LifeSupport/") {
        Some(Rule::Fit(3300 * pixels))
    } else {
        None
    }
}

fn message_error(reason: &str) -> Error {
    Error::Io(format!("invalid MSBT: {reason}"))
}

fn pages(raw: &[u8], endian: Endian) -> Result<Vec<Vec<Line>>> {
    if !raw.len().is_multiple_of(2) {
        return Err(message_error("a UTF-16 string has an odd length"));
    }
    let units: Vec<_> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| endian.decode_u16(b))
        .collect();
    let mut pages = Vec::new();
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut i = 0;
    while let Some(&unit) = units.get(i) {
        match unit {
            0x0e | 0x0f => {
                let size = match unit {
                    0x0e => {
                        let payload = units
                            .get(i + 3)
                            .ok_or_else(|| message_error("short control tag"))?;
                        4 + (*payload as usize / 2)
                    }
                    _ => 3,
                };
                let tag = units
                    .get(i..i + size)
                    .ok_or_else(|| message_error("short control tag"))?;
                line.push(Item::Tag(tag.to_vec()));
                if tag[..3] == [0x0e, 0, 4] {
                    lines.push(std::mem::take(&mut line));
                    pages.push(std::mem::take(&mut lines));
                }
                i += size;
            }
            10 => {
                lines.push(std::mem::take(&mut line));
                i += 1;
            }
            _ => {
                line.push(Item::Unit(unit));
                i += 1;
            }
        }
    }
    lines.push(line);
    pages.push(lines);
    Ok(pages)
}

fn encode(pages: &[Vec<Line>], endian: Endian) -> Vec<u8> {
    let mut bytes = Vec::new();
    for lines in pages {
        for (index, line) in lines.iter().enumerate() {
            if index != 0 {
                bytes.extend(endian.encode_u16(10));
            }
            for item in line {
                match item {
                    Item::Unit(unit) => bytes.extend(endian.encode_u16(*unit)),
                    Item::Tag(tag) => {
                        bytes.extend(tag.iter().flat_map(|unit| endian.encode_u16(*unit)));
                    }
                }
            }
        }
    }
    bytes
}

fn whitespace(unit: u16) -> bool {
    // Python's isspace also includes the four information separators.
    (0x1c..=0x1f).contains(&unit) || char::from_u32(unit.into()).is_some_and(char::is_whitespace)
}

fn tag_text(tag: &[u16], visible: &mut Vec<u16>) {
    let placeholder = match tag[..3] {
        [0x0e, 110, 0] => "...",
        [0x0e, 50, _] if tag.len() > 4 => {
            let mut longest = &[][..];
            for option in tag[4..].split(|unit| !(32..0xe000).contains(unit)) {
                if option.len() > longest.len() {
                    longest = option;
                }
            }
            visible.extend_from_slice(longest);
            return;
        }
        [0x0e, 90, _] => "000",
        [0x0e, 50 | 110 | 115 | 125 | 135 | 140 | 150, _] => "Nnnnnn",
        _ => "",
    };
    visible.extend(placeholder.encode_utf16());
}

fn width(line: &[Item], advance: &mut impl FnMut(u16) -> Result<i64>) -> Result<i64> {
    let mut visible = Vec::new();
    for item in line {
        match item {
            Item::Tag(tag) => tag_text(tag, &mut visible),
            Item::Unit(unit) if *unit != 0 => visible.push(*unit),
            Item::Unit(_) => {}
        }
    }
    let start = visible
        .iter()
        .position(|&unit| !whitespace(unit))
        .unwrap_or(visible.len());
    let end = visible
        .iter()
        .rposition(|&unit| !whitespace(unit))
        .map_or(start, |i| i + 1);
    visible[start..end]
        .iter()
        .try_fold(0, |width, &unit| Ok(width + advance(unit)?))
}

fn visible_units(line: &[Item]) -> impl DoubleEndedIterator<Item = u16> + '_ {
    line.iter().filter_map(|item| match item {
        Item::Unit(unit) if *unit != 0 => Some(*unit),
        _ => None,
    })
}

fn ends_sentence(line: &[Item]) -> bool {
    let last = line.iter().rfind(|item| match item {
        Item::Unit(unit) => !matches!(unit, 0 | 32),
        Item::Tag(tag) => !matches!(tag[1], 10 | 40),
    });
    if matches!(last, Some(Item::Tag(tag)) if tag[1..3] == [110, 0]) {
        return true;
    }
    matches!(
        visible_units(line).rev().find(|&unit| !whitespace(unit)),
        Some(46 | 33 | 63 | 0x2026 | 58)
    )
}

fn words(lines: &[Line]) -> Vec<&[Item]> {
    lines
        .iter()
        .flat_map(|line| line.split(Item::is_space))
        .filter(|word| !word.is_empty())
        .collect()
}

fn join(words: &[&[Item]]) -> Line {
    let mut line = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if index != 0 {
            line.push(Item::Unit(32));
        }
        line.extend_from_slice(word);
    }
    line
}

/// The narrowest widest line for an exact line count. Traverse cuts in their original
/// order and keep the first equal-cost cut, as the Python implementation does.
fn balance(
    words: &[&[Item]],
    widths: &[i64],
    lines: usize,
    space: i64,
    limit: i64,
) -> Option<Vec<Line>> {
    if words.len() < lines {
        return None;
    }
    let mut costs = vec![None; words.len() + 1];
    costs[0] = Some(0);
    let mut cuts = Vec::with_capacity(lines);
    for line in 1..=lines {
        let mut next = vec![None; costs.len()];
        let mut starts = vec![0; costs.len()];
        for end in line..=words.len() {
            for (start, previous) in costs.iter().enumerate().take(end).skip(line - 1) {
                let Some(previous) = previous else { continue };
                let wide = widths[end] - widths[start] + space * (end - start - 1) as i64;
                if wide > limit && end - start > 1 {
                    continue;
                }
                let cost = (*previous).max(wide);
                if next[end].is_none_or(|old| cost < old) {
                    next[end] = Some(cost);
                    starts[end] = start;
                }
            }
        }
        costs = next;
        cuts.push(starts);
    }
    costs[words.len()]?;
    let mut end = words.len();
    let mut wrapped = Vec::with_capacity(lines);
    for starts in cuts.iter().rev() {
        let start = starts[end];
        wrapped.push(join(&words[start..end]));
        end = start;
    }
    wrapped.reverse();
    Some(wrapped)
}

fn fit(
    words: &[&[Item]],
    lengths: std::ops::Range<usize>,
    space: i64,
    limit: i64,
    advance: &mut impl FnMut(u16) -> Result<i64>,
) -> Result<Option<Vec<Line>>> {
    if lengths.is_empty() {
        return Ok(None);
    }
    let mut widths = Vec::with_capacity(words.len() + 1);
    widths.push(0);
    let mut total = 0;
    for word in words {
        total += width(word, advance)?;
        widths.push(total);
    }
    Ok(lengths
        .filter(|count| *count <= words.len())
        .find_map(|count| balance(words, &widths, count, space, limit)))
}

fn reflow(
    raw: &[u8],
    endian: Endian,
    rule: Rule,
    advance: &mut impl FnMut(u16) -> Result<i64>,
) -> Result<Option<Vec<u8>>> {
    let mut pages = pages(raw, endian)?;
    let space = advance(32)?;
    let mut changed = false;
    for lines in &mut pages {
        match rule {
            Rule::Fit(limit) => {
                if let Some(output) = fit(&words(lines), 1..lines.len(), space, limit, advance)? {
                    *lines = output;
                    changed = true;
                }
            }
            Rule::Wrap(limit) => {
                let mut at = 0;
                while at < lines.len() {
                    if width(&lines[at], advance)? <= limit {
                        at += 1;
                        continue;
                    }
                    let mut end = at + 1;
                    while end < lines.len()
                        && end - at < 4
                        && !ends_sentence(&lines[end - 1])
                        && visible_units(&lines[end]).any(|unit| !whitespace(unit))
                    {
                        end += 1;
                    }
                    let count = end - at;
                    if let Some(output) = fit(
                        &words(&lines[at..end]),
                        count..count + 4,
                        space,
                        limit,
                        advance,
                    )? {
                        let len = output.len();
                        lines.splice(at..end, output);
                        at += len;
                        changed = true;
                    } else {
                        at += 1;
                    }
                }
            }
        }
    }
    Ok(changed.then(|| encode(&pages, endian)))
}

struct TextSection<'a> {
    start: usize,
    end: usize,
    endian: Endian,
    strings: Vec<&'a [u8]>,
}

fn read_u32(bytes: &[u8], offset: usize, endian: Endian) -> Result<usize> {
    let field = bytes.get(offset..).and_then(|suffix| suffix.first_chunk());
    field
        .map(|field| endian.decode_u32(*field) as usize)
        .ok_or_else(|| message_error("short section or table"))
}

fn field_u32(value: usize, endian: Endian) -> Result<[u8; 4]> {
    let value = u32::try_from(value).map_err(|_| message_error("message file is too large"))?;
    Ok(endian.encode_u32(value))
}

fn padded(value: usize) -> Result<usize> {
    value
        .checked_next_multiple_of(16)
        .ok_or_else(|| message_error("section is too large"))
}

impl<'a> TextSection<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Option<Self>> {
        let endian = match bytes.get(8..10) {
            Some([0xff, 0xfe]) => Endian::Little,
            Some([0xfe, 0xff]) => Endian::Big,
            _ => return Err(message_error("unknown byte order")),
        };
        let header = bytes
            .first_chunk::<32>()
            .ok_or_else(|| message_error("short header"))?;
        let sections = endian.decode_u16([header[14], header[15]]);
        let mut start = 32;
        for _ in 0..sections {
            let size = read_u32(bytes, start + 4, endian)?;
            let base = start + 16;
            let unpadded = base
                .checked_add(size)
                .ok_or_else(|| message_error("section is too large"))?;
            let end = padded(unpadded)?;
            if unpadded > bytes.len() {
                return Err(message_error("section goes past the file"));
            }
            let end = end.min(bytes.len());
            if bytes.get(start..start + 4) != Some(b"TXT2") {
                start = end;
                continue;
            }
            let text = &bytes[base..unpadded];
            let count = read_u32(text, 0, endian)?;
            if count > (text.len() - 4) / 4 {
                return Err(message_error("truncated string table"));
            }
            let mut previous = 4 + count * 4;
            let mut strings = Vec::with_capacity(count);
            for index in 0..count {
                let offset = read_u32(text, 4 + index * 4, endian)?;
                if offset < previous || offset > text.len() {
                    return Err(message_error("invalid string offset"));
                }
                if index != 0 {
                    strings.push(&text[previous..offset]);
                }
                previous = offset;
            }
            if count != 0 {
                strings.push(&text[previous..]);
            }
            return Ok(Some(Self {
                start,
                end,
                endian,
                strings,
            }));
        }
        Ok(None)
    }

    fn replace(&self, bytes: &[u8], strings: &[Option<Vec<u8>>]) -> Result<Vec<u8>> {
        let mut output = bytes[..self.start + 16].to_vec();
        let base = output.len();
        let table_end = base + 4 + strings.len() * 4;
        output.resize(table_end, 0);
        output[base..base + 4].copy_from_slice(&field_u32(strings.len(), self.endian)?);
        for (index, (replacement, original)) in strings.iter().zip(&self.strings).enumerate() {
            let offset = field_u32(output.len() - base, self.endian)?;
            let entry = base + 4 + index * 4;
            output[entry..entry + 4].copy_from_slice(&offset);
            output.extend_from_slice(replacement.as_deref().unwrap_or(original));
        }
        let size = field_u32(output.len() - base, self.endian)?;
        output[self.start + 4..self.start + 8].copy_from_slice(&size);
        output.resize(padded(output.len())?, 0xab);
        output.extend_from_slice(&bytes[self.end..]);
        let total = field_u32(output.len(), self.endian)?;
        output[18..22].copy_from_slice(&total);
        Ok(output)
    }
}

/// Rewrite eligible dialogue members using a per-UTF-16-unit advance in 1/64 pixels.
/// Returns a replacement archive and changed message count, or `None` if unchanged.
pub fn reflow_archive(
    name: &str,
    bytes: &[u8],
    mut advance: impl FnMut(u16) -> Result<i64>,
) -> Result<Option<(Vec<u8>, usize)>> {
    if rule(name, "LifeSupport/").is_none() {
        return Ok(None);
    }
    let archive = sarc::parse(bytes)?;
    let members = archive.members()?;
    let mut replaced = Vec::with_capacity(members.len());
    let mut changes = 0;
    for member in members {
        let mut replacement = None;
        if let Some(rule) = rule(name, &member.name) {
            if member.data.starts_with(b"MsgStdBn") {
                if let Some(section) = TextSection::parse(member.data)? {
                    let mut strings = Vec::with_capacity(section.strings.len());
                    let mut count = 0;
                    for raw in &section.strings {
                        let changed = reflow(raw, section.endian, rule, &mut advance)?;
                        count += usize::from(changed.is_some());
                        strings.push(changed);
                    }
                    if count != 0 {
                        changes += count;
                        replacement = Some(section.replace(member.data, &strings)?);
                    }
                }
            }
        }
        replaced.push(replacement);
    }
    if changes == 0 {
        Ok(None)
    } else {
        Ok(Some((archive.replace_members(&replaced)?, changes)))
    }
}

/// The extraction of a mod leaves this directory in each mod it just extracted.
const PENDING: &str = ".msg-reflow-pending";
/// Rust publishes this directory only after every replacement has been encoded.
const READY: &str = ".msg-reflow-ready";

/// Run the game stage's Rust work for newly extracted mods, including pending work
/// from a previous failed or interrupted prepare. The game's own font supplies the original
/// glyph advances. The two on-disk phases matter because reflow is not idempotent.
pub(crate) fn finish_pending(game_romfs: &Path, out: &Path) -> Result<()> {
    let mut mods = directory(&out.join("mods"))?;
    mods.sort();
    let mut measure = None;
    for path in mods {
        let pending = path.join(PENDING);
        let ready = path.join(READY);
        let romfs = path.join("romfs");
        if pending.is_dir() {
            let messages = romfs.join("Message");
            if !stage_replacements(game_romfs, &messages, &pending, &mut measure)? {
                // The tools to measure the lines are missing: leave the work pending so the
                // next prepare retries it, and do not install anything for this mod now.
                continue;
            }
            fs::rename(&pending, &ready)?;
        }
        if ready.is_dir() {
            install_replacements(&romfs, &ready)?;
        }
    }
    Ok(())
}

fn stage_replacements(
    game_romfs: &Path,
    messages: &Path,
    pending: &Path,
    measure: &mut Option<Option<Advances>>,
) -> Result<bool> {
    // A staging failure leaves all originals unchanged. Drop old partial outputs
    // before recomputing so they cannot survive a later successful retry.
    for path in directory(pending)? {
        if fs::symlink_metadata(&path)?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    let mut archives = directory(messages)?;
    archives.retain(|archive| {
        archive.is_file()
            && archive
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.ends_with(".sarc.zs") && rule(name, "LifeSupport/").is_some()
                })
    });
    if archives.is_empty() {
        return Ok(true);
    }
    if measure.is_none() {
        *measure = Some(open_measure(game_romfs)?);
    }
    if matches!(measure, Some(None)) {
        // Reported by `open_measure`; the reflow is retried the next time.
        return Ok(false);
    }
    if let Some(Some(measure)) = measure {
        archives.sort();
        for archive in archives {
            // The retain predicate checked every archive's name.
            let name = archive
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let data = sarc::read(&archive)?;
            let updated = reflow_archive(name, &data, |unit| measure.advance(unit))?;
            if let Some((updated, _)) = updated {
                sarc::write_compressed(&pending.join(name), &updated)?;
            }
        }
    }
    Ok(true)
}

fn install_replacements(romfs: &Path, ready: &Path) -> Result<()> {
    let mut files = directory(ready)?;
    files.sort();
    let messages = romfs.join("Message");
    for file in files {
        let name = file
            .file_name()
            .ok_or_else(|| Error::Io("missing staged archive name".to_owned()))?;
        // Moving an already-completed file out of ready records its publication
        // atomically. A retry applies only files that are still in ready.
        fs::rename(&file, messages.join(name))?;
    }
    sarc::fix_archives(romfs)?;
    fs::remove_dir(ready)?;
    Ok(())
}

fn directory(path: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn open_measure(game_romfs: &Path) -> Result<Option<Advances>> {
    match Command::new("zstd").arg("-V").output() {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("zstd not found: the translation's long lines are not reflowed yet; install zstd and prepare again");
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }
    let bytes = sarc::read(&game_romfs.join("Font").join("ScalableFont.sarc.zs"))?;
    let archive = sarc::parse(&bytes)?;
    let members = archive.members()?;
    let encrypted = members
        .iter()
        .find(|member| member.name.starts_with("nintendoP_Seurat-B"))
        .and_then(|member| member.data.get(8..))
        .ok_or_else(|| Error::Io("dialogue font not found".to_owned()))?;
    let key = [0x49, 0x62, 0x18, 0x06];
    let font: Vec<_> = encrypted
        .iter()
        .zip(key.into_iter().cycle())
        .map(|(&byte, key)| byte ^ key)
        .collect();
    Ok(Some(Advances::new(font)?))
}

/// The advance of each unit in the dialogue font at 100 px, in 1/64 px: the glyph's design
/// advance scaled to the size, unhinted (what the previous Pillow probe returned, to within
/// its rounding). A unit the font has no glyph for measures as the font's `.notdef`.
struct Advances {
    font: Vec<u8>,
    cache: HashMap<u16, i64>,
}

/// The size the original dialogue is laid out at.
const FONT_SIZE: i64 = 100;

impl Advances {
    fn new(font: Vec<u8>) -> Result<Self> {
        let face = ttf_parser::Face::parse(&font, 0)
            .map_err(|_| Error::Io("the dialogue font cannot be opened".to_owned()))?;
        if face.units_per_em() == 0 {
            return Err(Error::Io("the dialogue font has no em size".to_owned()));
        }
        Ok(Self {
            font,
            cache: HashMap::new(),
        })
    }

    fn advance(&mut self, unit: u16) -> Result<i64> {
        if let Some(&advance) = self.cache.get(&unit) {
            return Ok(advance);
        }
        let face = ttf_parser::Face::parse(&self.font, 0)
            .map_err(|_| Error::Io("the dialogue font cannot be opened".to_owned()))?;
        let glyph = char::from_u32(u32::from(unit))
            .and_then(|c| face.glyph_index(c))
            .unwrap_or(ttf_parser::GlyphId(0));
        let design = i64::from(face.glyph_hor_advance(glyph).unwrap_or(0));
        let em = i64::from(face.units_per_em());
        // round half up, like Pillow's `round` of the 1/64 px value for these magnitudes
        let advance = (design * FONT_SIZE * 64 * 2 + em) / (em * 2);
        self.cache.insert(unit, advance);
        Ok(advance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-in synthetic font (no game or host font), as lines of hexadecimal.
    fn synthetic_font() -> Vec<u8> {
        include_str!("../tests/fixtures/synthetic-buttons.hex")
            .split_ascii_whitespace()
            .flat_map(|line| {
                (0..line.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
            })
            .collect()
    }

    #[test]
    fn advances_are_the_design_advance_at_100_px_in_64ths() {
        let font = synthetic_font();
        let face = ttf_parser::Face::parse(&font, 0).unwrap();
        let em = i64::from(face.units_per_em());
        let mut advances = Advances::new(font.clone()).unwrap();
        let mut checked = 0;
        for unit in 0x20u16..0x7f {
            let Some(glyph) = face.glyph_index(char::from_u32(u32::from(unit)).unwrap()) else {
                continue;
            };
            let design = i64::from(face.glyph_hor_advance(glyph).unwrap());
            let exact = design * 100 * 64;
            let got = advances.advance(unit).unwrap();
            assert!((got * em - exact).abs() <= em / 2 + 1, "unit {unit:#x}");
            checked += 1;
        }
        assert!(checked > 0, "the synthetic font maps no ASCII glyph");
        // a unit the font lacks measures as `.notdef`, not as an error
        assert!(advances.advance(0xd800).is_ok());
    }

    #[test]
    fn a_file_that_is_not_a_font_is_refused() {
        assert!(Advances::new(b"not a font".to_vec()).is_err());
    }
}

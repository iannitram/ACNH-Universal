use super::controller::{self, Bounds, Label, Labels, Replacement};
use super::truetype::{Font, Outline};
use super::{decode_bfttf, encode_bfttf, EXT_FONT};
use crate::{
    error::{Error, Result},
    sarc,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const ARCHIVE: &str = "Font/ScalableFont.sarc.zs";
const LABEL_FONT: &str = "P_Seurat-B.bfttf";
const VARIANTS: [(&str, &str, bool, bool); 5] = [
    ("xbox", "console", false, false),
    ("xbox", "xbox", false, true),
    ("playstation", "console", true, false),
    ("playstation", "xbox", true, true),
    ("switch", "xbox", false, true),
];

fn missing(name: &str) -> Error {
    Error::BadGameFolder(format!("the scalable font archive is missing {name}"))
}

fn archive_path(base: &Path) -> std::path::PathBuf {
    base.join("romfs").join(ARCHIVE)
}

pub(super) fn build(romfs: &Path, out: &Path) -> Result<()> {
    let pending: Vec<_> = VARIANTS
        .into_iter()
        .map(|(style, layout, playstation, xbox_layout)| {
            (
                style,
                playstation,
                xbox_layout,
                out.join(format!("glyphs_{style}")).join(layout),
            )
        })
        .filter(|(_, _, _, base)| {
            !archive_path(base).is_file() || !base.join("ParkExt.ttf").is_file()
        })
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    let data = sarc::read(&romfs.join(ARCHIVE))?;
    let archive = sarc::parse(&data)?;
    let members = archive.members()?;
    let mut extensions = members
        .iter()
        .enumerate()
        .filter(|(_, member)| member.name == EXT_FONT);
    let (index, extension) = extensions.next().ok_or_else(|| missing(EXT_FONT))?;
    if extensions.next().is_some() {
        return Err(Error::BadGameFolder(
            "the scalable font archive has duplicate extension fonts".into(),
        ));
    }
    let labels = members
        .iter()
        .find(|member| member.name.ends_with(LABEL_FONT))
        .ok_or_else(|| missing(LABEL_FONT))?;
    let (extension, key) = decode_bfttf(extension.data)?;
    let (label_data, _) = decode_bfttf(labels.data)?;
    let labels = read_labels(&label_data)?;
    let source = Font::parse(&extension)?;

    for (style, playstation, xbox_layout, base) in pending {
        let mut font = Font::parse(&extension)?;
        let plan = if style == "switch" {
            controller::switch_plan()
        } else {
            controller::plan(playstation, xbox_layout)
        };
        let mut replaced = BTreeSet::new();
        for (code, replacement) in plan {
            let Some(gid) = glyph(&source, code) else {
                continue;
            };
            if !replaced.insert(gid) {
                continue;
            }
            let Some(b) = source.bounds(gid)? else {
                continue;
            };
            let bounds = Bounds(
                b.x_min.into(),
                b.y_min.into(),
                b.x_max.into(),
                b.y_max.into(),
            );
            let mut outline = match replacement {
                Replacement::Swap(code) => {
                    let Some(source_gid) = glyph(&source, code) else {
                        continue;
                    };
                    source.outline(source_gid)?.contours
                }
                Replacement::Draw(shape) => shape.draw(bounds, &labels),
            };
            if bounds.fit(&mut outline) {
                font.replace(gid, Outline { contours: outline }, source.advance(gid)?)?;
            }
        }
        let font = font.finish()?;
        let path = archive_path(&base);
        let parent = path.parent().expect("archive path has a parent");
        fs::create_dir_all(parent)?;
        super::write_font(&base.join("ParkExt.ttf"), &font)?;
        let mut replacements = vec![None; members.len()];
        replacements[index] = Some(encode_bfttf(&font, key)?);
        sarc::write_compressed(&path, &archive.replace_members(&replacements)?)?;
    }
    Ok(())
}

fn glyph(font: &Font, code: u32) -> Option<u16> {
    font.glyph_id(char::from_u32(code)?)
}

fn read_labels(data: &[u8]) -> Result<Labels> {
    let font = Font::parse(data)?;
    let cap = font
        .glyph_id('H')
        .map(|gid| font.tight_bounds(gid))
        .transpose()?
        .flatten()
        .map(|b| f64::from(b.y_max))
        .filter(|h| *h > 0.0)
        .ok_or_else(|| missing("label font cap height"))?;
    let mut labels = BTreeMap::new();
    for letter in "ABXYLRT12".chars() {
        let gid = font
            .glyph_id(letter)
            .ok_or_else(|| Error::BadGameFolder(format!("label font is missing {letter}")))?;
        let mut outline = font.outline(gid)?.contours;
        if let Some(bounds) = font.bounds(gid)? {
            // FontTools draws the label at its horizontal bearing, which can
            // differ from the raw glyf origin.
            let shift = f64::from(font.side_bearing(gid)?) - f64::from(bounds.x_min);
            for point in outline.iter_mut().flatten() {
                point.x += shift;
            }
        }
        labels.insert(
            letter,
            Label {
                advance: font.advance(gid)?.into(),
                outline,
            },
        );
    }
    Ok(Labels(cap, labels))
}

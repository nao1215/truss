//! The AVIF container walk.
//!
//! AVIF stores the picture's size, its alpha, its orientation, and its clean aperture as
//! item properties of the primary item rather than in a header, so reading any of them means
//! walking `meta` down to `ipco` and following the `ipma` associations. The walk is here
//! rather than beside the other sniffers because it is that much longer than they are, and
//! because the pipeline reads the clean aperture through it too.
//!
//! Nothing here decodes pixels. The boxes are read from the bytes as they lie, so an AVIF
//! whose properties truss understands is described correctly even in a build without the
//! `avif` feature, which has no decoder at all.

use super::{ArtifactMetadata, TransformError, read_u16_be, read_u32_be, read_u64_be};

pub(super) fn sniff_avif(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    if bytes.len() < 16 {
        return Err(TransformError::DecodeFailed(
            "avif file is too short".to_string(),
        ));
    }

    if !has_avif_brand(&bytes[8..]) {
        return Err(TransformError::DecodeFailed(
            "avif file is missing a compatible AVIF brand".to_string(),
        ));
    }

    let inspection = inspect_avif_container(bytes)?;

    // A clean aperture is the picture; the stored size is what it is cut from.
    let dimensions = match (inspection.dimensions, inspection.clean_aperture()) {
        (Some((width, height)), Some(aperture)) => {
            let (_, _, aperture_width, aperture_height) = aperture.rectangle(width, height)?;
            Some((aperture_width, aperture_height))
        }
        (dimensions, _) => dimensions,
    };

    Ok(ArtifactMetadata {
        width: dimensions.map(|(width, _)| width),
        height: dimensions.map(|(_, height)| height),
        // An animated AVIF is a moving-image sequence, and the container says so in its
        // brands rather than in a property: `avis` is the sequence brand. The frames live in
        // a `moov` track, which nothing here reads, so this records that there is more than
        // one of them without claiming how many.
        frame_count: if declares_image_sequence(&bytes[8..]) {
            2
        } else {
            1
        },
        duration: None,
        has_alpha: inspection.has_alpha(),
        orientation: inspection.orientation(),
    })
}

/// Reports whether the brand list names the AVIF image-sequence brand.
fn declares_image_sequence(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }

    if &bytes[0..4] == b"avis" {
        return true;
    }

    // Past the major brand and the minor version, the rest of the box is the compatible
    // brand list, four bytes each.
    let mut offset = 8;
    while offset + 4 <= bytes.len() {
        if &bytes[offset..offset + 4] == b"avis" {
            return true;
        }
        offset += 4;
    }

    false
}

pub(super) fn has_avif_brand(bytes: &[u8]) -> bool {
    if bytes.len() < 8 {
        return false;
    }

    if is_avif_brand(&bytes[0..4]) {
        return true;
    }

    let mut offset = 8;
    while offset + 4 <= bytes.len() {
        if is_avif_brand(&bytes[offset..offset + 4]) {
            return true;
        }
        offset += 4;
    }

    false
}

fn is_avif_brand(bytes: &[u8]) -> bool {
    matches!(bytes, b"avif" | b"avis")
}

const AVIF_ALPHA_AUX_TYPE: &[u8] = b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha";

/// One box of an AVIF `ipco` container, in the position it holds there.
///
/// `ipma` names properties by their one-based position in `ipco`, so every box is recorded
/// whether or not anything here reads it, or the positions would slip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AvifProperty {
    /// An `irot` box: the quarter turns, anti-clockwise, to display the stored pixels by.
    Rotation(u8),
    /// An `imir` box: 0 exchanges the top and bottom halves, 1 the left and right halves.
    Mirror(u8),
    /// A `clap` box: the part of the stored picture that is the picture.
    CleanAperture(AvifCleanAperture),
    Other,
}

/// The `clap` box: the clean aperture's size and the offset of its centre from the centre
/// of the stored picture, each as a numerator over a denominator.
///
/// It is the crop MIAF defines alongside `irot` and `imir`, applied before either of them.
/// An encoder writes one to trim a frame it coded larger than the picture, and a viewer
/// that ignores it shows the padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AvifCleanAperture {
    width: (u32, u32),
    height: (u32, u32),
    horizontal_offset: (i32, u32),
    vertical_offset: (i32, u32),
}

impl AvifCleanAperture {
    fn parse(payload: &[u8]) -> Result<Self, TransformError> {
        if payload.len() < 32 {
            return Err(avif_box_too_short("clap"));
        }
        let field = |index: usize| read_u32_be(&payload[index * 4..index * 4 + 4]);
        Ok(Self {
            width: (field(0)?, field(1)?),
            height: (field(2)?, field(3)?),
            horizontal_offset: (field(4)? as i32, field(5)?),
            vertical_offset: (field(6)? as i32, field(7)?),
        })
    }

    /// The pixel rectangle the aperture selects out of a `width` by `height` picture, as
    /// `(x, y, width, height)`.
    ///
    /// ISO 14496-12 defines the aperture by its centre: the offset is the distance from the
    /// picture centre to the aperture centre, so the left edge sits at
    /// `(width - aperture_width) / 2 + horizontal_offset`, and likewise for the top. MIAF
    /// requires the result to land on whole pixels for an AV1 image, and one that does not,
    /// or that reaches outside the picture, is refused rather than rounded.
    ///
    /// # Errors
    ///
    /// Returns [`TransformError::DecodeFailed`] naming the field that is not a whole number
    /// of pixels or that leaves the picture.
    pub(crate) fn rectangle(
        self,
        width: u32,
        height: u32,
    ) -> Result<(u32, u32, u32, u32), TransformError> {
        let aperture_width = avif_aperture_size(self.width, "width")?;
        let aperture_height = avif_aperture_size(self.height, "height")?;
        let x = avif_aperture_origin(width, aperture_width, self.horizontal_offset, "horizontal")?;
        let y = avif_aperture_origin(height, aperture_height, self.vertical_offset, "vertical")?;
        Ok((x, y, aperture_width, aperture_height))
    }
}

fn avif_aperture_size(
    (numerator, denominator): (u32, u32),
    axis: &str,
) -> Result<u32, TransformError> {
    if denominator == 0 || numerator == 0 || numerator % denominator != 0 {
        return Err(TransformError::DecodeFailed(format!(
            "avif clean aperture {axis} {numerator}/{denominator} is not a whole number of pixels"
        )));
    }
    Ok(numerator / denominator)
}

/// The aperture's first pixel along one axis: `(picture - aperture) / 2 + offset`, worked in
/// units of `2 * denominator` so that the halving and the fraction stay exact.
fn avif_aperture_origin(
    picture: u32,
    aperture: u32,
    (numerator, denominator): (i32, u32),
    axis: &str,
) -> Result<u32, TransformError> {
    if denominator == 0 {
        return Err(TransformError::DecodeFailed(format!(
            "avif clean aperture {axis} offset has a zero denominator"
        )));
    }
    if aperture > picture {
        return Err(TransformError::DecodeFailed(format!(
            "avif clean aperture is larger than the {picture}-pixel picture along the {axis} axis"
        )));
    }
    let scaled = i64::from(picture - aperture) * i64::from(denominator) + 2 * i64::from(numerator);
    let divisor = 2 * i64::from(denominator);
    if scaled % divisor != 0 {
        return Err(TransformError::DecodeFailed(format!(
            "avif clean aperture {axis} offset {numerator}/{denominator} does not land on a whole pixel"
        )));
    }
    let origin = scaled / divisor;
    if origin < 0 || origin + i64::from(aperture) > i64::from(picture) {
        return Err(TransformError::DecodeFailed(format!(
            "avif clean aperture leaves the picture along the {axis} axis"
        )));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(origin as u32)
}

#[derive(Debug, Default)]
struct AvifInspection {
    dimensions: Option<(u32, u32)>,
    saw_structured_meta: bool,
    found_alpha_item: bool,
    primary_item: Option<u32>,
    properties: Vec<AvifProperty>,
    /// Each item's one-based `ipco` positions, as `ipma` lists them.
    associations: Vec<(u32, Vec<u16>)>,
}

impl AvifInspection {
    fn has_alpha(&self) -> Option<bool> {
        if self.saw_structured_meta {
            Some(self.found_alpha_item)
        } else {
            None
        }
    }

    /// The transform the primary item's `irot` and `imir` properties add up to, as the EXIF
    /// orientation value that names it, or `None` when the item has neither.
    fn orientation(&self) -> Option<u16> {
        let mut rotation = None;
        let mut mirror = None;
        for property in self.primary_properties() {
            match property {
                AvifProperty::Rotation(angle) => rotation = Some(*angle),
                AvifProperty::Mirror(mode) => mirror = Some(*mode),
                _ => {}
            }
        }

        if rotation.is_none() && mirror.is_none() {
            return None;
        }
        Some(avif_orientation_value(rotation.unwrap_or(0), mirror))
    }

    /// The primary item's clean aperture, when it has one.
    fn clean_aperture(&self) -> Option<AvifCleanAperture> {
        self.primary_properties()
            .find_map(|property| match property {
                AvifProperty::CleanAperture(aperture) => Some(*aperture),
                _ => None,
            })
    }

    /// The properties `ipma` associates with the primary item, in the order it lists them.
    fn primary_properties(&self) -> impl Iterator<Item = &AvifProperty> {
        let positions = self.primary_item.and_then(|primary_item| {
            self.associations
                .iter()
                .find(|(item, _)| *item == primary_item)
                .map(|(_, positions)| positions.as_slice())
        });
        positions.unwrap_or(&[]).iter().filter_map(|position| {
            usize::from(*position)
                .checked_sub(1)
                .and_then(|index| self.properties.get(index))
        })
    }
}

/// Reads the clean aperture an AVIF's primary item carries, when it carries one.
///
/// The decoder applies it: `mp4parse` does not read the box, and since MIAF marks it
/// essential, it forbids the item that carries one, which is why the aperture is read here
/// from the same walk the sniffer does rather than from the parsed context.
///
/// # Errors
///
/// Returns [`TransformError::DecodeFailed`] when the container cannot be walked.
#[cfg(feature = "avif")]
/// Reports whether the file carries metadata items a strip policy would remove.
///
/// The passthrough that keeps `optimize` from returning more bytes than it was given needs
/// to know this before it can hand an AVIF back: handing back a file that still carries its
/// EXIF would satisfy the size guarantee by breaking the metadata one. Metadata lives in
/// `meta` as items whose `infe` entry names an item type, `Exif` for an EXIF block and
/// `mime` for XMP, so the walk stops at the item list rather than reading the items.
pub(crate) fn avif_carries_metadata(bytes: &[u8]) -> bool {
    fn walk(bytes: &[u8]) -> bool {
        let mut offset = 0;
        while offset + 8 <= bytes.len() {
            let Ok((box_type, payload, next_offset)) = parse_mp4_box(bytes, offset) else {
                return false;
            };
            match box_type {
                // A full box: four bytes of version and flags before the children.
                b"meta" => {
                    if payload.len() >= 4 && walk(&payload[4..]) {
                        return true;
                    }
                }
                b"iinf" => {
                    if payload.windows(4).any(|window| window == b"Exif")
                        || payload.windows(4).any(|window| window == b"mime")
                    {
                        return true;
                    }
                }
                _ => {}
            }
            if next_offset <= offset {
                return false;
            }
            offset = next_offset;
        }
        false
    }

    walk(bytes)
}

pub(crate) fn avif_clean_aperture(
    bytes: &[u8],
) -> Result<Option<AvifCleanAperture>, TransformError> {
    Ok(inspect_avif_container(bytes)?.clean_aperture())
}

/// Reads the orientation an AVIF signals through its `irot` and `imir` item properties.
///
/// AVIF has no orientation field of its own kind. An encoder handed a photo with an EXIF
/// orientation writes the transform as these two properties of the primary item, and a
/// browser applies them, so they are what decides whether the picture displays upright.
/// The container may also hold an Exif item whose Orientation field says something, and
/// that field is ignored here as Chrome and Firefox ignore it: the properties are the
/// signal the encoder chose, and honouring both would turn the picture twice.
pub(super) fn avif_orientation(bytes: &[u8]) -> Option<u16> {
    inspect_avif_container(bytes).ok()?.orientation()
}

/// Folds an AVIF rotation and mirror into the EXIF orientation value naming the same transform.
///
/// `angle` is the `irot` value, quarter turns anti-clockwise, and `mirror` the `imir` mode,
/// 0 for top-bottom and 1 for left-right. MIAF applies the rotation before the mirror
/// whatever order the file lists them in, and each row below is one mirror worked through
/// the four angles under that rule.
fn avif_orientation_value(angle: u8, mirror: Option<u8>) -> u16 {
    const NO_MIRROR: [u16; 4] = [1, 8, 3, 6];
    const TOP_BOTTOM: [u16; 4] = [4, 5, 2, 7];
    const LEFT_RIGHT: [u16; 4] = [2, 7, 4, 5];

    let row = match mirror {
        None => NO_MIRROR,
        Some(0) => TOP_BOTTOM,
        Some(_) => LEFT_RIGHT,
    };
    row[usize::from(angle & 0b11)]
}

fn inspect_avif_container(bytes: &[u8]) -> Result<AvifInspection, TransformError> {
    let mut inspection = AvifInspection::default();
    inspect_avif_boxes(bytes, &mut inspection)?;
    Ok(inspection)
}

fn inspect_avif_boxes(bytes: &[u8], inspection: &mut AvifInspection) -> Result<(), TransformError> {
    let mut offset = 0;

    while offset + 8 <= bytes.len() {
        let (box_type, payload, next_offset) = parse_mp4_box(bytes, offset)?;

        match box_type {
            b"meta" | b"iref" => {
                inspection.saw_structured_meta = true;
                if payload.len() < 4 {
                    return Err(TransformError::DecodeFailed(format!(
                        "{} box is too short",
                        String::from_utf8_lossy(box_type)
                    )));
                }
                inspect_avif_boxes(&payload[4..], inspection)?;
            }
            b"iprp" => {
                inspection.saw_structured_meta = true;
                inspect_avif_boxes(payload, inspection)?;
            }
            b"ipco" => {
                inspection.saw_structured_meta = true;
                inspect_avif_properties(payload, inspection)?;
            }
            b"pitm" => {
                inspection.saw_structured_meta = true;
                inspection.primary_item = Some(parse_avif_pitm(payload)?);
            }
            b"ipma" => {
                inspection.saw_structured_meta = true;
                inspection.associations.extend(parse_avif_ipma(payload)?);
            }
            b"auxl" => {
                inspection.saw_structured_meta = true;
                inspection.found_alpha_item = true;
            }
            _ => {}
        }

        offset = next_offset;
    }

    if offset != bytes.len() {
        return Err(TransformError::DecodeFailed(
            "avif box payload has trailing bytes".to_string(),
        ));
    }

    Ok(())
}

/// Walks an `ipco` box, recording every property in order so `ipma` positions resolve.
fn inspect_avif_properties(
    bytes: &[u8],
    inspection: &mut AvifInspection,
) -> Result<(), TransformError> {
    let mut offset = 0;

    while offset + 8 <= bytes.len() {
        let (box_type, payload, next_offset) = parse_mp4_box(bytes, offset)?;

        let property = match box_type {
            b"ispe" => {
                if inspection.dimensions.is_none() {
                    inspection.dimensions = Some(parse_avif_ispe(payload)?);
                }
                AvifProperty::Other
            }
            b"auxC" => {
                if avif_auxc_declares_alpha(payload)? {
                    inspection.found_alpha_item = true;
                }
                AvifProperty::Other
            }
            b"irot" => AvifProperty::Rotation(avif_transform_byte(payload, "irot")? & 0b11),
            b"imir" => AvifProperty::Mirror(avif_transform_byte(payload, "imir")? & 0b1),
            b"clap" => AvifProperty::CleanAperture(AvifCleanAperture::parse(payload)?),
            _ => AvifProperty::Other,
        };
        inspection.properties.push(property);

        offset = next_offset;
    }

    if offset != bytes.len() {
        return Err(TransformError::DecodeFailed(
            "avif box payload has trailing bytes".to_string(),
        ));
    }

    Ok(())
}

fn avif_box_too_short(box_name: &str) -> TransformError {
    TransformError::DecodeFailed(format!("avif {box_name} box is too short"))
}

/// The single payload byte of an `irot` or `imir` box.
fn avif_transform_byte(bytes: &[u8], box_name: &str) -> Result<u8, TransformError> {
    bytes
        .first()
        .copied()
        .ok_or_else(|| avif_box_too_short(box_name))
}

/// Reads the primary item id out of a `pitm` box.
fn parse_avif_pitm(bytes: &[u8]) -> Result<u32, TransformError> {
    let too_short = || avif_box_too_short("pitm");
    // A version 0 box holds a 16-bit id after the version and flags, version 1 a 32-bit one.
    match bytes.first().ok_or_else(too_short)? {
        0 => Ok(u32::from(read_u16_be(
            bytes.get(4..6).ok_or_else(too_short)?,
        )?)),
        _ => read_u32_be(bytes.get(4..8).ok_or_else(too_short)?),
    }
}

/// Reads an `ipma` box as (item id, one-based `ipco` positions) pairs.
///
/// The essential bit on each position is dropped: it tells a reader that cannot apply a
/// property to refuse the file, and both properties read here are applied.
fn parse_avif_ipma(bytes: &[u8]) -> Result<Vec<(u32, Vec<u16>)>, TransformError> {
    let too_short = || avif_box_too_short("ipma");
    let version = *bytes.first().ok_or_else(too_short)?;
    // The low flag bit widens each position from 7 bits to 15.
    let wide_positions = bytes.get(3).ok_or_else(too_short)? & 1 == 1;
    let entry_count = read_u32_be(bytes.get(4..8).ok_or_else(too_short)?)?;

    let mut offset = 8;
    let mut entries = Vec::new();
    for _ in 0..entry_count {
        let item = if version == 0 {
            let item = read_u16_be(bytes.get(offset..offset + 2).ok_or_else(too_short)?)?;
            offset += 2;
            u32::from(item)
        } else {
            let item = read_u32_be(bytes.get(offset..offset + 4).ok_or_else(too_short)?)?;
            offset += 4;
            item
        };
        let count = *bytes.get(offset).ok_or_else(too_short)?;
        offset += 1;

        let mut positions = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let position = if wide_positions {
                let position = read_u16_be(bytes.get(offset..offset + 2).ok_or_else(too_short)?)?;
                offset += 2;
                position & 0x7FFF
            } else {
                let position = *bytes.get(offset).ok_or_else(too_short)?;
                offset += 1;
                u16::from(position & 0x7F)
            };
            positions.push(position);
        }
        entries.push((item, positions));
    }

    Ok(entries)
}

fn parse_mp4_box(bytes: &[u8], offset: usize) -> Result<(&[u8; 4], &[u8], usize), TransformError> {
    if offset + 8 > bytes.len() {
        return Err(TransformError::DecodeFailed(
            "mp4 box header is truncated".to_string(),
        ));
    }

    let size = read_u32_be(&bytes[offset..offset + 4])?;
    let box_type = bytes[offset + 4..offset + 8]
        .try_into()
        .map_err(|_| TransformError::DecodeFailed("expected 4-byte box type".to_string()))?;
    let mut header_len = 8_usize;
    let end = match size {
        0 => bytes.len(),
        1 => {
            if offset + 16 > bytes.len() {
                return Err(TransformError::DecodeFailed(
                    "extended mp4 box header is truncated".to_string(),
                ));
            }
            header_len = 16;
            let extended_size = read_u64_be(&bytes[offset + 8..offset + 16])?;
            usize::try_from(extended_size)
                .map_err(|_| TransformError::DecodeFailed("mp4 box is too large".to_string()))?
        }
        _ => size as usize,
    };

    if end < header_len {
        return Err(TransformError::DecodeFailed(
            "mp4 box size is smaller than its header".to_string(),
        ));
    }

    let box_end = offset
        .checked_add(end)
        .ok_or_else(|| TransformError::DecodeFailed("mp4 box is too large".to_string()))?;
    if box_end > bytes.len() {
        return Err(TransformError::DecodeFailed(
            "mp4 box exceeds file length".to_string(),
        ));
    }

    Ok((box_type, &bytes[offset + header_len..box_end], box_end))
}

fn parse_avif_ispe(bytes: &[u8]) -> Result<(u32, u32), TransformError> {
    if bytes.len() < 12 {
        return Err(TransformError::DecodeFailed(
            "avif ispe box is too short".to_string(),
        ));
    }

    let width = read_u32_be(&bytes[4..8])?;
    let height = read_u32_be(&bytes[8..12])?;
    Ok((width, height))
}

fn avif_auxc_declares_alpha(bytes: &[u8]) -> Result<bool, TransformError> {
    if bytes.len() < 5 {
        return Err(TransformError::DecodeFailed(
            "avif auxC box is too short".to_string(),
        ));
    }

    let urn = &bytes[4..];
    Ok(urn
        .strip_suffix(&[0])
        .is_some_and(|urn| urn == AVIF_ALPHA_AUX_TYPE))
}

//! Reading and writing the metadata items an AVIF container carries.
//!
//! AVIF keeps EXIF as an item of its own, named `Exif` in the item information box, and an ICC
//! profile as a `colr` property of the primary item. Neither is anywhere near the pixels, so
//! both can be read out of a file and written back into one without touching a decoder, which
//! is what lets `--keep-metadata` work for an output format whose encoder writes pixels only.
//!
//! The box walk itself is the parent module's; what is here is the item structures the walk
//! stops short of, and the surgery that puts them back.

use super::{avif_box_too_short, parse_avif_pitm, parse_mp4_box};
use crate::core::{TransformError, read_u16_be, read_u32_be};

/// The metadata an AVIF container carries beside its pixels.
///
/// AVIF keeps EXIF as an item of its own, named `Exif` in the item information box, and an
/// ICC profile as a `colr` property of the primary item. Neither is anywhere near the pixels,
/// so both can be read out of the bytes and written back into them without touching a decoder.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AvifMetadata {
    /// The TIFF block an `Exif` item holds, past the four-byte header offset that precedes it.
    pub(crate) exif: Option<Vec<u8>>,
    /// The ICC profile a `colr` box of type `prof` or `rICC` holds.
    pub(crate) icc: Option<Vec<u8>>,
}

/// Reads the EXIF block and the ICC profile an AVIF carries.
///
/// A container this cannot walk reports no metadata rather than failing: the caller is
/// deciding what to carry over from an input the decoder has already accepted, and refusing
/// the transform because an item list is malformed would turn a picture truss can convert
/// into an error. What is read is what the walk is sure of.
pub(crate) fn avif_metadata(bytes: &[u8]) -> AvifMetadata {
    let Some(meta) = locate_meta_box(bytes) else {
        return AvifMetadata::default();
    };

    AvifMetadata {
        exif: read_exif_item(bytes, meta),
        icc: read_icc_profile(meta.payload),
    }
}

/// The `meta` box's payload, past its version and flags, with the file offset it begins at.
#[derive(Debug, Clone, Copy)]
struct MetaBox<'a> {
    payload: &'a [u8],
}

fn locate_meta_box(bytes: &[u8]) -> Option<MetaBox<'_>> {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let (box_type, payload, next_offset) = parse_mp4_box(bytes, offset).ok()?;
        if box_type == b"meta" {
            return payload.get(4..).map(|payload| MetaBox { payload });
        }
        if next_offset <= offset {
            return None;
        }
        offset = next_offset;
    }
    None
}

/// Finds one child of a box payload by type.
fn find_child<'a>(payload: &'a [u8], wanted: &[u8; 4]) -> Option<&'a [u8]> {
    let mut offset = 0;
    while offset + 8 <= payload.len() {
        let (box_type, child, next_offset) = parse_mp4_box(payload, offset).ok()?;
        if box_type == wanted {
            return Some(child);
        }
        if next_offset <= offset {
            return None;
        }
        offset = next_offset;
    }
    None
}

/// The EXIF item's payload, located through `iinf` and `iloc`.
///
/// The item's data begins with a four-byte `exif_tiff_header_offset`, which says how far past
/// itself the TIFF header sits; every writer in practice puts it immediately after and writes
/// zero, and a non-zero value is honoured here rather than assumed away.
fn read_exif_item(bytes: &[u8], meta: MetaBox<'_>) -> Option<Vec<u8>> {
    let items = parse_iinf(find_child(meta.payload, b"iinf")?).ok()?;
    let exif_item = items
        .iter()
        .find(|item| &item.item_type == b"Exif")?
        .item_id;
    let locations = parse_iloc(find_child(meta.payload, b"iloc")?).ok()?;
    let extents = &locations
        .iter()
        .find(|item| item.item_id == exif_item)?
        .extents;

    let mut payload = Vec::new();
    for extent in extents {
        let start = usize::try_from(extent.offset).ok()?;
        let end = start.checked_add(usize::try_from(extent.length).ok()?)?;
        payload.extend_from_slice(bytes.get(start..end)?);
    }

    let header_offset =
        usize::try_from(u32::from_be_bytes(payload.get(0..4)?.try_into().ok()?)).ok()?;
    let tiff = payload.get(4usize.checked_add(header_offset)?..)?;
    (!tiff.is_empty()).then(|| tiff.to_vec())
}

/// The ICC profile the primary item's `colr` property holds.
///
/// `prof` is a full profile and `rICC` a restricted one; both are the profile itself after the
/// four-byte colour type, and both are what an encoder writes when it is handed one. A `colr`
/// of type `nclx` describes the colour space with three enumerations and carries no profile.
fn read_icc_profile(meta_payload: &[u8]) -> Option<Vec<u8>> {
    let ipco = find_child(find_child(meta_payload, b"iprp")?, b"ipco")?;
    let mut offset = 0;
    while offset + 8 <= ipco.len() {
        let (box_type, payload, next_offset) = parse_mp4_box(ipco, offset).ok()?;
        if box_type == b"colr"
            && let Some(colour_type) = payload.get(0..4)
            && (colour_type == b"prof" || colour_type == b"rICC")
        {
            let profile = payload.get(4..)?;
            return (!profile.is_empty()).then(|| profile.to_vec());
        }
        if next_offset <= offset {
            return None;
        }
        offset = next_offset;
    }
    None
}

/// One entry of the item information box: an item's id and the four-character type naming it.
#[derive(Debug, Clone, Copy)]
struct AvifItem {
    item_id: u32,
    item_type: [u8; 4],
}

/// Reads `iinf` as the item ids and types it declares.
///
/// Only `infe` version 2 and 3 carry an item type; the version 0 and 1 forms name a MIME type
/// in a string instead and predate the item types AVIF uses, so an entry in one of those forms
/// is skipped rather than guessed at.
fn parse_iinf(payload: &[u8]) -> Result<Vec<AvifItem>, TransformError> {
    let too_short = || avif_box_too_short("iinf");
    let version = *payload.first().ok_or_else(too_short)?;
    let (entry_count, mut offset) = if version == 0 {
        (
            u32::from(read_u16_be(payload.get(4..6).ok_or_else(too_short)?)?),
            6,
        )
    } else {
        (read_u32_be(payload.get(4..8).ok_or_else(too_short)?)?, 8)
    };

    let mut items = Vec::new();
    for _ in 0..entry_count {
        let (box_type, entry, next_offset) = parse_mp4_box(payload, offset)?;
        if box_type == b"infe" {
            let entry_version = *entry.first().ok_or_else(too_short)?;
            let (item_id, type_at) = match entry_version {
                2 => (
                    u32::from(read_u16_be(entry.get(4..6).ok_or_else(too_short)?)?),
                    8,
                ),
                3 => (read_u32_be(entry.get(4..8).ok_or_else(too_short)?)?, 10),
                _ => {
                    offset = next_offset;
                    continue;
                }
            };
            let item_type: [u8; 4] = entry
                .get(type_at..type_at + 4)
                .ok_or_else(too_short)?
                .try_into()
                .map_err(|_| too_short())?;
            items.push(AvifItem { item_id, item_type });
        }
        if next_offset <= offset {
            break;
        }
        offset = next_offset;
    }

    Ok(items)
}

/// One item's data, as absolute file offsets.
#[derive(Debug, Clone)]
struct AvifItemLocation {
    item_id: u32,
    extents: Vec<AvifExtent>,
}

#[derive(Debug, Clone, Copy)]
struct AvifExtent {
    offset: u64,
    length: u64,
}

/// Reads `iloc` as absolute file offsets, refusing the forms that are not offsets into the file.
///
/// The base offset is folded into each extent, so what comes back is what a reader can slice
/// the file with. A construction method other than zero means the data is in the `idat` box or
/// in another file rather than at a file offset, and neither is something this reads.
fn parse_iloc(payload: &[u8]) -> Result<Vec<AvifItemLocation>, TransformError> {
    let too_short = || avif_box_too_short("iloc");
    let version = *payload.first().ok_or_else(too_short)?;
    let sizes = *payload.get(4).ok_or_else(too_short)?;
    let offset_size = usize::from(sizes >> 4);
    let length_size = usize::from(sizes & 0x0F);
    let base_and_index = *payload.get(5).ok_or_else(too_short)?;
    let base_offset_size = usize::from(base_and_index >> 4);
    let index_size = if version == 1 || version == 2 {
        usize::from(base_and_index & 0x0F)
    } else {
        0
    };

    let (item_count, mut offset) = if version < 2 {
        (
            u32::from(read_u16_be(payload.get(6..8).ok_or_else(too_short)?)?),
            8,
        )
    } else {
        (read_u32_be(payload.get(6..10).ok_or_else(too_short)?)?, 10)
    };

    let read_field = |payload: &[u8], at: usize, size: usize| -> Result<u64, TransformError> {
        let bytes = payload.get(at..at + size).ok_or_else(too_short)?;
        Ok(bytes
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
    };

    let mut locations = Vec::new();
    for _ in 0..item_count {
        let item_id = if version < 2 {
            let id = read_u16_be(payload.get(offset..offset + 2).ok_or_else(too_short)?)?;
            offset += 2;
            u32::from(id)
        } else {
            let id = read_u32_be(payload.get(offset..offset + 4).ok_or_else(too_short)?)?;
            offset += 4;
            id
        };
        if version == 1 || version == 2 {
            let construction =
                read_u16_be(payload.get(offset..offset + 2).ok_or_else(too_short)?)? & 0x0F;
            offset += 2;
            if construction != 0 {
                return Err(TransformError::DecodeFailed(
                    "avif iloc uses a construction method that is not a file offset".to_string(),
                ));
            }
        }
        offset += 2; // data_reference_index
        let base_offset = read_field(payload, offset, base_offset_size)?;
        offset += base_offset_size;
        let extent_count = read_u16_be(payload.get(offset..offset + 2).ok_or_else(too_short)?)?;
        offset += 2;

        let mut extents = Vec::with_capacity(usize::from(extent_count));
        for _ in 0..extent_count {
            offset += index_size;
            let extent_offset = read_field(payload, offset, offset_size)?;
            offset += offset_size;
            let length = read_field(payload, offset, length_size)?;
            offset += length_size;
            extents.push(AvifExtent {
                offset: base_offset
                    .checked_add(extent_offset)
                    .ok_or_else(|| avif_box_too_short("iloc"))?,
                length,
            });
        }
        locations.push(AvifItemLocation { item_id, extents });
    }

    Ok(locations)
}

fn avif_write_failed(what: &str) -> TransformError {
    TransformError::EncodeFailed(format!(
        "cannot write metadata into the avif container: {what}"
    ))
}

/// Writes an EXIF block and an ICC profile into an AVIF that has neither.
///
/// The encoder truss reaches through the `image` crate writes pixels and colour information and
/// nothing else, so retaining metadata across a transform to AVIF is a question about the
/// container rather than about the codec: the EXIF becomes an item of its own, referenced back
/// to the primary item as its description, and the ICC profile becomes a `colr` property
/// associated with that item. It is the same post-encode surgery
/// `webp_bytes_satisfying_metadata_policy` performs on the other container truss can rewrite.
///
/// The EXIF payload goes into a second `mdat` at the end of the file rather than into the one
/// the encoder wrote. Item data is addressed by file offset, so a box appended at the end moves
/// nothing, while growing the existing `mdat` would move every byte after it. The only offsets
/// that change are the ones the enlarged `meta` box pushes along, and those are rewritten.
///
/// # Errors
///
/// Returns [`TransformError::EncodeFailed`] when the container is not one this can rewrite: no
/// `meta` box, no primary item, an item whose data is not at a file offset, or an offset that
/// does not fit the four bytes the rewritten `iloc` gives it. The encoder's own output is none
/// of those, so an error here is a defect rather than something an input can cause.
pub(crate) fn avif_with_metadata(
    bytes: &[u8],
    exif: Option<&[u8]>,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, TransformError> {
    if exif.is_none() && icc.is_none() {
        return Ok(bytes.to_vec());
    }

    let (meta_start, meta_end) = locate_top_level_meta(bytes)?;
    let meta =
        locate_meta_box(bytes).ok_or_else(|| avif_write_failed("the meta box has no payload"))?;
    let primary_item = find_child(meta.payload, b"pitm")
        .and_then(|pitm| parse_avif_pitm(pitm).ok())
        .ok_or_else(|| avif_write_failed("there is no primary item"))?;
    let items = parse_iinf(
        find_child(meta.payload, b"iinf")
            .ok_or_else(|| avif_write_failed("there is no item information box"))?,
    )
    .map_err(|_| avif_write_failed("the item information cannot be read"))?;
    let locations = parse_iloc(
        find_child(meta.payload, b"iloc")
            .ok_or_else(|| avif_write_failed("there is no item location box"))?,
    )
    .map_err(|_| avif_write_failed("the item locations cannot be read"))?;

    let exif_item_id = items
        .iter()
        .map(|item| item.item_id)
        .chain(locations.iter().map(|location| location.item_id))
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| avif_write_failed("there is no item id left"))?;

    // The EXIF item's data is the four-byte `exif_tiff_header_offset` the format puts in front
    // of the TIFF block, then the block. Every writer puts the block immediately after, so the
    // offset is zero, and a reader that honours it reads the same bytes back.
    let exif_payload = exif.map(|exif| {
        let mut payload = Vec::with_capacity(exif.len() + 4);
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(exif);
        payload
    });

    // The meta box is built twice: once to learn how much longer it is than the one it
    // replaces, and once with every item offset moved along by that difference and the EXIF
    // item pointed at where the file now ends. The two are the same length, because the
    // rewritten `iloc` gives every offset four bytes whatever it holds.
    let placeholder = exif_payload.as_ref().map(|payload| AvifExtent {
        offset: 0,
        length: payload.len() as u64,
    });
    let measured = build_meta_box(
        &meta,
        primary_item,
        exif_item_id,
        &locations,
        placeholder,
        icc,
        0,
    )?;

    let shift = i64::try_from(measured.len())
        .ok()
        .zip(i64::try_from(meta_end - meta_start).ok())
        .map(|(new, old)| new - old)
        .ok_or_else(|| avif_write_failed("the meta box is too large"))?;
    let rewritten_len = meta_start + measured.len() + (bytes.len() - meta_end);

    let exif_extent = exif_payload.as_ref().map(|payload| AvifExtent {
        // Past everything the file will hold, plus the header of the `mdat` it goes into.
        offset: (rewritten_len + 8) as u64,
        length: payload.len() as u64,
    });
    let rebuilt = build_meta_box(
        &meta,
        primary_item,
        exif_item_id,
        &locations,
        exif_extent,
        icc,
        shift,
    )?;
    if rebuilt.len() != measured.len() {
        return Err(avif_write_failed(
            "the rewritten meta box changed size on the second pass",
        ));
    }

    let mut output =
        Vec::with_capacity(rewritten_len + exif_payload.as_ref().map_or(0, Vec::len) + 8);
    output.extend_from_slice(&bytes[..meta_start]);
    output.extend_from_slice(&rebuilt);
    output.extend_from_slice(&bytes[meta_end..]);
    debug_assert_eq!(output.len(), rewritten_len);

    if let Some(payload) = exif_payload {
        output.extend_from_slice(&box_with_type(b"mdat", &payload)?);
    }

    Ok(output)
}

/// The start and end offsets of the top-level `meta` box.
fn locate_top_level_meta(bytes: &[u8]) -> Result<(usize, usize), TransformError> {
    let mut found = None;
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let (box_type, _, next_offset) = parse_mp4_box(bytes, offset)
            .map_err(|_| avif_write_failed("the top-level boxes cannot be walked"))?;
        if box_type == b"meta" {
            found = Some((offset, next_offset));
        }
        if next_offset <= offset {
            return Err(avif_write_failed("a top-level box has no size"));
        }
        offset = next_offset;
    }
    found.ok_or_else(|| avif_write_failed("there is no meta box"))
}

/// Builds the replacement `meta` box, with `shift` added to every existing item offset.
fn build_meta_box(
    meta: &MetaBox<'_>,
    primary_item: u32,
    exif_item_id: u32,
    locations: &[AvifItemLocation],
    exif_extent: Option<AvifExtent>,
    icc: Option<&[u8]>,
    shift: i64,
) -> Result<Vec<u8>, TransformError> {
    let mut children = Vec::new();
    let mut rewrote_iref = false;
    let mut offset = 0;
    while offset + 8 <= meta.payload.len() {
        let (box_type, payload, next_offset) = parse_mp4_box(meta.payload, offset)
            .map_err(|_| avif_write_failed("the meta box children cannot be walked"))?;
        match box_type {
            b"iinf" => children.push(build_iinf(payload, exif_item_id, exif_extent.is_some())?),
            b"iloc" => children.push(build_iloc(locations, exif_item_id, exif_extent, shift)?),
            b"iprp" => children.push(build_iprp(payload, primary_item, icc)?),
            b"iref" => {
                rewrote_iref = true;
                children.push(build_iref(
                    Some(payload),
                    primary_item,
                    exif_item_id,
                    exif_extent.is_some(),
                )?);
            }
            _ => children.push(meta.payload[offset..next_offset].to_vec()),
        }
        if next_offset <= offset {
            return Err(avif_write_failed("a meta box child has no size"));
        }
        offset = next_offset;
    }

    if exif_extent.is_some() && !rewrote_iref {
        children.push(build_iref(None, primary_item, exif_item_id, true)?);
    }

    // The `meta` box's own version and flags, which `locate_meta_box` steps past.
    let mut payload = vec![0, 0, 0, 0];
    for child in children {
        payload.extend_from_slice(&child);
    }
    box_with_type(b"meta", &payload)
}

/// The item information box with an `Exif` entry added.
fn build_iinf(
    payload: &[u8],
    exif_item_id: u32,
    with_exif: bool,
) -> Result<Vec<u8>, TransformError> {
    let failed = || avif_write_failed("the item information cannot be rewritten");
    let version = *payload.first().ok_or_else(failed)?;
    let (count, entries_at) = if version == 0 {
        (
            u32::from(read_u16_be(payload.get(4..6).ok_or_else(failed)?)?),
            6,
        )
    } else {
        (read_u32_be(payload.get(4..8).ok_or_else(failed)?)?, 8)
    };
    let entries = payload.get(entries_at..).ok_or_else(failed)?;
    let new_count = if with_exif { count + 1 } else { count };

    let mut body = Vec::new();
    if version == 0 {
        // Version 0 counts entries in sixteen bits. A file already holding 65535 items is not
        // one this rewrites, and saying so is better than wrapping the count.
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(
            &u16::try_from(new_count)
                .map_err(|_| failed())?
                .to_be_bytes(),
        );
    } else {
        body.extend_from_slice(&[version, 0, 0, 0]);
        body.extend_from_slice(&new_count.to_be_bytes());
    }
    body.extend_from_slice(entries);
    if with_exif {
        body.extend_from_slice(&build_infe(exif_item_id, b"Exif")?);
    }

    box_with_type(b"iinf", &body)
}

/// One item information entry, version 2, with an empty name.
fn build_infe(item_id: u32, item_type: &[u8; 4]) -> Result<Vec<u8>, TransformError> {
    let mut body = vec![2, 0, 0, 0];
    body.extend_from_slice(
        &u16::try_from(item_id)
            .map_err(|_| avif_write_failed("the item id is too large"))?
            .to_be_bytes(),
    );
    body.extend_from_slice(&0u16.to_be_bytes());
    body.extend_from_slice(item_type);
    body.push(0);
    box_with_type(b"infe", &body)
}

/// The item location box, rewritten as version 0 with four-byte offsets and lengths.
///
/// Reading normalizes every form into absolute file offsets, so writing one form back is what
/// keeps this short: the base offset is folded in, and a construction method that is not a file
/// offset was refused while reading.
fn build_iloc(
    locations: &[AvifItemLocation],
    exif_item_id: u32,
    exif_extent: Option<AvifExtent>,
    shift: i64,
) -> Result<Vec<u8>, TransformError> {
    let mut entries: Vec<(u32, Vec<AvifExtent>)> = Vec::with_capacity(locations.len() + 1);
    for location in locations {
        let mut extents = Vec::with_capacity(location.extents.len());
        for extent in &location.extents {
            let moved = i64::try_from(extent.offset)
                .ok()
                .and_then(|offset| offset.checked_add(shift))
                .and_then(|offset| u64::try_from(offset).ok())
                .ok_or_else(|| avif_write_failed("an item offset moved out of range"))?;
            extents.push(AvifExtent {
                offset: moved,
                length: extent.length,
            });
        }
        entries.push((location.item_id, extents));
    }
    if let Some(extent) = exif_extent {
        entries.push((exif_item_id, vec![extent]));
    }

    let mut body = vec![0, 0, 0, 0];
    body.push(0x44); // four-byte offsets, four-byte lengths
    body.push(0x00); // no base offset, no extent index
    body.extend_from_slice(
        &u16::try_from(entries.len())
            .map_err(|_| avif_write_failed("there are too many items"))?
            .to_be_bytes(),
    );
    for (item_id, extents) in &entries {
        body.extend_from_slice(
            &u16::try_from(*item_id)
                .map_err(|_| avif_write_failed("an item id is too large"))?
                .to_be_bytes(),
        );
        body.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
        body.extend_from_slice(
            &u16::try_from(extents.len())
                .map_err(|_| avif_write_failed("an item has too many extents"))?
                .to_be_bytes(),
        );
        for extent in extents {
            body.extend_from_slice(
                &u32::try_from(extent.offset)
                    .map_err(|_| avif_write_failed("an item offset does not fit four bytes"))?
                    .to_be_bytes(),
            );
            body.extend_from_slice(
                &u32::try_from(extent.length)
                    .map_err(|_| avif_write_failed("an item length does not fit four bytes"))?
                    .to_be_bytes(),
            );
        }
    }

    box_with_type(b"iloc", &body)
}

/// The item reference box with a `cdsc` from the EXIF item to the primary item.
///
/// `cdsc` is what says the EXIF describes the picture rather than being a picture of its own;
/// without it a reader has an item of type `Exif` and nothing saying which item it belongs to.
fn build_iref(
    existing: Option<&[u8]>,
    primary_item: u32,
    exif_item_id: u32,
    with_exif: bool,
) -> Result<Vec<u8>, TransformError> {
    let mut body = match existing {
        // Version and flags, then the reference boxes, which are copied through.
        Some(payload) => payload.to_vec(),
        None => vec![0, 0, 0, 0],
    };
    if with_exif {
        let mut cdsc = Vec::new();
        cdsc.extend_from_slice(
            &u16::try_from(exif_item_id)
                .map_err(|_| avif_write_failed("the item id is too large"))?
                .to_be_bytes(),
        );
        cdsc.extend_from_slice(&1u16.to_be_bytes());
        cdsc.extend_from_slice(
            &u16::try_from(primary_item)
                .map_err(|_| avif_write_failed("the primary item id is too large"))?
                .to_be_bytes(),
        );
        body.extend_from_slice(&box_with_type(b"cdsc", &cdsc)?);
    }
    box_with_type(b"iref", &body)
}

/// The item properties box with a `colr` holding the ICC profile, associated with the primary
/// item.
///
/// The property goes at the end of `ipco`, so every position `ipma` already names keeps naming
/// the same property; the association is added to the primary item's list and is not essential,
/// which is what lets a reader that ignores colour profiles still show the picture.
fn build_iprp(
    payload: &[u8],
    primary_item: u32,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, TransformError> {
    let Some(icc) = icc else {
        return box_with_type(b"iprp", payload);
    };

    let failed = || avif_write_failed("the item properties cannot be rewritten");
    let ipco = find_child(payload, b"ipco").ok_or_else(failed)?;
    let ipma = find_child(payload, b"ipma").ok_or_else(failed)?;

    let mut property_count = 0u16;
    let mut offset = 0;
    while offset + 8 <= ipco.len() {
        let (_, _, next_offset) = parse_mp4_box(ipco, offset).map_err(|_| failed())?;
        property_count = property_count.checked_add(1).ok_or_else(failed)?;
        if next_offset <= offset {
            return Err(failed());
        }
        offset = next_offset;
    }

    let mut colr = Vec::with_capacity(icc.len() + 4);
    colr.extend_from_slice(b"prof");
    colr.extend_from_slice(icc);
    let mut new_ipco = ipco.to_vec();
    new_ipco.extend_from_slice(&box_with_type(b"colr", &colr)?);
    let icc_position = property_count.checked_add(1).ok_or_else(failed)?;

    let mut body = box_with_type(b"ipco", &new_ipco)?;
    body.extend_from_slice(&build_ipma(ipma, primary_item, icc_position)?);
    box_with_type(b"iprp", &body)
}

/// The item property association box with one more association on the primary item.
///
/// The essential bit of every association that is already there is copied, since it is what
/// tells a reader to refuse a file whose property it cannot apply; the ICC profile's own is
/// clear, because a reader that ignores it shows the picture in the wrong colour space rather
/// than showing the wrong picture.
fn build_ipma(
    payload: &[u8],
    primary_item: u32,
    icc_position: u16,
) -> Result<Vec<u8>, TransformError> {
    let failed = || avif_write_failed("the property associations cannot be rewritten");
    let version = *payload.first().ok_or_else(failed)?;
    let wide = payload.get(3).ok_or_else(failed)? & 1 == 1;
    let entry_count = read_u32_be(payload.get(4..8).ok_or_else(failed)?)?;

    let mut offset = 8;
    let mut entries: Vec<(u32, Vec<u16>)> = Vec::new();
    for _ in 0..entry_count {
        let item = if version == 0 {
            let item = read_u16_be(payload.get(offset..offset + 2).ok_or_else(failed)?)?;
            offset += 2;
            u32::from(item)
        } else {
            let item = read_u32_be(payload.get(offset..offset + 4).ok_or_else(failed)?)?;
            offset += 4;
            item
        };
        let count = *payload.get(offset).ok_or_else(failed)?;
        offset += 1;
        let mut associations = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            if wide {
                let association = read_u16_be(payload.get(offset..offset + 2).ok_or_else(failed)?)?;
                offset += 2;
                associations.push(association);
            } else {
                let association = *payload.get(offset).ok_or_else(failed)?;
                offset += 1;
                // Widen the one-byte form: the essential bit moves from 0x80 to 0x8000 and the
                // position keeps its value, so the two forms hold the same associations.
                associations
                    .push((u16::from(association & 0x80) << 8) | u16::from(association & 0x7F));
            }
        }
        entries.push((item, associations));
    }

    let mut added = false;
    for (item, associations) in &mut entries {
        if *item == primary_item {
            associations.push(icc_position);
            added = true;
        }
    }
    if !added {
        entries.push((primary_item, vec![icc_position]));
    }

    // Every position still fits seven bits unless the file already had more than 126
    // properties, in which case the wide form is what can name the new one.
    let wide_out = entries
        .iter()
        .flat_map(|(_, associations)| associations.iter())
        .any(|association| association & 0x7FFF > 0x7F);
    let wide_ids = entries.iter().any(|(item, _)| *item > u32::from(u16::MAX));

    let mut body = vec![u8::from(wide_ids), 0, 0, u8::from(wide_out)];
    body.extend_from_slice(
        &u32::try_from(entries.len())
            .map_err(|_| failed())?
            .to_be_bytes(),
    );
    for (item, associations) in &entries {
        if wide_ids {
            body.extend_from_slice(&item.to_be_bytes());
        } else {
            body.extend_from_slice(&u16::try_from(*item).map_err(|_| failed())?.to_be_bytes());
        }
        body.push(u8::try_from(associations.len()).map_err(|_| failed())?);
        for association in associations {
            if wide_out {
                body.extend_from_slice(&association.to_be_bytes());
            } else {
                let essential = u8::from(association & 0x8000 != 0) << 7;
                body.push(essential | u8::try_from(association & 0x7F).map_err(|_| failed())?);
            }
        }
    }

    box_with_type(b"ipma", &body)
}

/// Wraps a payload in a box header of the given type.
fn box_with_type(box_type: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>, TransformError> {
    let size = u32::try_from(payload.len() + 8)
        .map_err(|_| avif_write_failed("a box does not fit a 32-bit size"))?;
    let mut boxed = Vec::with_capacity(payload.len() + 8);
    boxed.extend_from_slice(&size.to_be_bytes());
    boxed.extend_from_slice(box_type);
    boxed.extend_from_slice(payload);
    Ok(boxed)
}

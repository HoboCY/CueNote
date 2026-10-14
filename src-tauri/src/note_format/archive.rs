use std::collections::{BTreeMap, HashSet};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::package::{FormatError, NotePackage};
use super::validation::{
    parse_document_json, parse_manifest_json, validate_json_depth, validate_package, MAX_ASSETS,
    MAX_DOCUMENT_BYTES, MAX_EXPANDED_BYTES, MAX_MANIFEST_BYTES, MAX_PACKAGE_BYTES,
};
use super::{AssetManifest, Document};

const EOCD_SIG: u32 = 0x0605_4b50;
const CDR_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const DESCRIPTOR_SIG: u32 = 0x0807_4b50;
const EOCD_MIN_SIZE: usize = 22;
const CDR_SIZE: usize = 46;
const LOCAL_SIZE: usize = 30;
const MAX_CENTRAL_DIRECTORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRY_COUNT: usize = MAX_ASSETS + 2;

#[derive(Clone, Debug)]
struct CentralEntry {
    name: String,
    flags: u16,
    method: u16,
    crc32: u32,
    compressed_size: u32,
    size: u32,
    local_offset: u32,
}

pub(crate) fn read<R: Read + Seek>(mut reader: R) -> Result<NotePackage, FormatError> {
    let declared_file_size = reader.seek(SeekFrom::End(0)).map_err(archive_error)?;
    if declared_file_size > MAX_PACKAGE_BYTES {
        return Err(budget_error("package exceeds the 256 MiB limit"));
    }
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| archive_error(error))?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(archive_error)?;
        if read == 0 {
            break;
        }
        let next_len = bytes
            .len()
            .checked_add(read)
            .ok_or_else(|| budget_error("package exceeds the 256 MiB limit"))?;
        if next_len as u64 > MAX_PACKAGE_BYTES {
            return Err(budget_error("package exceeds the 256 MiB limit"));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let (entries, central_offset) = preflight(&bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(archive_error)?;
    if archive.len() != entries.len() {
        return Err(archive_error("ZIP entry count changed after preflight"));
    }

    let mut document_bytes = None;
    let mut manifest_bytes = None;
    let mut assets = BTreeMap::new();
    let mut expanded = 0u64;
    for (index, expected) in entries.iter().enumerate() {
        let mut file = archive.by_index(index).map_err(archive_error)?;
        let name = file.name().map_err(archive_error)?;
        let method_matches = match expected.method {
            0 => file.compression() == CompressionMethod::Stored,
            8 => file.compression() == CompressionMethod::Deflated,
            _ => false,
        };
        if name.as_ref() != expected.name
            || !method_matches
            || file.size() != u64::from(expected.size)
            || file.compressed_size() != u64::from(expected.compressed_size)
        {
            return Err(archive_error(
                "ZIP central-directory entry disagrees with preflight",
            ));
        }
        let maximum = match expected.name.as_str() {
            "document.json" => MAX_DOCUMENT_BYTES,
            "assets/manifest.json" => MAX_MANIFEST_BYTES,
            _ => MAX_EXPANDED_BYTES,
        };
        if u64::from(expected.size) > maximum {
            return Err(budget_error(format!(
                "{} exceeds its size limit",
                expected.name
            )));
        }
        let mut data = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut chunk).map_err(archive_error)?;
            if count == 0 {
                break;
            }
            let new_len = data
                .len()
                .checked_add(count)
                .ok_or_else(|| budget_error("expanded package exceeds 512 MiB"))?;
            if new_len as u64 > maximum {
                return Err(budget_error(format!(
                    "{} exceeds its size limit",
                    expected.name
                )));
            }
            expanded = expanded
                .checked_add(count as u64)
                .ok_or_else(|| budget_error("expanded package exceeds 512 MiB"))?;
            if expanded > MAX_EXPANDED_BYTES {
                return Err(budget_error("expanded package exceeds 512 MiB"));
            }
            data.extend_from_slice(&chunk[..count]);
        }
        if data.len() != expected.size as usize {
            return Err(archive_error(
                "ZIP entry expanded size does not match its declaration",
            ));
        }
        match expected.name.as_str() {
            "document.json" => document_bytes = Some(data),
            "assets/manifest.json" => manifest_bytes = Some(data),
            asset_path => {
                let id = asset_path
                    .strip_prefix("assets/")
                    .ok_or_else(|| archive_error("unexpected ZIP path"))?;
                assets.insert(id.to_owned(), data);
            }
        }
    }
    let document_bytes = document_bytes.ok_or_else(|| archive_error("document.json is missing"))?;
    let manifest_bytes =
        manifest_bytes.ok_or_else(|| archive_error("assets/manifest.json is missing"))?;
    let document: Document = parse_document_json(&document_bytes)?;
    let manifest: AssetManifest = parse_manifest_json(&manifest_bytes)?;
    let package = NotePackage {
        document,
        manifest,
        assets,
    };
    validate_package(&package)?;
    let end = archive
        .into_inner()
        .seek(SeekFrom::End(0))
        .map_err(archive_error)?;
    if end < central_offset as u64 {
        return Err(archive_error(
            "ZIP central directory lies beyond end of file",
        ));
    }
    Ok(package)
}

pub(crate) fn write<W: Write + Seek>(
    mut writer: W,
    package: &NotePackage,
) -> Result<(), FormatError> {
    let initial_end = writer.seek(SeekFrom::End(0)).map_err(archive_error)?;
    if initial_end != 0 {
        return Err(FormatError::new(
            "invalid_archive",
            "The package writer requires an empty destination positioned at offset zero.",
        ));
    }
    writer.seek(SeekFrom::Start(0)).map_err(archive_error)?;
    validate_package(package)?;
    let document = serde_json::to_vec(&package.document)
        .map_err(|error| FormatError::new("invalid_document", error.to_string()))?;
    let manifest = serde_json::to_vec(&package.manifest)
        .map_err(|error| FormatError::new("invalid_manifest", error.to_string()))?;
    if document.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(budget_error("document.json exceeds 64 MiB"));
    }
    if manifest.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(budget_error("assets/manifest.json exceeds 16 MiB"));
    }
    validate_json_depth(&document, "invalid_document")?;
    validate_json_depth(&manifest, "invalid_manifest")?;
    let expanded = document
        .len()
        .checked_add(manifest.len())
        .and_then(|sum| {
            package
                .assets
                .values()
                .try_fold(sum, |sum, asset| sum.checked_add(asset.len()))
        })
        .ok_or_else(|| budget_error("expanded package exceeds 512 MiB"))?;
    if expanded as u64 > MAX_EXPANDED_BYTES {
        return Err(budget_error("expanded package exceeds 512 MiB"));
    }

    let bounded = BoundedWriter {
        inner: writer,
        position: 0,
        length: 0,
    };
    let mut zip = ZipWriter::new(bounded);
    let json_options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let asset_options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    write_entry(&mut zip, "document.json", &document, json_options)?;
    write_entry(&mut zip, "assets/manifest.json", &manifest, asset_options)?;
    for (asset_id, bytes) in &package.assets {
        write_entry(
            &mut zip,
            &format!("assets/{asset_id}"),
            bytes,
            asset_options,
        )?;
    }
    let bounded = zip.finish().map_err(write_archive_error)?;
    if bounded.length as u64 > MAX_PACKAGE_BYTES {
        return Err(budget_error("package exceeds 256 MiB"));
    }
    Ok(())
}

pub(crate) fn check_encoded_size(package: &NotePackage) -> Result<(), FormatError> {
    write(CountingWriter::default(), package)
}

fn write_entry<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    name: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<(), FormatError> {
    zip.start_file(name, options).map_err(write_archive_error)?;
    zip.write_all(bytes).map_err(write_archive_error)
}

struct BoundedWriter<W> {
    inner: W,
    position: u64,
    length: u64,
}

#[derive(Default)]
struct CountingWriter {
    position: u64,
    length: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .position
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("package exceeds 256 MiB"))?;
        if end > MAX_PACKAGE_BYTES {
            return Err(io::Error::other("package exceeds 256 MiB"));
        }
        self.position = end;
        self.length = self.length.max(end);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for CountingWriter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => self
                .length
                .checked_add_signed(offset)
                .ok_or_else(|| io::Error::other("invalid package seek"))?,
            SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or_else(|| io::Error::other("invalid package seek"))?,
        };
        if next > MAX_PACKAGE_BYTES {
            return Err(io::Error::other("package exceeds 256 MiB"));
        }
        self.position = next;
        Ok(next)
    }
}

impl<W: Write + Seek> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .position
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("package exceeds 256 MiB"))?;
        if end > MAX_PACKAGE_BYTES {
            return Err(io::Error::other("package exceeds 256 MiB"));
        }
        let count = self.inner.write(bytes)?;
        self.position += count as u64;
        self.length = self.length.max(self.position);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write + Seek> Seek for BoundedWriter<W> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let new_position = match position {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => {
                let end = self.inner.seek(SeekFrom::End(0))?;
                self.inner.seek(SeekFrom::Start(self.position))?;
                end.checked_add_signed(offset)
                    .ok_or_else(|| io::Error::other("invalid package seek"))?
            }
            SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or_else(|| io::Error::other("invalid package seek"))?,
        };
        if new_position > MAX_PACKAGE_BYTES {
            return Err(io::Error::other("package exceeds 256 MiB"));
        }
        let actual = self.inner.seek(SeekFrom::Start(new_position))?;
        self.position = actual;
        Ok(actual)
    }
}

fn preflight(bytes: &[u8]) -> Result<(Vec<CentralEntry>, u32), FormatError> {
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(budget_error("package exceeds 256 MiB"));
    }
    if bytes.len() < EOCD_MIN_SIZE {
        return Err(archive_error(
            "ZIP end-of-central-directory record is missing",
        ));
    }
    let search_start = bytes
        .len()
        .saturating_sub(EOCD_MIN_SIZE + u16::MAX as usize);
    let eocd = (search_start..=bytes.len() - EOCD_MIN_SIZE)
        .rev()
        .find(|offset| get_u32(bytes, *offset) == Some(EOCD_SIG))
        .ok_or_else(|| archive_error("ZIP end-of-central-directory record is missing"))?;
    let comment_len =
        get_u16(bytes, eocd + 20).ok_or_else(|| archive_error("truncated ZIP EOCD"))? as usize;
    if eocd + EOCD_MIN_SIZE + comment_len != bytes.len() {
        return Err(archive_error(
            "ZIP has trailing data or a malformed comment",
        ));
    }
    let disk = get_u16(bytes, eocd + 4).unwrap_or(u16::MAX);
    let cd_disk = get_u16(bytes, eocd + 6).unwrap_or(u16::MAX);
    let disk_count = get_u16(bytes, eocd + 8).unwrap_or(u16::MAX);
    let total_count = get_u16(bytes, eocd + 10).unwrap_or(u16::MAX);
    let cd_size = get_u32(bytes, eocd + 12).unwrap_or(u32::MAX);
    let cd_offset = get_u32(bytes, eocd + 16).unwrap_or(u32::MAX);
    if disk != 0 || cd_disk != 0 || disk_count != total_count {
        return Err(archive_error("multi-volume ZIP archives are not supported"));
    }
    if disk_count == u16::MAX || cd_size == u32::MAX || cd_offset == u32::MAX {
        return Err(archive_error("ZIP64 packages are not supported"));
    }
    if disk_count as usize > MAX_ENTRY_COUNT {
        return Err(budget_error("ZIP contains more than 8194 entries"));
    }
    if cd_size as usize > MAX_CENTRAL_DIRECTORY_BYTES {
        return Err(budget_error("ZIP central directory exceeds 4 MiB"));
    }
    let cd_end = (cd_offset as usize)
        .checked_add(cd_size as usize)
        .ok_or_else(|| archive_error("invalid ZIP central-directory bounds"))?;
    if cd_end != eocd {
        return Err(archive_error(
            "ZIP central directory is not immediately before EOCD",
        ));
    }

    let mut entries = Vec::with_capacity(disk_count as usize);
    let mut names = HashSet::new();
    let mut expected_offset = cd_offset as usize;
    let mut expanded_total = 0u64;
    for _ in 0..disk_count {
        if expected_offset
            .checked_add(CDR_SIZE)
            .is_none_or(|end| end > cd_end)
            || get_u32(bytes, expected_offset) != Some(CDR_SIG)
        {
            return Err(archive_error("invalid ZIP central-directory entry"));
        }
        let flags = get_u16(bytes, expected_offset + 8).unwrap();
        let method = get_u16(bytes, expected_offset + 10).unwrap();
        let crc32 = get_u32(bytes, expected_offset + 16).unwrap();
        let compressed_size = get_u32(bytes, expected_offset + 20).unwrap();
        let size = get_u32(bytes, expected_offset + 24).unwrap();
        let name_len = get_u16(bytes, expected_offset + 28).unwrap() as usize;
        let extra_len = get_u16(bytes, expected_offset + 30).unwrap() as usize;
        let comment_len = get_u16(bytes, expected_offset + 32).unwrap() as usize;
        let start_disk = get_u16(bytes, expected_offset + 34).unwrap();
        let external_attrs = get_u32(bytes, expected_offset + 38).unwrap();
        let local_offset = get_u32(bytes, expected_offset + 42).unwrap();
        let name_start = expected_offset + CDR_SIZE;
        let entry_end = name_start
            .checked_add(name_len)
            .and_then(|end| end.checked_add(extra_len))
            .and_then(|end| end.checked_add(comment_len))
            .ok_or_else(|| archive_error("invalid ZIP entry bounds"))?;
        if entry_end > cd_end || name_len == 0 {
            return Err(archive_error("truncated ZIP central-directory entry"));
        }
        if start_disk != 0
            || local_offset == u32::MAX
            || compressed_size == u32::MAX
            || size == u32::MAX
        {
            return Err(archive_error(
                "multi-volume and ZIP64 entries are not supported",
            ));
        }
        if flags & !0x0808 != 0 || flags & 1 != 0 {
            return Err(archive_error(
                "encrypted ZIP entries or unsupported flags are not permitted",
            ));
        }
        if method != 0 && method != 8 {
            return Err(archive_error(
                "only stored and deflated ZIP entries are supported",
            ));
        }
        let extra_start = name_start + name_len;
        if extra_len != 0 {
            let mut cursor = extra_start;
            let extra_end = cursor + extra_len;
            while cursor < extra_end {
                if cursor + 4 > extra_end {
                    return Err(archive_error("malformed ZIP extra field"));
                }
                let tag = get_u16(bytes, cursor).unwrap();
                let length = get_u16(bytes, cursor + 2).unwrap() as usize;
                cursor += 4 + length;
                if cursor > extra_end {
                    return Err(archive_error("malformed ZIP extra field"));
                }
                if tag == 0x0001 || tag == 0x7075 || tag == 0x6375 {
                    return Err(archive_error(
                        "ZIP64 and alternate-name fields are not permitted",
                    ));
                }
            }
        }
        let raw_name = &bytes[name_start..name_start + name_len];
        if !raw_name.is_ascii() {
            return Err(archive_error("ZIP entry paths must be ASCII"));
        }
        let name = std::str::from_utf8(raw_name)
            .map_err(|_| archive_error("invalid ZIP entry path"))?
            .to_owned();
        validate_path(&name)?;
        if !names.insert(name.clone()) {
            return Err(archive_error("duplicate ZIP entry path"));
        }
        if (external_attrs >> 16) & 0o170000 == 0o120000 {
            return Err(archive_error("symbolic-link ZIP entries are not permitted"));
        }
        expanded_total = expanded_total
            .checked_add(u64::from(size))
            .ok_or_else(|| budget_error("expanded package exceeds 512 MiB"))?;
        if expanded_total > MAX_EXPANDED_BYTES {
            return Err(budget_error("expanded package exceeds 512 MiB"));
        }
        entries.push(CentralEntry {
            name,
            flags,
            method,
            crc32,
            compressed_size,
            size,
            local_offset,
        });
        expected_offset = entry_end;
    }
    if expected_offset != cd_end {
        return Err(archive_error(
            "ZIP central-directory size does not match its entries",
        ));
    }
    if !names.contains("document.json") || !names.contains("assets/manifest.json") {
        return Err(archive_error(
            "document.json and assets/manifest.json are required",
        ));
    }
    validate_local_records(bytes, &entries, cd_offset as usize)?;
    Ok((entries, cd_offset))
}

fn validate_path(name: &str) -> Result<(), FormatError> {
    if name == "document.json" || name == "assets/manifest.json" {
        return Ok(());
    }
    let Some(id) = name.strip_prefix("assets/") else {
        return Err(archive_error("unexpected ZIP entry path"));
    };
    if id.contains('/') || id.contains('\\') || id.is_empty() {
        return Err(archive_error(
            "ZIP directories and path aliases are not permitted",
        ));
    }
    let uuid = uuid::Uuid::parse_str(id).map_err(|_| archive_error("invalid asset ZIP path"))?;
    if uuid.get_version_num() != 4 || uuid.to_string() != id {
        return Err(archive_error(
            "asset ZIP paths must be lowercase UUIDv4 values",
        ));
    }
    Ok(())
}

fn validate_local_records(
    bytes: &[u8],
    entries: &[CentralEntry],
    central_offset: usize,
) -> Result<(), FormatError> {
    let mut spans = Vec::with_capacity(entries.len());
    for entry in entries {
        let start = entry.local_offset as usize;
        if start
            .checked_add(LOCAL_SIZE)
            .is_none_or(|end| end > bytes.len())
            || get_u32(bytes, start) != Some(LOCAL_SIG)
        {
            return Err(archive_error("invalid ZIP local file header"));
        }
        let flags =
            get_u16(bytes, start + 6).ok_or_else(|| archive_error("truncated ZIP local header"))?;
        let method = get_u16(bytes, start + 8).unwrap();
        let local_crc = get_u32(bytes, start + 14).unwrap();
        let local_compressed = get_u32(bytes, start + 18).unwrap();
        let local_size = get_u32(bytes, start + 22).unwrap();
        let name_len = get_u16(bytes, start + 26).unwrap() as usize;
        let extra_len = get_u16(bytes, start + 28).unwrap() as usize;
        let name_start = start + LOCAL_SIZE;
        let data_start = name_start
            .checked_add(name_len)
            .and_then(|at| at.checked_add(extra_len))
            .ok_or_else(|| archive_error("invalid ZIP local header bounds"))?;
        let data_end = data_start
            .checked_add(entry.compressed_size as usize)
            .ok_or_else(|| archive_error("invalid ZIP compressed-data bounds"))?;
        if data_end > central_offset || name_start + name_len > bytes.len() {
            return Err(archive_error("truncated ZIP local file entry"));
        }
        if &bytes[name_start..name_start + name_len] != entry.name.as_bytes()
            || flags != entry.flags
            || method != entry.method
        {
            return Err(archive_error(
                "ZIP local header disagrees with central directory",
            ));
        }
        if extra_len != 0 {
            return Err(archive_error("ZIP local extra fields are not permitted"));
        }
        let mut end = data_end;
        if flags & 0x0008 != 0 {
            if (local_crc != 0 && local_crc != entry.crc32)
                || (local_compressed != 0 && local_compressed != entry.compressed_size)
                || (local_size != 0 && local_size != entry.size)
            {
                return Err(archive_error(
                    "ZIP local sizes disagree with central directory",
                ));
            }
            let has_sig = get_u32(bytes, end) == Some(DESCRIPTOR_SIG);
            if has_sig {
                end += 4;
            }
            if get_u32(bytes, end) != Some(entry.crc32)
                || get_u32(bytes, end + 4) != Some(entry.compressed_size)
                || get_u32(bytes, end + 8) != Some(entry.size)
            {
                return Err(archive_error(
                    "ZIP data descriptor disagrees with central directory",
                ));
            }
            end += 12;
        } else if local_crc != entry.crc32
            || local_compressed != entry.compressed_size
            || local_size != entry.size
        {
            return Err(archive_error(
                "ZIP local sizes disagree with central directory",
            ));
        }
        if end > central_offset {
            return Err(archive_error("ZIP entry overlaps its central directory"));
        }
        spans.push((start, end));
    }
    spans.sort_unstable();
    let mut cursor = 0usize;
    for (start, end) in spans {
        if start != cursor {
            return Err(archive_error(
                "ZIP contains hidden data, gaps, or overlapping entries",
            ));
        }
        cursor = end;
    }
    if cursor != central_offset {
        return Err(archive_error(
            "ZIP contains unindexed data before its central directory",
        ));
    }
    Ok(())
}

fn get_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn get_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn archive_error(error: impl std::fmt::Display) -> FormatError {
    FormatError::new("invalid_archive", error.to_string())
}

fn write_archive_error(error: impl std::fmt::Display) -> FormatError {
    let message = error.to_string();
    if message
        .to_ascii_lowercase()
        .contains("package exceeds 256 mib")
    {
        budget_error(message)
    } else {
        archive_error(message)
    }
}

fn budget_error(message: impl Into<String>) -> FormatError {
    FormatError::new("budget_exceeded", message)
}

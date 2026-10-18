use std::collections::HashMap;
use std::io::{Cursor, Write};

use cuenote_lib::note_format::{
    read_package, write_package, AssetManifestEntry, Block, Inline, Mark, MediaType, MindMapNode,
    NotePackage, PlainText,
};
use sha2::Digest;

const VALID_RICH: &[u8] = include_bytes!("fixtures/valid-rich-v1.cuenote");

// Included in the precompiled device resource suite as well as hosted CI.
// CPU backend changes must preserve note revisions and asset identities.
#[test]
fn sha256_preserves_canonical_digests_across_block_boundaries() {
    let cases = [
        (
            Vec::new(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc".to_vec(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".to_vec(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            vec![b'a'; 1_000_000],
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(format!("{:x}", sha2::Sha256::digest(&input)), expected);
        for chunk_size in [1, 63, 64, 65, 4096] {
            let mut digest = sha2::Sha256::new();
            for chunk in input.chunks(chunk_size) {
                digest.update(chunk);
            }
            assert_eq!(format!("{:x}", digest.finalize()), expected);
        }
    }
}

#[test]
fn previously_decoded_image_does_not_authorize_changed_bytes_or_metadata() {
    let package = read_package(Cursor::new(VALID_RICH)).unwrap();
    let asset = package
        .manifest
        .assets
        .iter()
        .find(|entry| entry.media_type == MediaType::Png)
        .unwrap();
    let id = asset.id.clone();
    // Reading and writing first exercise the success path before any mutation.
    let mut encoded = Cursor::new(Vec::new());
    write_package(&mut encoded, &package).unwrap();
    read_package(Cursor::new(encoded.into_inner())).unwrap();

    let mut corrupt = package.clone();
    let header_only = corrupt.assets[&id][..33].to_vec();
    update_asset(&mut corrupt, &id, header_only);
    let error = write_package(&mut Cursor::new(Vec::new()), &corrupt).unwrap_err();
    assert_eq!(error.kind, "invalid_asset");
    assert_eq!(
        read_package(Cursor::new(archive_from_package(&corrupt)))
            .unwrap_err()
            .kind,
        "invalid_asset"
    );

    let mut wrong_dimensions = package.clone();
    wrong_dimensions
        .manifest
        .assets
        .iter_mut()
        .find(|entry| entry.id == id)
        .unwrap()
        .width += 1;
    assert_eq!(
        write_package(&mut Cursor::new(Vec::new()), &wrong_dimensions)
            .unwrap_err()
            .kind,
        "asset_integrity"
    );
    let mut wrong_type = package;
    wrong_type
        .manifest
        .assets
        .iter_mut()
        .find(|entry| entry.id == id)
        .unwrap()
        .media_type = MediaType::Jpeg;
    assert_eq!(
        write_package(&mut Cursor::new(Vec::new()), &wrong_type)
            .unwrap_err()
            .kind,
        "invalid_asset"
    );
}

#[test]
fn rich_v1_fixture_reads_and_round_trips_every_approved_block() {
    let package = read_package(Cursor::new(VALID_RICH)).expect("fixed v1 fixture is valid");
    let content = &package.document.sections.content;
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::Heading1 { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::Heading2 { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::Heading3 { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::OrderedList { start: 2, .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::BulletList { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::TaskList { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::Blockquote { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::CodeBlock { language: None, .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::HorizontalRule)));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::Table { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::MathBlock { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::Image { .. })));
    assert!(content
        .iter()
        .any(|block| matches!(block, Block::MindMap { .. })));
    let Block::Paragraph { children } = &content[0] else {
        panic!("fixture begins with a paragraph");
    };
    assert!(children
        .iter()
        .any(|inline| matches!(inline, Inline::MathInline { .. })));
    assert!(children
        .iter()
        .any(|inline| matches!(inline, Inline::HardBreak)));
    let Inline::Text { marks, .. } = &children[3] else {
        panic!("fixture includes a marked text run");
    };
    assert!(marks.iter().any(|mark| matches!(mark, Mark::Bold)));
    assert!(marks.iter().any(|mark| matches!(mark, Mark::Italic)));
    assert!(marks.iter().any(|mark| matches!(mark, Mark::Strike)));
    assert!(marks.iter().any(|mark| matches!(mark, Mark::Code)));
    assert!(marks.iter().any(|mark| matches!(mark, Mark::Link { .. })));
    assert_eq!(package.assets.len(), 1);

    let disk_document: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/document-rich-v1.json")).unwrap();
    let encoded_document = serde_json::to_value(&package.document).unwrap();
    assert_eq!(encoded_document, disk_document);

    let mut output = Cursor::new(Vec::new());
    write_package(&mut output, &package).expect("valid v1 package writes");
    let written = read_package(Cursor::new(output.into_inner())).expect("written package reads");
    assert_eq!(written, package);
}

#[test]
fn invalid_fixed_packages_return_precise_format_kinds() {
    let cases: [(&str, &[u8], &str); 7] = [
        (
            "unsupported-version.cuenote",
            include_bytes!("fixtures/unsupported-version.cuenote"),
            "unsupported_version",
        ),
        (
            "unsupported-manifest-version.cuenote",
            include_bytes!("fixtures/unsupported-manifest-version.cuenote"),
            "unsupported_version",
        ),
        (
            "duplicate-json-key.cuenote",
            include_bytes!("fixtures/duplicate-json-key.cuenote"),
            "invalid_document",
        ),
        (
            "missing-asset.cuenote",
            include_bytes!("fixtures/missing-asset.cuenote"),
            "invalid_manifest",
        ),
        (
            "corrupt-asset.cuenote",
            include_bytes!("fixtures/corrupt-asset.cuenote"),
            "asset_integrity",
        ),
        (
            "path-alias.cuenote",
            include_bytes!("fixtures/path-alias.cuenote"),
            "invalid_archive",
        ),
        (
            "duplicate-path.cuenote",
            include_bytes!("fixtures/duplicate-path.cuenote"),
            "invalid_archive",
        ),
    ];
    for (name, bytes, expected_kind) in cases {
        let error = read_package(Cursor::new(bytes)).expect_err(name);
        assert_eq!(error.kind, expected_kind, "fixture {name}: {error}");
    }
}

#[test]
fn plain_editor_projection_is_lossless_only_for_supported_plain_nodes() {
    let rich = read_package(Cursor::new(VALID_RICH)).unwrap();
    assert!(rich.plain_text().is_none());
    let projected = rich.text_projection();
    assert!(projected.content.contains("All approved blocks"));
    assert!(projected
        .content
        .contains("[image 11111111-1111-4111-8111-111111111111]"));
    assert!(projected.content.contains("root\n  child"));

    let text = PlainText {
        title: "Plain title".into(),
        cue: "cue line 1\ncue line 2".into(),
        content: "body".into(),
        summary: "summary".into(),
    };
    let mut plain =
        NotePackage::new_plain("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(), text.clone());
    assert_eq!(plain.plain_text(), Some(text.clone()));
    let identity = plain.document.note_id.clone();
    let organization = plain.document.organization.clone();
    let created = plain.document.timestamps.created_at.clone();
    let content_modified = plain.document.timestamps.content_modified_at.clone();
    plain.replace_plain_text(text).unwrap();
    assert_eq!(
        plain.document.timestamps.content_modified_at,
        content_modified
    );
    plain
        .replace_plain_text(PlainText {
            title: "Updated".into(),
            cue: "cue line 1\ncue line 2".into(),
            content: "body".into(),
            summary: "summary".into(),
        })
        .unwrap();
    assert_eq!(plain.document.note_id, identity);
    assert_eq!(plain.document.organization, organization);
    assert_eq!(plain.document.timestamps.created_at, created);
    assert!(
        plain.document.timestamps.content_modified_at > content_modified,
        "{} should follow {}",
        plain.document.timestamps.content_modified_at,
        content_modified
    );
}

#[test]
fn generated_schema_matches_required_nullable_v1_fields() {
    let schema: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../schemas/document-v1.schema.json")).unwrap();
    let organization = &schema["$defs"]["Organization"];
    for field in ["notebookId", "trashedAt"] {
        assert!(organization["required"]
            .as_array()
            .unwrap()
            .contains(&field.into()));
        assert!(
            schema_allows_null(&organization["properties"][field]),
            "{field}"
        );
    }
    let block = &schema["$defs"]["Block"];
    let code_language = block["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["type"]["const"] == "codeBlock")
        .expect("codeBlock schema variant");
    assert!(code_language["required"]
        .as_array()
        .unwrap()
        .contains(&"language".into()));
    assert!(schema_allows_null(&code_language["properties"]["language"]));
}

fn schema_allows_null(schema: &serde_json::Value) -> bool {
    schema["type"] == "null"
        || schema["type"]
            .as_array()
            .is_some_and(|types| types.iter().any(|kind| kind == "null"))
        || schema["anyOf"]
            .as_array()
            .is_some_and(|variants| variants.iter().any(schema_allows_null))
}

#[test]
fn package_byte_limit_accepts_exact_size_and_rejects_oversize() {
    use cuenote_lib::note_format::MAX_PACKAGE_BYTES;
    let small = read_package(Cursor::new(VALID_RICH)).unwrap();
    let mut small_output = Cursor::new(Vec::new());
    write_package(&mut small_output, &small).unwrap();
    let small_archive_len = small_output.get_ref().len();
    let asset_id = small.manifest.assets[0].id.clone();
    let original_asset_len = small.assets[&asset_id].len();
    let original_manifest_len = serde_json::to_vec(&small.manifest).unwrap().len();
    assert!((small_archive_len as u64) < MAX_PACKAGE_BYTES);

    let mut exact_asset_len = original_asset_len + (MAX_PACKAGE_BYTES as usize - small_archive_len);
    let mut exact_package = read_package(Cursor::new(VALID_RICH)).unwrap();
    let mut exact_asset = exact_package.assets[&asset_id].clone();
    exact_asset.resize(exact_asset_len, 0);
    update_asset(&mut exact_package, &asset_id, exact_asset);
    let exact_manifest_len = serde_json::to_vec(&exact_package.manifest).unwrap().len();
    exact_asset_len -= exact_manifest_len - original_manifest_len;
    let mut exact_asset = exact_package.assets[&asset_id][..original_asset_len].to_vec();
    exact_asset.resize(exact_asset_len, 0);
    update_asset(&mut exact_package, &asset_id, exact_asset);
    let mut exact_output = Cursor::new(Vec::new());
    write_package(&mut exact_output, &exact_package).unwrap();
    assert_eq!(exact_output.get_ref().len() as u64, MAX_PACKAGE_BYTES);
    drop(exact_package);

    let exact_archive = exact_output.into_inner();
    let read_exact =
        read_package(Cursor::new(exact_archive)).expect("exact package limit is inclusive");
    assert_eq!(read_exact.assets[&asset_id].len(), exact_asset_len);
    drop(read_exact);

    let mut oversized_package = read_package(Cursor::new(VALID_RICH)).unwrap();
    let mut oversized_asset = oversized_package.assets[&asset_id].clone();
    oversized_asset.resize(exact_asset_len + 1, 0);
    update_asset(&mut oversized_package, &asset_id, oversized_asset);
    let mut oversized_output = Cursor::new(Vec::new());
    let error = write_package(&mut oversized_output, &oversized_package).unwrap_err();
    assert_eq!(error.kind, "budget_exceeded");

    let error = read_package(SizeOnlyReader {
        length: MAX_PACKAGE_BYTES + 1,
        position: 0,
    })
    .unwrap_err();
    assert_eq!(error.kind, "budget_exceeded");
}

#[test]
fn document_and_manifest_size_limits_accept_exact_and_reject_outside() {
    use cuenote_lib::note_format::{MAX_DOCUMENT_BYTES, MAX_MANIFEST_BYTES};

    let mut document_exact = plain_package();
    document_exact.document.title.clear();
    let empty_title_len = serde_json::to_vec(&document_exact.document).unwrap().len();
    document_exact.document.title = "x".repeat(MAX_DOCUMENT_BYTES as usize - empty_title_len);
    let document_bytes = serde_json::to_vec(&document_exact.document).unwrap();
    assert_eq!(document_bytes.len() as u64, MAX_DOCUMENT_BYTES);
    let mut output = Cursor::new(Vec::new());
    write_package(&mut output, &document_exact).unwrap();
    let round_trip = read_package(Cursor::new(output.into_inner())).unwrap();
    assert_eq!(
        round_trip.document.title.len(),
        document_exact.document.title.len()
    );

    document_exact.document.title.push('x');
    let error = write_package(&mut Cursor::new(Vec::new()), &document_exact).unwrap_err();
    assert_eq!(error.kind, "budget_exceeded");
    let document_outside = serde_json::to_vec(&document_exact.document).unwrap();
    assert_eq!(document_outside.len() as u64, MAX_DOCUMENT_BYTES + 1);
    let archive = archive_bytes(
        &document_outside,
        b"{\"schemaVersion\":1,\"assets\":[]}",
        &[],
    );
    assert_eq!(
        read_package(Cursor::new(archive)).unwrap_err().kind,
        "budget_exceeded"
    );

    let plain = plain_package();
    let plain_doc = serde_json::to_vec(&plain.document).unwrap();
    let manifest = b"{\"schemaVersion\":1,\"assets\":[]}";
    let mut manifest_exact = manifest.to_vec();
    manifest_exact.resize(MAX_MANIFEST_BYTES as usize, b' ');
    let archive = archive_bytes(&plain_doc, &manifest_exact, &[]);
    read_package(Cursor::new(archive)).expect("manifest exactly at its byte limit is accepted");
    manifest_exact.push(b' ');
    let archive = archive_bytes(&plain_doc, &manifest_exact, &[]);
    assert_eq!(
        read_package(Cursor::new(archive)).unwrap_err().kind,
        "budget_exceeded"
    );
}

#[test]
fn json_depth_limit_accepts_64_container_levels_and_rejects_the_next() {
    let mut exact = None;
    for quote_count in 0..=32 {
        let package = package_with_quote_depth(quote_count);
        let json = serde_json::to_value(&package.document).unwrap();
        if json_container_depth(&json, 0) == 63 {
            exact = Some((quote_count, package));
            break;
        }
    }
    let (quote_count, exact_package) =
        exact.expect("an AST shape reaches exactly 64 JSON containers");
    let mut output = Cursor::new(Vec::new());
    write_package(&mut output, &exact_package).expect("exact JSON depth is allowed");
    read_package(Cursor::new(output.into_inner())).expect("exact JSON depth round trips");

    let outside = package_with_quote_depth(quote_count + 1);
    let error = write_package(&mut Cursor::new(Vec::new()), &outside).unwrap_err();
    assert_eq!(error.kind, "invalid_document");
}

#[test]
fn asset_count_and_zip_entry_limits_are_inclusive() {
    use cuenote_lib::note_format::MAX_ASSETS;

    for count in [MAX_ASSETS - 1, MAX_ASSETS] {
        let package = image_count_package(count);
        let mut output = Cursor::new(Vec::new());
        write_package(&mut output, &package).unwrap();
        let round_trip = read_package(Cursor::new(output.into_inner())).unwrap();
        assert_eq!(round_trip.assets.len(), count);
    }
    let over = image_count_package(MAX_ASSETS + 1);
    assert_eq!(
        write_package(&mut Cursor::new(Vec::new()), &over)
            .unwrap_err()
            .kind,
        "resource_limit"
    );

    let package = image_count_package(MAX_ASSETS + 1);
    let document = serde_json::to_vec(&package.document).unwrap();
    let manifest = serde_json::to_vec(&package.manifest).unwrap();
    let assets = package
        .assets
        .iter()
        .map(|(id, bytes)| (format!("assets/{id}"), bytes.as_slice()))
        .collect::<Vec<_>>();
    let archive = archive_bytes(&document, &manifest, &assets);
    assert_eq!(
        read_package(Cursor::new(archive)).unwrap_err().kind,
        "budget_exceeded"
    );
}

#[test]
fn mind_map_node_limit_accepts_exact_and_rejects_outside() {
    use cuenote_lib::note_format::MAX_MAP_NODES;

    for count in [MAX_MAP_NODES - 1, MAX_MAP_NODES] {
        let package = package_with_map_nodes(count);
        let mut output = Cursor::new(Vec::new());
        write_package(&mut output, &package).unwrap();
        let round_trip = read_package(Cursor::new(output.into_inner())).unwrap();
        let Block::MindMap { nodes, .. } = &round_trip.document.sections.content[0] else {
            panic!("mind map block");
        };
        assert_eq!(nodes.len(), count);
    }
    let over = package_with_map_nodes(MAX_MAP_NODES + 1);
    assert_eq!(
        write_package(&mut Cursor::new(Vec::new()), &over)
            .unwrap_err()
            .kind,
        "resource_limit"
    );
    assert_eq!(
        read_package(Cursor::new(archive_from_package(&over)))
            .unwrap_err()
            .kind,
        "resource_limit"
    );
}

#[test]
fn image_pixel_limits_accept_exact_and_reject_outside() {
    let under = image_dimensions_package(&[(4_999, 5_000)]);
    let mut output = Cursor::new(Vec::new());
    write_package(&mut output, &under).expect("below 25M pixels");
    read_package(Cursor::new(output.into_inner())).expect("reader accepts below 25M pixels");
    let exact = image_dimensions_package(&[(5_000, 5_000)]);
    let mut output = Cursor::new(Vec::new());
    write_package(&mut output, &exact).expect("exactly 25M pixels");
    read_package(Cursor::new(output.into_inner())).expect("reader accepts exactly 25M pixels");
    let over = image_dimensions_package(&[(5_001, 5_000)]);
    assert_eq!(
        write_package(&mut Cursor::new(Vec::new()), &over)
            .unwrap_err()
            .kind,
        "resource_limit"
    );
    assert_eq!(
        read_package(Cursor::new(archive_from_package(&over)))
            .unwrap_err()
            .kind,
        "resource_limit"
    );

    let aggregate_exact = image_dimensions_package(&[(5_000, 5_000); 4]);
    let mut output = Cursor::new(Vec::new());
    write_package(&mut output, &aggregate_exact).expect("exactly 100M pixels");
    read_package(Cursor::new(output.into_inner())).expect("reader accepts exactly 100M pixels");
    let aggregate_over = image_dimensions_package(
        &[(5_000, 5_000); 4]
            .into_iter()
            .chain([(1, 1)])
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        write_package(&mut Cursor::new(Vec::new()), &aggregate_over)
            .unwrap_err()
            .kind,
        "resource_limit"
    );
    assert_eq!(
        read_package(Cursor::new(archive_from_package(&aggregate_over)))
            .unwrap_err()
            .kind,
        "resource_limit"
    );
}

#[test]
fn q51_ordinary_and_stress_packages_round_trip_with_valid_png_capacity_chunks() {
    const MIB: usize = 1024 * 1024;
    let cases = [
        (5_000, 5, MIB, 1_024, 50, 5 * MIB),
        (50_000, 10, 5 * MIB, 2_048, 500, 50 * MIB),
    ];
    for (unicode_chars, image_count, bytes_per_image, side, map_nodes, expected_assets) in cases {
        let package = q51_sample(unicode_chars, image_count, bytes_per_image, side, map_nodes);
        assert_eq!(
            package.assets.values().map(Vec::len).sum::<usize>(),
            expected_assets,
            "deterministic capacity fixture asset bytes"
        );

        let mut archive = Cursor::new(Vec::new());
        write_package(&mut archive, &package).expect("Q51 capacity sample is accepted by writer");
        let archive = archive.into_inner();
        drop(package);

        let reopened =
            read_package(Cursor::new(archive)).expect("Q51 capacity sample is accepted by reader");
        let Block::Paragraph { children } = &reopened.document.sections.content[0] else {
            panic!("plain text paragraph remains the first content block");
        };
        let Inline::Text { text, marks } = &children[0] else {
            panic!("plain text remains intact");
        };
        assert!(marks.is_empty());
        assert_eq!(text.chars().count(), unicode_chars);
        assert_eq!(reopened.assets.len(), image_count);
        assert_eq!(
            reopened.assets.values().map(Vec::len).sum::<usize>(),
            expected_assets
        );
        let Block::MindMap { nodes, .. } = reopened.document.sections.content.last().unwrap()
        else {
            panic!("mind map remains the last content block");
        };
        assert_eq!(nodes.len(), map_nodes);
    }
}

fn q51_sample(
    unicode_chars: usize,
    image_count: usize,
    bytes_per_image: usize,
    image_side: u32,
    map_node_count: usize,
) -> NotePackage {
    let mut package = NotePackage::new_plain(
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
        PlainText {
            title: "Q51 deterministic capacity fixture".into(),
            cue: String::new(),
            content: "界".repeat(unicode_chars),
            summary: String::new(),
        },
    );
    let png = png_with_ancillary_capacity(image_side, image_side, bytes_per_image);
    let hash = format!("{:x}", sha2::Sha256::digest(&png));
    for index in 0..image_count {
        let id = test_uuid(index);
        package.document.sections.content.push(Block::Image {
            asset_id: id.clone(),
            display_width_px: image_side,
        });
        package.manifest.assets.push(AssetManifestEntry {
            id: id.clone(),
            media_type: MediaType::Png,
            byte_length: png.len() as u64,
            sha256: hash.clone(),
            width: image_side,
            height: image_side,
        });
        package.assets.insert(id, png.clone());
    }

    let root_id = test_uuid(20_000);
    let nodes = std::iter::once(MindMapNode {
        id: root_id.clone(),
        text: "root".into(),
        children: (0..map_node_count.saturating_sub(1))
            .map(|index| test_uuid(20_001 + index))
            .collect(),
    })
    .chain(
        (0..map_node_count.saturating_sub(1)).map(|index| MindMapNode {
            id: test_uuid(20_001 + index),
            text: format!("node {index}"),
            children: Vec::new(),
        }),
    )
    .collect();
    package.document.sections.content.push(Block::MindMap {
        map_id: test_uuid(30_000),
        root_id,
        nodes,
    });
    package
}

fn png_with_ancillary_capacity(width: u32, height: u32, target_size: usize) -> Vec<u8> {
    let original = encode_solid_png(width, height);
    let padding_length = target_size
        .checked_sub(original.len() + 12)
        .expect("capacity target leaves room for PNG chunk framing");
    let iend_type = original
        .windows(4)
        .rposition(|bytes| bytes == b"IEND")
        .expect("encoded PNG contains IEND");
    let chunk_offset = iend_type - 4;

    let mut chunk = Vec::with_capacity(padding_length + 12);
    chunk.extend_from_slice(&(padding_length as u32).to_be_bytes());
    chunk.extend_from_slice(b"caPa");
    chunk.resize(8 + padding_length, 0);
    let crc = png_crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());

    let mut png = Vec::with_capacity(target_size);
    png.extend_from_slice(&original[..chunk_offset]);
    png.extend_from_slice(&chunk);
    png.extend_from_slice(&original[chunk_offset..]);
    assert_eq!(png.len(), target_size);
    png
}

fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

#[test]
fn expanded_byte_limit_accepts_exact_and_rejects_real_overflow_and_forged_size() {
    use cuenote_lib::note_format::MAX_EXPANDED_BYTES;

    read_package(Cursor::new(VALID_RICH)).expect("fixed fixture is inside expanded-byte limit");
    let exact_archive = expanded_archive(MAX_EXPANDED_BYTES);
    let round_trip = read_package(Cursor::new(exact_archive.clone()))
        .expect("expanded package exactly at 512 MiB is accepted");
    assert_eq!(
        round_trip.assets.values().next().unwrap().len() as u64
            + serde_json::to_vec(&round_trip.document).unwrap().len() as u64
            + serde_json::to_vec(&round_trip.manifest).unwrap().len() as u64,
        MAX_EXPANDED_BYTES
    );
    drop(round_trip);

    let outside_archive = expanded_archive(MAX_EXPANDED_BYTES + 1);
    let mut forged_archive = outside_archive.clone();
    assert_eq!(
        read_package(Cursor::new(outside_archive)).unwrap_err().kind,
        "budget_exceeded"
    );

    let actual_size = central_entry_size(
        &forged_archive,
        "assets/11111111-1111-4111-8111-111111111111",
    );
    set_central_entry_size_and_local_size(
        &mut forged_archive,
        "assets/11111111-1111-4111-8111-111111111111",
        actual_size - 1,
    );
    assert!(
        read_package(Cursor::new(forged_archive)).is_err(),
        "reader rejects a ZIP whose declarations understate actual deflate output"
    );
}

fn archive_from_package(package: &NotePackage) -> Vec<u8> {
    let document = serde_json::to_vec(&package.document).expect("document JSON");
    let manifest = serde_json::to_vec(&package.manifest).expect("manifest JSON");
    let assets = package
        .assets
        .iter()
        .map(|(id, bytes)| (format!("assets/{id}"), bytes.as_slice()))
        .collect::<Vec<_>>();
    archive_bytes(&document, &manifest, &assets)
}

fn update_asset(package: &mut NotePackage, id: &str, bytes: Vec<u8>) {
    let entry = package
        .manifest
        .assets
        .iter_mut()
        .find(|entry| entry.id == id)
        .expect("manifest entry");
    entry.byte_length = bytes.len() as u64;
    entry.sha256 = format!("{:x}", sha2::Sha256::digest(&bytes));
    package.assets.insert(id.to_owned(), bytes);
}

fn plain_package() -> NotePackage {
    NotePackage::new_plain(
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
        PlainText {
            title: "Title".into(),
            cue: String::new(),
            content: String::new(),
            summary: String::new(),
        },
    )
}

fn package_with_quote_depth(depth: usize) -> NotePackage {
    let mut package = plain_package();
    let mut block = Block::Paragraph {
        children: vec![Inline::Text {
            text: "deep".into(),
            marks: vec![Mark::Bold],
        }],
    };
    for _ in 0..depth {
        block = Block::Blockquote {
            blocks: vec![block],
        };
    }
    package.document.sections.content.push(block);
    package
}

fn json_container_depth(value: &serde_json::Value, depth: usize) -> usize {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| json_container_depth(item, depth + 1))
            .max()
            .unwrap_or(depth)
            .max(depth),
        serde_json::Value::Object(object) => object
            .values()
            .map(|item| json_container_depth(item, depth + 1))
            .max()
            .unwrap_or(depth)
            .max(depth),
        _ => depth.saturating_sub(1),
    }
}

fn image_count_package(count: usize) -> NotePackage {
    let mut package = plain_package();
    let png = include_bytes!("fixtures/pixel.png").to_vec();
    let hash = format!("{:x}", sha2::Sha256::digest(&png));
    for index in 0..count {
        let id = test_uuid(index);
        package.document.sections.content.push(Block::Image {
            asset_id: id.clone(),
            display_width_px: 100,
        });
        package.manifest.assets.push(AssetManifestEntry {
            id: id.clone(),
            media_type: MediaType::Png,
            byte_length: png.len() as u64,
            sha256: hash.clone(),
            width: 1,
            height: 1,
        });
        package.assets.insert(id, png.clone());
    }
    package
}

fn package_with_map_nodes(count: usize) -> NotePackage {
    let mut package = plain_package();
    let root_id = test_uuid(0);
    let mut nodes = Vec::with_capacity(count);
    nodes.push(MindMapNode {
        id: root_id.clone(),
        text: "root".into(),
        children: (1..count).map(test_uuid).collect(),
    });
    for index in 1..count {
        nodes.push(MindMapNode {
            id: test_uuid(index),
            text: format!("node {index}"),
            children: vec![],
        });
    }
    package.document.sections.content.push(Block::MindMap {
        map_id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc".into(),
        root_id,
        nodes,
    });
    package
}

fn image_dimensions_package(dimensions: &[(u32, u32)]) -> NotePackage {
    let mut package = plain_package();
    let mut encoded = HashMap::<(u32, u32), Vec<u8>>::new();
    for (index, &(width, height)) in dimensions.iter().enumerate() {
        let bytes = encoded
            .entry((width, height))
            .or_insert_with(|| encode_solid_png(width, height));
        let id = test_uuid(index);
        package.document.sections.content.push(Block::Image {
            asset_id: id.clone(),
            display_width_px: width,
        });
        package.manifest.assets.push(AssetManifestEntry {
            id: id.clone(),
            media_type: MediaType::Png,
            byte_length: bytes.len() as u64,
            sha256: format!("{:x}", sha2::Sha256::digest(bytes.as_slice())),
            width,
            height,
        });
        package.assets.insert(id, bytes.clone());
    }
    package
}

fn encode_solid_png(width: u32, height: u32) -> Vec<u8> {
    let pixel_count = width as usize * height as usize;
    let image = image::ImageBuffer::<image::Luma<u8>, Vec<u8>>::from_raw(
        width,
        height,
        vec![127; pixel_count],
    )
    .expect("image buffer dimensions");
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(image)
        .write_to(&mut output, image::ImageFormat::Png)
        .expect("encode fixture image");
    output.into_inner()
}

fn archive_bytes(document: &[u8], manifest: &[u8], assets: &[(String, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let json_options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let asset_options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("document.json", json_options).unwrap();
    zip.write_all(document).unwrap();
    zip.start_file("assets/manifest.json", asset_options)
        .unwrap();
    zip.write_all(manifest).unwrap();
    for (name, bytes) in assets {
        zip.start_file(name, asset_options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn expanded_archive(expanded_target: u64) -> Vec<u8> {
    let mut package = read_package(Cursor::new(VALID_RICH)).unwrap();
    let document = serde_json::to_vec(&package.document).unwrap();
    let asset_id = package.manifest.assets[0].id.clone();
    let png = package.assets[&asset_id].clone();
    let mut asset_len = expanded_target
        - document.len() as u64
        - serde_json::to_vec(&package.manifest).unwrap().len() as u64;
    for _ in 0..3 {
        package.manifest.assets[0].byte_length = asset_len;
        let manifest_len = serde_json::to_vec(&package.manifest).unwrap().len() as u64;
        let corrected = expanded_target - document.len() as u64 - manifest_len;
        if corrected == asset_len {
            break;
        }
        asset_len = corrected;
    }
    let trailing_zeros = asset_len - png.len() as u64;
    package.manifest.assets[0].byte_length = asset_len;
    package.manifest.assets[0].sha256 = hash_with_zero_suffix(&png, trailing_zeros);
    let manifest = serde_json::to_vec(&package.manifest).unwrap();
    assert_eq!(
        document.len() as u64 + manifest.len() as u64 + asset_len,
        expanded_target
    );

    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "document.json",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(&document).unwrap();
    zip.start_file(
        "assets/manifest.json",
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(&manifest).unwrap();
    zip.start_file(
        format!("assets/{asset_id}"),
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(&png).unwrap();
    let zeroes = [0u8; 64 * 1024];
    let mut remaining = trailing_zeros;
    while remaining > 0 {
        let count = remaining.min(zeroes.len() as u64) as usize;
        zip.write_all(&zeroes[..count]).unwrap();
        remaining -= count as u64;
    }
    zip.finish().unwrap().into_inner()
}

fn hash_with_zero_suffix(prefix: &[u8], zero_count: u64) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(prefix);
    let zeroes = [0u8; 64 * 1024];
    let mut remaining = zero_count;
    while remaining > 0 {
        let count = remaining.min(zeroes.len() as u64) as usize;
        hasher.update(&zeroes[..count]);
        remaining -= count as u64;
    }
    format!("{:x}", hasher.finalize())
}

fn central_entry_size(archive: &[u8], name: &str) -> u32 {
    central_entry_offset(archive, name)
        .map(|offset| u32::from_le_bytes(archive[offset + 24..offset + 28].try_into().unwrap()))
        .expect("central entry exists")
}

fn set_central_entry_size_and_local_size(archive: &mut [u8], name: &str, size: u32) {
    let central_offset = central_entry_offset(archive, name).expect("central entry exists");
    let local_offset = u32::from_le_bytes(
        archive[central_offset + 42..central_offset + 46]
            .try_into()
            .unwrap(),
    ) as usize;
    archive[central_offset + 24..central_offset + 28].copy_from_slice(&size.to_le_bytes());
    archive[local_offset + 22..local_offset + 26].copy_from_slice(&size.to_le_bytes());
}

fn central_entry_offset(archive: &[u8], name: &str) -> Option<usize> {
    let name_bytes = name.as_bytes();
    for offset in 0..archive.len().saturating_sub(46) {
        if archive[offset..offset + 4] != [0x50, 0x4b, 0x01, 0x02] {
            continue;
        }
        let name_len =
            u16::from_le_bytes(archive[offset + 28..offset + 30].try_into().ok()?) as usize;
        if name_len == name_bytes.len()
            && archive.get(offset + 46..offset + 46 + name_len)? == name_bytes
        {
            return Some(offset);
        }
    }
    None
}

fn test_uuid(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012x}")
}

struct SizeOnlyReader {
    length: u64,
    position: u64,
}

impl std::io::Read for SizeOnlyReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.length.saturating_sub(self.position);
        let count = remaining.min(buffer.len() as u64) as usize;
        buffer[..count].fill(0);
        self.position += count as u64;
        Ok(count)
    }
}

impl std::io::Seek for SizeOnlyReader {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        let next = match position {
            std::io::SeekFrom::Start(position) => Some(position),
            std::io::SeekFrom::End(delta) => self.length.checked_add_signed(delta),
            std::io::SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        }
        .ok_or_else(|| std::io::Error::other("invalid test seek"))?;
        self.position = next;
        Ok(next)
    }
}

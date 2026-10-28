//! A caller's metadata limit refuses oversized sections before allocating.

#![cfg(all(feature = "read", feature = "write", feature = "zstd"))]

use stipplefs::error::Error;
use stipplefs::format::{FLAG_COMPRESSED_METADATA, SectionKind};
use stipplefs::read::Image;
use stipplefs::write::{Builder, Node, Options, Timestamps};

/// An image with compressed metadata, whose name table decodes to far more
/// than it stores.
fn image() -> Vec<u8> {
    let mut b = Builder::new(Options {
        timestamps: Timestamps::Clamp,
        ..Options::default()
    })
    .unwrap();
    for i in 0..200 {
        b.insert(
            format!("a-rather-long-and-repetitive-file-name-{i:04}").as_bytes(),
            Node::File {
                content: vec![i as u8; 100],
                mode: 0o644,
                uid: 0,
                gid: 0,
                link_group: None,
            },
        )
        .unwrap();
    }
    b.build().unwrap()
}

fn malformed<T>(result: Result<T, Error>) -> &'static str {
    match result {
        Err(Error::Malformed { detail }) => detail,
        Err(other) => panic!("expected a malformed image, got {other}"),
        Ok(_) => panic!("an oversized section was accepted"),
    }
}

/// The names section's stored content length and the decoded length its
/// frame declares.
fn names_lengths(image: &[u8], img: &Image<'_>) -> (u64, u64) {
    let section = img.superblock().section(SectionKind::Names);
    let at = section.offset as usize;
    let decoded = u64::from_le_bytes(image[at..at + 8].try_into().unwrap());
    (section.size - 8, decoded)
}

#[test]
fn the_default_limit_accepts_ordinary_images() {
    let image = image();
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    assert_ne!(img.superblock().flags & FLAG_COMPRESSED_METADATA, 0);
    assert!(img.reader().is_ok());
}

#[test]
fn a_declared_decoded_length_over_the_limit_is_refused() {
    let image = image();
    let mut img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let (stored, decoded) = names_lengths(&image, &img);
    assert!(
        decoded > stored,
        "names must compress: {stored} -> {decoded}"
    );
    img.set_metadata_limit(stored);
    let detail = malformed(img.section_data(SectionKind::Names));
    assert!(detail.contains("declares more"), "{detail}");
    assert!(img.reader().is_err());
}

#[test]
fn a_stored_section_over_the_limit_is_refused() {
    let image = image();
    let mut img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let (stored, _) = names_lengths(&image, &img);
    img.set_metadata_limit(stored - 1);
    let detail = malformed(img.section_data(SectionKind::Names));
    assert!(detail.contains("larger than"), "{detail}");
}

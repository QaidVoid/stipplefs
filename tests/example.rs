//! The example an embedder would copy, kept in the test suite so that it
//! cannot regress without a test failing.

#![cfg(all(feature = "read", feature = "write"))]

use stipplefs::read::Image;
use stipplefs::write::{Builder, Node, Options};

/// Build an image, embed it in a larger file, then read one file back out.
///
/// This is the whole embedder path. If it needs more than a handful of lines,
/// the API has a problem.
#[test]
fn reading_an_embedded_image_takes_a_handful_of_lines() {
    // --- something to read -------------------------------------------------
    let mut builder = Builder::new(Options::default()).unwrap();
    builder
        .insert(
            b"docs/hello.txt",
            Node::File {
                content: b"hello from an embedded image\n".to_vec(),
                mode: 0o644,
                uid: 0,
                gid: 0,
                link_group: None,
            },
        )
        .unwrap();
    let image = builder.build().unwrap();

    // --- pretend it lives inside a larger file -----------------------------
    let mut carrier = vec![0xEEu8; 4321];
    let offset = carrier.len() as u64;
    let length = image.len() as u64;
    carrier.extend_from_slice(&image);
    carrier.extend_from_slice(&[0xDDu8; 99]);

    // --- the part an embedder writes ---------------------------------------
    let image = Image::new(carrier, offset, length).unwrap();
    let reader = image.reader().unwrap();
    let ino = reader.resolve(b"docs/hello.txt").unwrap();
    let bytes = reader.read_all(ino).unwrap();
    // -----------------------------------------------------------------------

    assert_eq!(bytes, b"hello from an embedded image\n");
}

/// Walking a directory should not require arithmetic on inode fields.
#[test]
fn listing_a_directory_is_a_plain_iterator() {
    let mut builder = Builder::new(Options::default()).unwrap();
    for name in ["b.txt", "a.txt", "c.txt"] {
        builder
            .insert(
                format!("dir/{name}").as_bytes(),
                Node::File {
                    content: name.as_bytes().to_vec(),
                    mode: 0o644,
                    uid: 0,
                    gid: 0,
                    link_group: None,
                },
            )
            .unwrap();
    }
    let image = builder.build().unwrap();
    let image = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let reader = image.reader().unwrap();

    let dir = reader.resolve(b"dir").unwrap();
    let names: Vec<String> = reader
        .entries(dir)
        .map(|(name, _)| String::from_utf8_lossy(name).into_owned())
        .collect();

    assert_eq!(names, ["a.txt", "b.txt", "c.txt"]);
}

/// A path that is not present resolves to nothing rather than an error.
#[test]
fn a_missing_path_resolves_to_none() {
    let mut builder = Builder::new(Options::default()).unwrap();
    builder
        .insert(
            b"present",
            Node::File {
                content: b"x".to_vec(),
                mode: 0o644,
                uid: 0,
                gid: 0,
                link_group: None,
            },
        )
        .unwrap();
    let image = builder.build().unwrap();
    let image = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let reader = image.reader().unwrap();

    assert!(reader.resolve(b"present").is_some());
    assert!(reader.resolve(b"absent").is_none());
    assert!(reader.resolve(b"present/deeper").is_none());
    assert_eq!(reader.resolve(b"").unwrap(), reader.root());
    assert_eq!(reader.resolve(b"/").unwrap(), reader.root());
}

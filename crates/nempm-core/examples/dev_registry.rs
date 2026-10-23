//! A local stand-in for the registry and GitHub Releases, for trying the app
//! end to end before anything is published.
//!
//!     cargo run -p nempm-core --example dev_registry [-- --port 8787]
//!
//! It builds a sample Core tar, two tool zips that each come with a
//! NEMESIS.data asset, and one zip that carries its NEMESIS.data inside,
//! writes an index.json that points at them, and serves everything from
//! http://127.0.0.1:<port>/. Then
//! start the app with NEMPM_INDEX_URL=http://127.0.0.1:<port>/index.json, or
//! paste that URL into Settings → Index URL. Supports Range requests, so
//! resumed downloads work too.

use std::collections::HashMap;
use std::io::{Cursor, Write};

use sha2::{Digest, Sha256};

fn sample_tar(size: usize) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "nemesis/core.bin", &data[..]).unwrap();
    builder.into_inner().unwrap()
}

fn sample_zip(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in files {
        w.start_file(*name, opts).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap().into_inner()
}

struct Sample {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    format: &'static str,
    /// The release asset's file name (the Core tar keeps its own on the drive).
    file: &'static str,
    body: Vec<u8>,
    /// This package's NEMESIS.data, published in the same release. Without
    /// one, a tool's zip carries it inside.
    data: Option<Vec<u8>>,
    extract: bool,
}

fn main() {
    let port: u16 = std::env::args()
        .skip_while(|a| a != "--port")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);
    let base = format!("http://127.0.0.1:{port}");

    let assets = [
        Sample {
            id: "nemesis-core",
            name: "Nemesis Core",
            description: "Put this on a stick by itself to set up a new device. Written to the drive root.",
            format: "tar",
            file: "ota_package.tar",
            body: sample_tar(24 * 1024 * 1024),
            data: None,
            extract: false,
        },
        Sample {
            id: "example-tool",
            name: "Example Tool",
            description: "A small tool. Writes NEMESIS.data and packages/example-tool.zip.",
            format: "zip",
            file: "example-tool.zip",
            body: sample_zip(&[
                ("README.txt", b"Example tool\n".to_vec()),
                ("bin/run.sh", b"#!/bin/sh\necho hello\n".to_vec()),
            ]),
            data: Some(b"NEMESIS.data for example-tool 1.0.0\n".to_vec()),
            extract: false,
        },
        Sample {
            id: "my.tool",
            name: "My Tool",
            description: "Laid out like a real Nemesis package; unpacked by default into the drive root.",
            format: "zip",
            file: "my.tool.zip",
            body: sample_zip(&[
                ("packages/my.tool.inner.zip", vec![7u8; 2 * 1024 * 1024]),
                ("mytool/install.sh", b"#!/bin/sh\necho installing\n".to_vec()),
                ("mytool/config.json", br#"{"enabled":true}"#.to_vec()),
            ]),
            data: Some(b"NEMESIS.data for my.tool 1.0.0\n".to_vec()),
            extract: true,
        },
        Sample {
            id: "my.kit",
            name: "My Kit",
            description: "Just a zip with the whole stick layout, NEMESIS.data included. Always unpacked.",
            format: "zip",
            file: "my-kit-install-usb.zip",
            body: sample_zip(&[
                ("NEMESIS.data", b"NEMESIS.data for my.kit 1.0.0\n".to_vec()),
                ("packages/my.kit.inner.zip", vec![9u8; 1024 * 1024]),
                ("mykit/install.sh", b"#!/bin/sh\necho installing\n".to_vec()),
            ]),
            data: None,
            extract: true,
        },
    ];

    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    let mut packages = Vec::new();
    for s in assets {
        let release = format!("/releases/{}/v1.0.0", s.id);
        let data = s.data.as_ref().map(|d| {
            serde_json::json!({
                "url": format!("{base}{release}/NEMESIS.data"),
                "sha256": hex::encode(Sha256::digest(d)),
                "size": d.len(),
            })
        });
        let mut pkg = serde_json::json!({
            "id": s.id,
            "name": s.name,
            "description": s.description,
            "version": "1.0.0",
            "format": s.format,
            "core": s.format == "tar",
            "url": format!("{base}{release}/{}", s.file),
            "sha256": hex::encode(Sha256::digest(&s.body)),
            "size": s.body.len(),
            "extract": s.extract,
            "slot": if s.format == "tar" { "core" } else { "packages" },
            "profiles": ["nemesis"],
            "requires": if s.format == "tar" { vec![] } else { vec!["nemesis-core"] },
        });
        if let Some(data) = data {
            pkg["data"] = data;
        }
        packages.push(pkg);
        files.insert(format!("{release}/{}", s.file), s.body);
        if let Some(d) = s.data {
            files.insert(format!("{release}/NEMESIS.data"), d);
        }
    }
    let profile: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/nemesis.json")).unwrap();
    let index = serde_json::json!({
        "schemaVersion": 1,
        "generatedAt": nempm_core::manifest::now(),
        "packages": packages,
        "profiles": [profile],
    });
    files.insert("/index.json".into(), serde_json::to_vec_pretty(&index).unwrap());

    let server = tiny_http::Server::http(("127.0.0.1", port)).expect("port in use? pass --port <n>");
    println!("Dev registry at {base}/index.json");
    println!("Run the app with NEMPM_INDEX_URL={base}/index.json, or set it in Settings.");
    for req in server.incoming_requests() {
        let path = req.url().split('?').next().unwrap_or("").to_string();
        let Some(body) = files.get(&path) else {
            println!("404 {path}");
            let _ = req.respond(tiny_http::Response::empty(404));
            continue;
        };
        let start = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .and_then(|h| h.value.as_str().strip_prefix("bytes=").map(str::to_string))
            .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok())
            .filter(|&s| s > 0 && s < body.len());
        println!(
            "200 {path}{}",
            start.map(|s| format!(" from byte {s}")).unwrap_or_default()
        );
        let resp = match start {
            Some(s) => tiny_http::Response::from_data(body[s..].to_vec())
                .with_status_code(206)
                .with_header(
                    tiny_http::Header::from_bytes(
                        "Content-Range",
                        format!("bytes {s}-{}/{}", body.len() - 1, body.len()),
                    )
                    .unwrap(),
                ),
            None => tiny_http::Response::from_data(body.clone()),
        };
        let _ = req.respond(resp);
    }
}

//! A local stand-in for the registry and GitHub Releases, for trying the app
//! end to end before anything is published.
//!
//!     cargo run -p pkgmgr-core --example dev_registry [-- --port 8787]
//!
//! It builds a sample core tar and two tool zips, writes an index.json that
//! points at them, and serves everything from http://127.0.0.1:<port>/. Then
//! start the app with PKGMGR_INDEX_URL=http://127.0.0.1:<port>/index.json, or
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
    body: Vec<u8>,
    deps: &'static [&'static str],
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
            description: "The core data, written to the drive root as NEMESIS.data.",
            format: "tar",
            body: sample_tar(24 * 1024 * 1024),
            deps: &[],
            extract: false,
        },
        Sample {
            id: "example-tool",
            name: "Example Tool",
            description: "A small tool package, written to packages/example-tool.zip.",
            format: "zip",
            body: sample_zip(&[
                (
                    "README.txt",
                    b"Example tool
"
                    .to_vec(),
                ),
                (
                    "bin/run.sh",
                    b"#!/bin/sh
echo hello
"
                    .to_vec(),
                ),
            ]),
            deps: &["nemesis-core"],
            extract: false,
        },
        Sample {
            id: "unpacked-tool",
            name: "Unpacked Tool",
            description: "Unpacked by default into packages/unpacked-tool/.",
            format: "zip",
            body: sample_zip(&[
                ("tool/config.json", br#"{"enabled":true}"#.to_vec()),
                ("tool/data.bin", vec![7u8; 2 * 1024 * 1024]),
            ]),
            deps: &["nemesis-core"],
            extract: true,
        },
    ];

    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    let mut packages = Vec::new();
    for s in assets {
        let file = format!("{}.{}", s.id, s.format);
        packages.push(serde_json::json!({
            "id": s.id,
            "name": s.name,
            "description": s.description,
            "version": "1.0.0",
            "format": s.format,
            "core": s.format == "tar",
            "url": format!("{base}/assets/{file}"),
            "sha256": hex::encode(Sha256::digest(&s.body)),
            "size": s.body.len(),
            "extract": s.extract,
            "slot": if s.format == "tar" { "core" } else { "packages" },
            "profiles": ["nemesis"],
            "deps": s.deps,
        }));
        files.insert(format!("/assets/{file}"), s.body);
    }
    let profile: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/nemesis.json")).unwrap();
    let index = serde_json::json!({
        "schemaVersion": 1,
        "generatedAt": pkgmgr_core::manifest::now(),
        "packages": packages,
        "profiles": [profile],
    });
    files.insert("/index.json".into(), serde_json::to_vec_pretty(&index).unwrap());

    let server = tiny_http::Server::http(("127.0.0.1", port)).expect("port in use? pass --port <n>");
    println!("Dev registry at {base}/index.json");
    println!("Run the app with PKGMGR_INDEX_URL={base}/index.json, or set it in Settings.");
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

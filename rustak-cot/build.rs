//! Compiles `proto/tak_protocol_v1.proto` into Rust structs without requiring
//! a `protoc` binary anywhere on the build machine (including cross-compile
//! containers).
//!
//! [`protox`] is a pure-Rust protobuf compiler: it parses the `.proto` file
//! into a `FileDescriptorSet` itself, so [`prost_build`] never has to shell
//! out to `protoc`.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("proto");
    println!("cargo:rerun-if-changed={}", proto_dir.display());

    let descriptors = protox::compile([proto_dir.join("tak_protocol_v1.proto")], [&proto_dir])?;

    prost_build::Config::new()
        .bytes(["."]) // detail/extension payloads decode as `bytes::Bytes`, zero-copy
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .compile_fds(descriptors)?; // writes $OUT_DIR/rustak.cot.v1.rs

    Ok(())
}

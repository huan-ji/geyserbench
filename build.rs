use std::{env, path::PathBuf};

const PROTOC_ENVAR: &str = "PROTOC";
#[inline]
pub fn protoc() -> String {
    protobuf_src::protoc()
        .to_str()
        .expect("Vendored Protoc should always be available")
        .to_string()
}

#[inline]
pub fn mpath(path: &str) -> String {
    path.to_string()
}

fn main() -> anyhow::Result<()> {
    if env::var(PROTOC_ENVAR).is_err() {
        println!("protoc not found in PATH, attempting to fix");
        env::set_var(PROTOC_ENVAR, protoc());
    }

    let proto_files = [
        mpath("proto/arpc.proto"),
        mpath("proto/events.proto"),
        mpath("proto/publisher.proto"),
        mpath("proto/shredstream.proto"),
        mpath("proto/shreder.proto"),
        mpath("proto/jetstream.proto"),
    ];

    let out_dir = env::var("OUT_DIR").map(PathBuf::from)?;

    tonic_build::configure()
        .file_descriptor_set_path(out_dir.join("proto_descriptors.bin"))
        .compile_protos(&proto_files, &[mpath("proto")])?;

    Ok(())
}

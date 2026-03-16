use std::path::PathBuf;

fn main() {
    let proto_dir = PathBuf::from("../protobufs");
    if !proto_dir.join("meshtastic/mesh.proto").exists() {
        // Submodule not checked out — skip generation.
        // The hand-written types in src/proto/ will be used instead.
        println!("cargo:warning=protobufs submodule not found, skipping code generation");
        return;
    }

    let proto_files: Vec<PathBuf> = std::fs::read_dir(proto_dir.join("meshtastic"))
        .expect("read protobufs/meshtastic/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "proto"))
        .collect();

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_protos(&proto_files, &[proto_dir])
        .expect("prost-build failed");

    println!("cargo:rerun-if-changed=../protobufs/meshtastic/");
}

//! Generate messages from the shared protobuf contract.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "../../../proto/bpfman.proto";
    println!("cargo:rerun-if-changed={proto}");
    println!("cargo:rerun-if-changed=../../../proto/csi.proto");
    prost_build::Config::new()
        .btree_map(["."])
        .disable_comments([".csi"])
        .compile_protos(&[proto, "../../../proto/csi.proto"], &["../../../proto"])?;
    Ok(())
}

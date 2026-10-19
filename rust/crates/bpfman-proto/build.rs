//! Generate messages from the shared protobuf contract.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "../../../proto/bpfman.proto";
    println!("cargo:rerun-if-changed={proto}");
    prost_build::Config::new()
        .btree_map(["."])
        .compile_protos(&[proto], &["../../../proto"])?;
    Ok(())
}

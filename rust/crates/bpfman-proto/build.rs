//! Generate CSI messages and the optional bpfman compatibility protocol.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut protos = vec!["../../../proto/csi.proto"];
    if cfg!(feature = "management-api") {
        protos.push("../../../proto/bpfman.proto");
    }
    for proto in &protos {
        println!("cargo:rerun-if-changed={proto}");
    }
    prost_build::Config::new()
        .btree_map(["."])
        .disable_comments([".csi"])
        .compile_protos(&protos, &["../../../proto"])?;
    Ok(())
}

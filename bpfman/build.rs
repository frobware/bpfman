// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

use std::{
    env,
    path::PathBuf,
    process::Command,
};
use pkg_config::Library;

fn main() {
    buildinfo::generate_version_info();

    // Build dispatcher bytecode for hermetic builds - fail fast if this fails
    build_dispatchers_direct().expect("Failed to build dispatcher bytecode - required for hermetic builds");
}

fn build_dispatchers_direct() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")?;
    let workspace_root = PathBuf::from(&manifest_dir).parent().unwrap().to_path_buf();

    // Tell cargo to recompile if dispatcher source files change
    let bpf_src_dir = workspace_root.join("bpf");
    println!("cargo:rerun-if-changed={}", bpf_src_dir.join("xdp_dispatcher_v2.bpf.c").display());
    println!("cargo:rerun-if-changed={}", bpf_src_dir.join("tc_dispatcher.bpf.c").display());

    // Find libbpf - required for building dispatchers
    let libbpf_location = find_libbpf().map_err(|e| format!("libbpf not found - required for dispatcher compilation: {e}"))?;

    // Create output directory
    let output_dir = workspace_root.join(".output");
    std::fs::create_dir_all(&output_dir)?;

    // Check if any dispatcher needs rebuilding
    let xdp_needs_build = needs_rebuild(&workspace_root, "xdp_dispatcher_v2.bpf.c", "xdp_dispatcher_v2.bpf.o");
    let tc_needs_build = needs_rebuild(&workspace_root, "tc_dispatcher.bpf.c", "tc_dispatcher.bpf.o");

    if xdp_needs_build || tc_needs_build {
        println!("cargo:warning=Building dispatcher bytecode");
    }

    // Build dispatchers only if needed
    if xdp_needs_build {
        build_dispatcher_direct(&workspace_root, &libbpf_location, "xdp_dispatcher_v2.bpf.c", "xdp_dispatcher_v2.bpf.o")?;
    }
    if tc_needs_build {
        build_dispatcher_direct(&workspace_root, &libbpf_location, "tc_dispatcher.bpf.c", "tc_dispatcher.bpf.o")?;
    }

    Ok(())
}

fn needs_rebuild(workspace_root: &PathBuf, src_name: &str, out_name: &str) -> bool {
    let src_path = workspace_root.join("bpf").join(src_name);
    let out_path = workspace_root.join(".output").join(out_name);

    // If output doesn't exist, rebuild is needed
    if !out_path.exists() {
        return true;
    }

    // If we can't get metadata, rebuild to be safe
    let Ok(src_meta) = src_path.metadata() else { return true; };
    let Ok(out_meta) = out_path.metadata() else { return true; };

    // If we can't get timestamps, rebuild to be safe
    let Ok(src_time) = src_meta.modified() else { return true; };
    let Ok(out_time) = out_meta.modified() else { return true; };

    // Rebuild if source is newer than output
    src_time > out_time
}

fn build_dispatcher_direct(
    workspace_root: &PathBuf,
    libbpf_location: &LibbpfLocation,
    src_name: &str,
    out_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let src_path = workspace_root.join("bpf").join(src_name);
    let out_path = workspace_root.join(".output").join(out_name);

    println!("cargo:warning=Building dispatcher: {} -> {}", src_path.display(), out_path.display());

    let mut cmd = Command::new("clang");

    // Add libbpf flags based on how we found libbpf
    match libbpf_location {
        LibbpfLocation::Source(path) => {
            // For source builds, include the src directory
            cmd.arg(format!("-I{}", path.join("src").display()));
        }
        LibbpfLocation::PkgConfig(lib) => {
            // For pkg-config, use the CFLAGS it provides
            for include_path in &lib.include_paths {
                cmd.arg(format!("-I{}", include_path.display()));
            }
        }
    }

    // Get target architecture define for eBPF compilation
    let target_arch_define = get_target_arch_define()?;

    // Standard eBPF compilation flags
    cmd.args([
        "-g", "-O2", "-target", "bpfel", "-c",
        &target_arch_define,
    ]);

    cmd.arg(&src_path);
    cmd.arg("-o");
    cmd.arg(&out_path);

    println!("cargo:warning=Executing clang command: {:?}", cmd);
    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!("clang compilation failed for {}: stdout: {} stderr: {}", src_name, stdout, stderr).into());
    }

    if !out_path.exists() {
        return Err(format!("Expected output file {} was not created", out_path.display()).into());
    }

    println!("cargo:warning=Successfully built: {}", out_path.display());
    Ok(())
}

fn get_target_arch_define() -> Result<String, Box<dyn std::error::Error>> {
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;

    let define = match target_arch.as_str() {
        "x86_64" | "i686" => "-D__TARGET_ARCH_x86",
        "aarch64" => "-D__TARGET_ARCH_arm64",
        "powerpc64le" => "-D__TARGET_ARCH_powerpc",
        "s390x" => "-D__TARGET_ARCH_s390",
        _ => return Err(format!("Unsupported target architecture: {}", target_arch).into()),
    };

    println!("cargo:warning=Using target architecture define: {} for {}", define, target_arch);
    Ok(define.to_string())
}

enum LibbpfLocation {
    Source(PathBuf),
    PkgConfig(Box<Library>),
}

fn find_libbpf() -> Result<LibbpfLocation, Box<dyn std::error::Error>> {
    // Check environment variable first (for explicit override like CI)
    if let Ok(libbpf_dir) = env::var("LIBBPF_DIR") {
        let path = PathBuf::from(libbpf_dir);
        if path.join("src").exists() {
            return Ok(LibbpfLocation::Source(path));
        }
    }

    // Use pkg-config to find system-installed libbpf
    let lib = pkg_config::probe_library("libbpf")?;
    Ok(LibbpfLocation::PkgConfig(Box::new(lib)))
}

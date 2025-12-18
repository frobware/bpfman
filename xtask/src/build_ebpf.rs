use std::{
    env,
    fs::{self, create_dir_all},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use clap::Parser;

use crate::workspace::WORKSPACE_ROOT;

#[derive(Debug, Parser)]
pub struct Options {
    /// Optional: Build the release target
    #[clap(long)]
    pub release: bool,
    /// Optional: Compile rust eBPF dispatcher
    #[clap(long)]
    pub compile_rust_ebpf: bool,
    /// Required: Libbpf dir, required for compiling C code
    #[clap(long, action)]
    pub libbpf_dir: PathBuf,
}

fn get_libbpf_headers<P: AsRef<Path>>(libbpf_dir: P, include_path: P) -> anyhow::Result<()> {
    let dir = include_path.as_ref();
    fs::create_dir_all(dir)?;
    let status = Command::new("make")
        .current_dir(libbpf_dir.as_ref().join("src"))
        .arg(format!("INCLUDEDIR={}", dir.as_os_str().to_string_lossy()))
        .arg("install_headers")
        .status()
        .expect("failed to build get libbpf headers");
    assert!(status.success());
    Ok(())
}

fn build_ebpf_files(
    src_path: PathBuf,
    include_path: PathBuf,
    out_path: PathBuf,
) -> anyhow::Result<()> {
    let files = fs::read_dir(src_path).unwrap();
    for file in files {
        let p = file.unwrap().path();
        if let Some(ext) = p.extension() {
            if ext == "c" {
                let mut out = PathBuf::from(&out_path);
                out.push(p.file_stem().unwrap());
                compile_with_clang(&p, &out, &include_path)?;

                // For dispatcher files, create simple-named versions for include_bytes_aligned!
                // Normal build creates arch-specific files (bpf_x86_bpfel.o), but embedding
                // expects simple names (xdp_dispatcher_v2.bpf.o) for hermetic builds
                if let Some(filename) = p.file_name().and_then(|n| n.to_str()) {
                    if filename.contains("dispatcher") {
                        compile_dispatcher_for_embedding(&p, &out_path, &include_path)?;
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn build_ebpf(opts: Options) -> anyhow::Result<()> {
    // build operational eBPF code
    let mut src_path = PathBuf::from(WORKSPACE_ROOT.to_string());
    src_path.push("bpf");

    let mut out_path = PathBuf::from(WORKSPACE_ROOT.to_string());
    out_path.push(".output");
    create_dir_all(&out_path)?;

    let include_path = out_path.join("include");
    get_libbpf_headers(&opts.libbpf_dir, &include_path)?;

    build_ebpf_files(src_path, include_path.clone(), out_path)?;

    // build integration test eBPF code (reuse include_path)
    let mut src_path = PathBuf::from(WORKSPACE_ROOT.to_string());
    src_path.push("tests/integration-test/bpf");

    let mut out_path = PathBuf::from(WORKSPACE_ROOT.to_string());
    out_path.push("tests/integration-test/bpf/.output");
    create_dir_all(&out_path)?;

    build_ebpf_files(src_path, include_path, out_path)?;

    Ok(())
}

/// Build eBPF programs with clang and libbpf headers.
fn compile_with_clang<P: Clone + AsRef<Path>>(
    src: P,
    out: P,
    include_path: P,
) -> anyhow::Result<()> {
    let clang = match env::var("CLANG") {
        Ok(val) => val,
        Err(_) => String::from("/usr/bin/clang"),
    };
    // (Clang arch string, used to define the clang -target flag, as per
    // "clang -print-targets",
    // Linux arch string, used to define __TARGET_ARCH_xzy macros used by
    // https://github.com/libbpf/libbpf/blob/master/src/bpf_tracing.h)
    //
    let arches = Vec::from([
        ("bpfel", "x86"),
        ("bpfel", "arm64"),
        ("bpfeb", "s390"),
        ("bpfel", "powerpc"),
    ]);
    for (target, arch) in arches {
        // remove the .bpf postfix
        let mut outfile = out.as_ref().to_path_buf();
        fs::create_dir_all(&outfile)?;
        // make sure our filenames are compataible with cilium/ebpf
        outfile.push(format!("bpf_{arch}_{target}.o"));
        let mut cmd = Command::new(clang.clone());
        cmd.arg(format!("-I{}", include_path.as_ref().to_string_lossy()))
            .arg("-g")
            .arg("-O2")
            .arg("-target")
            .arg(target)
            .arg("-c")
            .arg(format!("-D__TARGET_ARCH_{}", arch))
            .arg(src.as_ref().as_os_str())
            .arg("-o")
            .arg(outfile);

        let output = cmd.output().context("Failed to execute clang")?;
        if !output.status.success() {
            bail!(
                "Failed to compile BPF programs\n \
                stdout=\n \
                {}\n \
                stderr=\n \
                {}\n",
                String::from_utf8(output.stdout).unwrap(),
                String::from_utf8(output.stderr).unwrap()
            );
        }
    }

    Ok(())
}

/// Compile dispatcher for embedding with include_bytes_aligned!
/// Creates simple filenames like xdp_dispatcher_v2.bpf.o for hermetic builds
fn compile_dispatcher_for_embedding<P: Clone + AsRef<Path>>(
    src: P,
    out_dir: P,
    include_path: P,
) -> anyhow::Result<()> {
    let clang = match env::var("CLANG") {
        Ok(val) => val,
        Err(_) => String::from("clang"),
    };

    // Create simple output filename for embedding (remove .c, keep .bpf.o)
    let src_filename = src.as_ref().file_name().unwrap().to_str().unwrap();
    let out_filename = src_filename.replace(".c", ".o");
    let outfile = out_dir.as_ref().join(out_filename);

    // Compile for x86 target as default for embedding
    let mut cmd = Command::new(clang);
    cmd.arg(format!("-I{}", include_path.as_ref().to_string_lossy()))
        .arg("-g")
        .arg("-O2")
        .arg("-target")
        .arg("bpfel")
        .arg("-c")
        .arg("-D__TARGET_ARCH_x86")
        .arg(src.as_ref().as_os_str())
        .arg("-o")
        .arg(&outfile);

    let output = cmd.output().context("Failed to execute clang for dispatcher embedding")?;
    if !output.status.success() {
        bail!(
            "Failed to compile dispatcher for embedding: {}\n \
            stdout=\n \
            {}\n \
            stderr=\n \
            {}\n",
            outfile.display(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap()
        );
    }

    println!("Compiled dispatcher for embedding: {}", outfile.display());
    Ok(())
}

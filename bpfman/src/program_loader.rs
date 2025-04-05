// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

//! # Program Loader Module
//!
//! This module is responsible for loading and unloading eBPF programs
//! into and out of the kernel. It handles the parsing of eBPF
//! bytecode, loading of individual programs (along with any
//! associated maps), and, if a load error occurs, performing a
//! best-effort rollback by unloading any programs that were already
//! loaded. This module uses the [aya] library for interfacing with
//! the kernel.
//!
//! ## Separation of Concerns
//!
//! Note: This module focuses solely on kernel-level operations
//! (loading/unloading eBPF programs). It does not deal with database
//! (DB) persistence. Database-related actions are handled by a higher
//! layer (e.g., the public API function that wraps these kernel
//! operations in a DB transaction). In the event of a DB persistence
//! failure, the higher layer will use the unload functionality
//! provided here to roll back the kernel state.
//!
//! [aya]: https://github.com/aya-rs/aya
use std::{collections::HashMap, path::PathBuf};

use anyhow::Result;
use aya::{Ebpf, maps::Map};
use chrono::Utc;
use derive_builder::Builder;
use serde::Serialize;
use serde_json;

use crate::{
    BpfmanError, ProgramType, calc_map_pin_path, create_map_pin_path,
    directories::*,
    init_image_manager,
    k32::KernelU32,
    models::{BpfMap, BpfProgram},
    types::Location,
    uintblob::U64Blob,
    utils::should_map_be_pinned,
};

/// The `LoadSpec` struct defines the parameters required for loading eBPF
/// programs.
///
/// This struct is used to configure the details for loading eBPF programs,
/// including program bytecode, function names, global data, metadata, and
/// other related parameters.
///
/// It uses a **builder pattern** to construct instances of `LoadSpec`,
/// allowing for flexible and incremental configuration of the struct's
/// fields.
///
/// ```rust
/// use bpfman::program_loader::LoadSpecBuilder;
/// use bpfman::types::Location;
///
/// let load_spec = LoadSpecBuilder::default()
///     .bytecode_source(Location::File("path/to/program.o".to_string()))
///     .global_data(vec![("key1".to_string(), b"value1".to_vec())])
///     .build();
/// ```
///
/// # Notes
/// - The `global_data` and `metadata` fields are also optional and will
///   default to `None` if not provided. These fields are serialized to JSON
///   when the struct is built.
#[derive(Debug, Builder)]
#[builder(pattern = "mutable", build_fn(name = "build_partial"))]
pub struct LoadSpec {
    #[builder(setter(into))]
    bytecode_source: Location,

    #[builder(default)]
    programs: Vec<ProgramType>,

    #[builder(setter(strip_option), default)]
    global_data: Option<Vec<(String, Vec<u8>)>>,

    #[builder(setter(strip_option), default)]
    metadata: Option<Vec<(String, String)>>,

    #[builder(default)]
    map_owner_id: Option<u32>,

    // The following fields are computed in build().
    #[builder(setter(skip), default = "String::from(\"{}\")")]
    global_data_json: String,

    #[builder(setter(skip), default = "String::from(\"{}\")")]
    metadata_json: String,
}

impl LoadSpecBuilder {
    pub fn build(&mut self) -> Result<LoadSpec, String> {
        let mut spec = self.build_partial().map_err(|e| e.to_string())?;

        let global_data_map =
            Self::global_data_to_map(spec.global_data.as_deref().unwrap_or_default());
        spec.global_data_json = serde_json::to_string(&global_data_map)
            .map_err(|e| format!("Failed to serialise global data to JSON: {}", e))?;

        let metadata_map = Self::metadata_to_map(spec.metadata.as_deref().unwrap_or_default());
        spec.metadata_json = serde_json::to_string(&metadata_map)
            .map_err(|e| format!("Failed to serialise metadata to JSON: {}", e))?;

        println!("{:?}", self.programs);

        // // Parse and validate program types
        // let mut validated_programs = Vec::new();
        // for (prog_type, parts) in self.programs.as_ref().unwrap_or(&vec![]) {
        //     // Get the program name
        //     let name = parts
        //         .first()
        //         .ok_or_else(|| format!("Missing program name for {}", prog_type))?
        //         .clone();

        //     // Reconstruct the program string for parsing.
        //     let program_str = if parts.is_empty() {
        //         prog_type.clone()
        //     } else {
        //         format!("{}:{}", prog_type, parts.join(":"))
        //     };

        //     // Use ProgramType::parse for validation
        //     let program_type = ProgramType::parse(&program_str)
        //         .map_err(|e| format!("Invalid program type: {}", e))?;

        //     println!("{:?}", program_type);

        //     validated_programs.push((program_type, name));
        // }

        // spec.programs_by_type = validated_programs;

        Ok(spec)
    }

    fn global_data_to_map(data: &[(String, Vec<u8>)]) -> HashMap<String, Vec<u8>> {
        data.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    fn metadata_to_map(data: &[(String, String)]) -> HashMap<String, String> {
        data.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

/// Represents a program that was successfully loaded into the kernel,
/// along with any associated pinned maps.
///
/// This structure records the state of an eBPF program at the time of
/// its successful kernel load. Depending on subsequent operations,
/// the program may still be live in the kernel, or it may have been
/// unloaded (rolled back) due to an error in a later phase (such as
/// database persistence).
///
/// In both cases, the information contained in this structure remains
/// accurate and useful as a historical record of the load operation.
/// However, whether the program is currently live or not must be
/// interpreted in the context of subsequent unload operations.
#[derive(Debug, Serialize)]
pub struct LoadedProgram {
    pub kind: ProgramType, // XXX(frobware): do we really need this?
    pub program: BpfProgram,
    pub maps: Vec<BpfMap>,
}

/// Represents an error encountered when unloading an eBPF program
/// from the kernel.
///
/// This structure is used to capture and report details about
/// failures during the unload (rollback) process. For example, if a
/// previously loaded program cannot be properly removed from the
/// kernel during error handling, an `UnloadError` is generated for
/// that programme.
#[derive(Debug)]
pub struct UnloadError {
    pub program_id: KernelU32,
    /// The underlying error encountered during the unload operation.
    /// This error is stored as an `anyhow::Error` to allow downstream
    /// callers to inspect or downcast it if needed.
    pub error: anyhow::Error,
}

fn build_bpfmap_from_aya_map(data: &Map, map_name: &str) -> Result<BpfMap, aya::maps::MapError> {
    let info = match data {
        Map::Array(d)
        | Map::BloomFilter(d)
        | Map::CpuMap(d)
        | Map::DevMap(d)
        | Map::DevMapHash(d)
        | Map::HashMap(d)
        | Map::LpmTrie(d)
        | Map::LruHashMap(d)
        | Map::PerCpuArray(d)
        | Map::PerCpuHashMap(d)
        | Map::PerCpuLruHashMap(d)
        | Map::PerfEventArray(d)
        | Map::ProgramArray(d)
        | Map::Queue(d)
        | Map::RingBuf(d)
        | Map::SockHash(d)
        | Map::SockMap(d)
        | Map::Stack(d)
        | Map::StackTraceMap(d)
        | Map::XskMap(d) => d.info()?,
        Map::Unsupported(d) => d.info()?,
    };

    Ok(BpfMap {
        id: info.id().into(),
        name: map_name.to_string(),
        map_type: Some(format!("{:?}", info.map_type()?)),
        key_size: KernelU32::from(info.key_size()),
        value_size: KernelU32::from(info.value_size()),
        max_entries: KernelU32::from(info.max_entries()),
        created_at: Utc::now().naive_utc(),
        updated_at: None,
    })
}

fn build_bpfprogram_from_aya_program(
    prog_info: &aya::programs::ProgramInfo,
    program_type: &ProgramType,
    spec: &LoadSpec,
    map_pin_path_str: &str,
) -> Result<BpfProgram, BpfmanError> {
    let (location_type, file_path, image_url, image_pull_policy, username, password) =
        match &spec.bytecode_source {
            Location::File(path) => ("file", Some(path.clone()), None, None, None, None),
            Location::Image(image) => (
                "image",
                None,
                Some(image.image_url.clone()),
                Some(image.image_pull_policy.to_string()),
                image.username.clone(),
                image.password.clone(),
            ),
        };

    if location_type == "file" && file_path.is_none() {
        return Err(BpfmanError::Error(
            "File-based program requires a file path".to_string(),
        ));
    }
    if location_type == "image" && image_url.is_none() {
        return Err(BpfmanError::Error(
            "Image-based program requires an image URL".to_string(),
        ));
    }

    let kernel_name = prog_info
        .name_as_str()
        .map(|n| n.to_string())
        .or_else(|| Some(format!("prog_{}", prog_info.id())));

    let kernel_loaded_at = prog_info
        .loaded_at()
        .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());

    let prog_name = program_type
        .function_name()
        .ok_or_else(|| BpfmanError::BpfFunctionNameNotValid("<none>".to_string()))?;

    Ok(BpfProgram {
        id: prog_info.id().into(),
        name: prog_name.to_owned(),
        kind: program_type.type_str().to_owned(),
        state: "loaded".to_string(),
        location_type: location_type.to_string(),
        file_path,
        image_url,
        image_pull_policy,
        username,
        password,
        map_pin_path: map_pin_path_str.to_string(),
        map_owner_id: spec.map_owner_id.map(KernelU32::from),
        program_bytes: vec![], // TODO(frobware) XXX
        // program_bytes: spec.program_bytes.to_vec(),
        metadata: spec.metadata_json.clone(),
        global_data: spec.global_data_json.clone(),
        retprobe: program_type.is_retprobe(),
        fn_name: program_type.function_name().map(String::from),
        kernel_name,
        kernel_program_type: prog_info
            .program_type()
            .ok()
            .map(|pt| KernelU32::from(pt as u32)),
        kernel_loaded_at,
        kernel_tag: U64Blob::from(prog_info.tag()),
        kernel_gpl_compatible: prog_info.gpl_compatible(),
        kernel_btf_id: prog_info.btf_id().map(KernelU32::from),
        kernel_bytes_xlated: prog_info.size_translated().map(KernelU32::from),
        kernel_jited: Some(prog_info.size_jitted() > 0),
        kernel_bytes_jited: Some(KernelU32::from(prog_info.size_jitted())),
        kernel_verified_insns: prog_info.verified_instruction_count().map(KernelU32::from),
        kernel_bytes_memlock: prog_info.memory_locked().ok().map(KernelU32::from),
        created_at: Utc::now().naive_utc(),
        updated_at: None,
    })
}

/// Converts a generic `aya::programs::Program` into its specific type
/// and loads it into the kernel.
///
/// This function does not handle pinning or metadata extraction.
///
/// # Arguments
///
/// - `program_type`: The expected type of the eBPF program.
/// - `ebpf_program`: A mutable reference to the program retrieved from `program_mut()`.
///
/// # Returns
///
/// - `Ok(())` if the program is successfully converted and loaded.
/// - `Err(BpfmanError)` if conversion or loading fails.
fn load_program(
    program_type: &ProgramType,
    ebpf_program: &mut aya::programs::Program,
) -> Result<(), BpfmanError> {
    match program_type {
        ProgramType::Xdp { function_name: _ } => {
            let prog: &mut aya::programs::Xdp = ebpf_program
                .try_into()
                .map_err(BpfmanError::BpfProgramError)?;
            prog.load().map_err(BpfmanError::BpfProgramError)?;
        }
        ProgramType::Tc | ProgramType::Tcx => {
            let prog: &mut aya::programs::SchedClassifier = ebpf_program
                .try_into()
                .map_err(BpfmanError::BpfProgramError)?;
            prog.load().map_err(BpfmanError::BpfProgramError)?;
        }
        ProgramType::Tracepoint { function_name: _ } => {
            let prog: &mut aya::programs::TracePoint = ebpf_program
                .try_into()
                .map_err(BpfmanError::BpfProgramError)?;
            prog.load().map_err(BpfmanError::BpfProgramError)?;
        }
        ProgramType::Kprobe { function_name: _ } | ProgramType::Kretprobe { function_name: _ } => {
            let prog: &mut aya::programs::KProbe = ebpf_program
                .try_into()
                .map_err(BpfmanError::BpfProgramError)?;
            prog.load().map_err(BpfmanError::BpfProgramError)?;
        }
        ProgramType::Uprobe { function_name: _ } | ProgramType::Uretprobe { function_name: _ } => {
            let prog: &mut aya::programs::UProbe = ebpf_program
                .try_into()
                .map_err(BpfmanError::BpfProgramError)?;
            prog.load().map_err(BpfmanError::BpfProgramError)?;
        }
        ProgramType::Fentry {
            function_name: _,
            attach_function,
        } => {
            let btf = aya::Btf::from_sys_fs().map_err(BpfmanError::BtfError)?;
            let prog: &mut aya::programs::FEntry = ebpf_program
                .try_into()
                .map_err(BpfmanError::BpfProgramError)?;
            prog.load(attach_function, &btf)
                .map_err(BpfmanError::BpfProgramError)?;
        }
        ProgramType::Fexit {
            function_name: _,
            attach_function,
        } => {
            let btf = aya::Btf::from_sys_fs().map_err(BpfmanError::BtfError)?;
            let prog: &mut aya::programs::FExit = ebpf_program
                .try_into()
                .map_err(BpfmanError::BpfProgramError)?;
            prog.load(attach_function, &btf)
                .map_err(BpfmanError::BpfProgramError)?;
        }
    };

    Ok(())
}

fn attempt_unload(_lp: &LoadedProgram) -> Result<()> {
    // TODO(frobware).
    // Circle back here when we address the `unload` bpfman command. We
    // may want to go through the public API (i.e., the front door).
    // todo!("Implement unload using sqlite interface");
    Ok(())
}

/// Loads an individual eBPF program into the kernel and returns
/// associated metadata.
///
/// This function retrieves the eBPF program from the loaded bytecode,
/// applies necessary configurations, and loads it into the kernel. It
/// also gathers metadata such as pinned maps and program information.
///
/// # Arguments
///
/// - `program_type`: The type of eBPF program being loaded (e.g.,
///   `Xdp`, `Kprobe`).
/// - `name`: The name of the function within the bytecode.
/// - `program_bytecode`: A mutable reference to an `Ebpf` instance
///   containing loaded bytecode.
///
/// # Returns
///
/// - `Ok(EbpfLoadResult)` containing metadata about the loaded
///   program.
/// - `Err(BpfmanError)` if program retrieval or kernel loading fails.
///
/// # Errors
///
/// - If the function name does not exist in the loaded bytecode.
/// - If the program fails to convert into its expected type.
/// - If the kernel rejects the program.
fn load_program_into_kernel(
    program: &ProgramType,
    bytecode: &mut Ebpf,
    spec: &LoadSpec,
) -> Result<LoadedProgram, BpfmanError> {
    let prog_name = program
        .function_name()
        .ok_or_else(|| BpfmanError::BpfFunctionNameNotValid("<none>".to_string()))?;

    let ebpf_program = bytecode
        .program_mut(prog_name)
        .ok_or_else(|| BpfmanError::BpfFunctionNameNotValid(prog_name.to_string()))?;

    load_program(program, ebpf_program)?;

    // Retrieve the program info after loading.
    let prog_info = ebpf_program.info().map_err(BpfmanError::BpfProgramError)?;
    let id = prog_info.id();

    // Pin the program.
    let program_pin_path = format!("{RTDIR_FS}/prog_{id}");
    ebpf_program
        .pin(&program_pin_path)
        .map_err(BpfmanError::UnableToPinProgram)?;

    // Calculate the map pin path and create the directory.
    let map_pin_path: PathBuf = calc_map_pin_path(id);
    create_map_pin_path(&map_pin_path)?;

    // Collect map metadata.
    let mut maps = Vec::new();
    for (map_name, map) in bytecode.maps_mut() {
        if !should_map_be_pinned(map_name) {
            continue;
        }

        // Pin the map.
        let map_fs_path = map_pin_path.join(map_name);
        map.pin(map_fs_path.clone())
            .map_err(BpfmanError::UnableToPinMap)?;

        maps.push(build_bpfmap_from_aya_map(map, map_name).map_err(BpfmanError::BpfMapInfoError)?);
    }

    let map_pin_path_str = map_pin_path.to_string_lossy().to_string();
    let bpf_prog = build_bpfprogram_from_aya_program(&prog_info, program, spec, &map_pin_path_str);

    Ok(LoadedProgram {
        kind: program.clone(),
        program: bpf_prog?,
        maps,
    })
}

/// Loads eBPF programs from a `LoadSpec`, parsing bytecode and
/// registering programs.
///
/// This function takes a `LoadSpec`, which contains information about
/// the bytecode source, function mappings, metadata, and other
/// parameters. It performs the following steps:
///
/// 1. Creates an `EbpfLoader` to parse and prepare the bytecode.
/// 2. Loads the bytecode into an `Ebpf` instance, representing the
///    set of parsed eBPF programs.
/// 3. Iterates over the program definitions in the spec, extracting
///    the function names and types.
/// 4. Calls `load_into_kernel` to load each program into the kernel
///    and collect metadata.
/// 5. Returns a vector of `EbpfLoadResult`, containing details about
///    the loaded programs.
///
/// # Arguments
///
/// * `params` - A `LoadSpec` containing eBPF program definitions,
///   bytecode, and metadata.
///
/// # Returns
///
/// * `Ok(Vec<EbpfLoadResult>)` - A list of successfully loaded eBPF
///   programs.
/// * `Err(BpfmanError)` - If loading the bytecode, parsing programs,
///   or kernel loading fails.
///
/// # Errors
///
/// This function may return an error in the following cases:
/// - The bytecode fails to load (e.g., invalid format, missing
///   sections).
/// - A program definition is invalid (e.g., missing function name).
/// - An attempt to load a program into the kernel fails.
pub(crate) fn load_from_spec(spec: &LoadSpec) -> Result<Vec<LoadedProgram>, BpfmanError> {
    let mut image_manager = init_image_manager()?;

    let (program_bytes, _function_names) = spec
        .bytecode_source
        .get_program_bytes_no_sled(&mut image_manager)?;

    let mut bytecode_loader = aya::EbpfLoader::new();
    bytecode_loader.allow_unsupported_maps();

    let mut program_bytecode = bytecode_loader
        .load(&program_bytes)
        .map_err(BpfmanError::BpfLoadError)?;

    let mut loaded_programs = Vec::new();

    for program in &spec.programs {
        match load_program_into_kernel(program, &mut program_bytecode, spec) {
            Ok(loaded) => loaded_programs.push(loaded),
            Err(err) => {
                let unload_failures = unload_all(&loaded_programs);

                return Err(BpfmanError::ProgramLoadError {
                    cause: Box::new(err),
                    loaded_before_failure: loaded_programs,
                    unload_failures,
                });
            }
        }
    }

    Ok(loaded_programs)
}

/// Attempts to unload every program in the provided slice.
///
/// For each `LoadedProgram` in `programs`, this function calls
/// `attempt_unload`. If an unload operation fails, it records an
/// `UnloadError` containing the program's unique identifier and a
/// description of the error. The returned vector contains all unload
/// failures; if all programs are successfully unloaded, the vector
/// will be empty.
///
/// # Arguments
///
/// - `programs`: A slice of `LoadedProgram` instances that were
///   previously loaded into the kernel.
///
/// # Returns
///
/// A vector of `UnloadError` instances detailing any failures
/// encountered during the unload process.
pub(crate) fn unload_all(programs: &[LoadedProgram]) -> Vec<UnloadError> {
    let mut failures = Vec::new();

    for lp in programs {
        if let Err(e) = attempt_unload(lp) {
            failures.push(UnloadError {
                program_id: lp.program.id,
                error: e,
            });
        }
    }

    failures
}

#[cfg(test)]
mod tests {
    mod load_spec {
        use crate::{ProgramType, program_loader::LoadSpecBuilder, types::Location};

        fn valid_programs() -> Vec<ProgramType> {
            vec![ProgramType::Tcx]
        }

        #[test]
        fn test_build_fails_with_no_fields() {
            let result = LoadSpecBuilder::default().build();
            assert!(result.is_err());
        }

        #[test]
        fn test_build_global_data_serialises_to_json() {
            let result = LoadSpecBuilder::default()
                .bytecode_source(Location::File("path/to/bytecode".into()))
                .global_data(vec![
                    ("key1".into(), b"value1".to_vec()),
                    ("key2".into(), b"value2".to_vec()),
                ])
                .programs(valid_programs())
                .build();

            assert!(result.is_ok());
            let spec = result.unwrap();

            let json: serde_json::Value = serde_json::from_str(&spec.global_data_json).unwrap();
            assert!(json.get("key1").is_some(), "expected key1 in JSON");
            assert!(json.get("key2").is_some(), "expected key2 in JSON");
        }

        #[test]
        fn test_build_metadata_serialises_to_json() {
            let result = LoadSpecBuilder::default()
                .bytecode_source(Location::File("path/to/bytecode".into()))
                .metadata(vec![
                    ("key1".into(), "value1".to_string()),
                    ("key2".into(), "value2".to_string()),
                ])
                .programs(valid_programs())
                .build();

            assert!(result.is_ok());
            let spec = result.unwrap();

            let json: serde_json::Value = serde_json::from_str(&spec.metadata_json).unwrap();
            assert!(json.get("key1").is_some(), "expected key1 in JSON");
            assert!(json.get("key2").is_some(), "expected key2 in JSON");
        }
    }
}

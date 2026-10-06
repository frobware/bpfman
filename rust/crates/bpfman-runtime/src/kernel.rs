//! Input file capture; ELF interpretation and kernel objects belong to the backend.
use crate::load_error::LoadCause;
use bpfman_model::ProgramSpec;
use std::{
    collections::BTreeMap, fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, path::Path,
};

pub(super) struct LocalObject {
    pub(super) bytes: Vec<u8>,
    pub(super) license: String,
    pub(super) maps: Vec<String>,
    pub(super) globals: BTreeMap<String, Vec<u8>>,
}

impl LocalObject {
    pub(super) fn read(
        kernel: &impl bpfman_kernel::ObjectLoader,
        path: &Path,
        spec: &ProgramSpec,
    ) -> Result<Self, LoadCause> {
        // O_NONBLOCK makes a FIFO fail regular-file validation instead of hanging.
        // This is input I/O, not a managed-object filesystem operation.
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(LoadCause::Read)?;

        if !file.metadata().map_err(LoadCause::Read)?.is_file() {
            return Err(LoadCause::Invalid(
                "source must be a regular ELF file".into(),
            ));
        }

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(LoadCause::Read)?;
        let info = kernel.validate_object(&bytes, spec)?;
        Ok(Self {
            bytes,
            license: info.license,
            maps: info.maps,
            globals: BTreeMap::new(),
        })
    }
}

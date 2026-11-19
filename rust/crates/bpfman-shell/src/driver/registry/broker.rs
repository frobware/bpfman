use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

use super::super::process::{self, Interrupt};

const SOURCE_ENV: &str = "BPFMAN_E2E_BYTECODE_SOURCE";
const ROOT_ENV: &str = "BPFMAN_E2E_REPO_ROOT";

pub(super) struct Load {
    bytecode: PathBuf,
    relative: PathBuf,
}

impl Load {
    pub(super) fn from_environment(argv: &[String]) -> Result<Option<Self>, String> {
        let [noun, operation, source, bytecode, ..] = argv else {
            return Ok(None);
        };
        if noun != "program"
            || operation != "load"
            || source != "file"
            || env::var_os(SOURCE_ENV).is_none_or(|mode| mode != "image")
        {
            return Ok(None);
        }
        let root = env::var_os(ROOT_ENV)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| format!("{SOURCE_ENV}=image requires {ROOT_ENV}"))?;
        let root = Path::new(&root)
            .canonicalize()
            .map_err(|error| format!("{ROOT_ENV}: {error}"))?;
        let bytecode = Path::new(bytecode)
            .canonicalize()
            .map_err(|error| format!("{bytecode}: {error}"))?;
        Self::plan(&root, bytecode).map(Some)
    }

    fn plan(root: &Path, bytecode: PathBuf) -> Result<Self, String> {
        let relative = bytecode
            .strip_prefix(root)
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| {
                format!(
                    "bytecode path {:?} is outside repository root {}",
                    bytecode,
                    root.display()
                )
            })?
            .to_owned();
        Ok(Self { bytecode, relative })
    }

    fn reference(&self, host: &str, pid: u32, sequence: u64) -> Result<String, String> {
        let name = self
            .relative
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or("bytecode name must be UTF-8")?;
        let relative = self
            .relative
            .to_str()
            .ok_or("bytecode path must be UTF-8")?;
        let digest = Sha256::digest(relative.as_bytes());
        let prefix = digest[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(format!(
            "{host}/bpfman-e2e/{}:{prefix}-{pid}-{sequence}",
            super::repository(name)
        ))
    }
}

#[derive(Default)]
pub(super) struct Cache(HashMap<PathBuf, String>);

impl Cache {
    pub(super) fn resolve(
        &mut self,
        load: Load,
        host: &str,
        executable: &OsStr,
        interrupt: &Interrupt,
    ) -> Result<String, String> {
        if let Some(reference) = self.0.get(&load.bytecode) {
            return Ok(reference.clone());
        }
        let reference = load.reference(host, std::process::id(), super::next_sequence())?;
        let output = process::capture(
            Command::new(executable)
                .args(["image", "build", &reference])
                .arg(&load.bytecode),
            interrupt,
        )?;
        if !output.status.success() {
            return Err(format!(
                "build bytecode image {reference} from {}: {}\n{}{}",
                load.bytecode.display(),
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            ));
        }
        self.0.insert(load.bytecode, reference.clone());
        Ok(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_reference_is_scoped_to_repo_and_process() -> Result<(), String> {
        let file = PathBuf::from("/repo/fixtures/My_object.bpf.o");
        let load = Load::plan(Path::new("/repo"), file.clone())?;
        let first = load.reference("127.0.0.1:2345", 7, 1)?;
        let second = load.reference("127.0.0.1:2345", 7, 2)?;
        assert!(first.starts_with("127.0.0.1:2345/bpfman-e2e/my-object-bpf:"));
        assert!(first.ends_with("-7-1"));
        assert_eq!(first.trim_end_matches('1'), second.trim_end_matches('2'));
        assert!(Load::plan(Path::new("/rep"), file).is_err());
        Ok(())
    }
}

//! Process-level child observations assembled from each thread's children list.

use std::{fs, io, num::NonZeroU32, path::Path};

use bpfman_shell_runtime::{Argument, Value};

pub(super) fn invoke(arguments: &[Argument]) -> Result<Value, String> {
    let [operation, pid] = arguments else {
        return Err("process: usage: process children PID".to_owned());
    };
    if super::argument(operation)? != "children" {
        return Err("process: usage: process children PID".to_owned());
    }
    let pid = super::argument(pid)?
        .parse::<NonZeroU32>()
        .map_err(|error| format!("process children: invalid PID: {error}"))?;
    let pids = read_children(Path::new("/proc"), pid)?;
    super::bpfman::project(serde_json::json!({ "count": pids.len(), "pids": pids }))
}

fn read_children(root: &Path, pid: NonZeroU32) -> Result<Vec<u32>, String> {
    let tasks = root.join(pid.to_string()).join("task");
    let entries = fs::read_dir(&tasks).map_err(|error| observe_error(&tasks, error))?;
    let mut pids = Vec::new();
    let mut saw_entry = false;
    let mut read_task = false;
    for entry in entries {
        let task = entry.map_err(|error| observe_error(&tasks, error))?.path();
        saw_entry = true;
        let path = task.join("children");
        let children = match fs::read_to_string(&path) {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound => match task.try_exists() {
                Ok(false) => continue,
                Ok(true) => {
                    return Err(format!(
                        "process children: {} missing for live task",
                        path.display()
                    ));
                }
                Err(error) => return Err(observe_error(&task, error)),
            },
            Err(error) => return Err(observe_error(&path, error)),
        };
        read_task = true;
        for child in children.split_whitespace() {
            let child = child.parse::<NonZeroU32>().map_err(|_| {
                format!(
                    "process children: malformed PID {child:?} in {}",
                    path.display()
                )
            })?;
            pids.push(child.get());
        }
    }
    if saw_entry && !read_task {
        fs::metadata(&tasks).map_err(|error| observe_error(&tasks, error))?;
    }
    pids.sort_unstable();
    Ok(pids)
}

fn observe_error(path: &Path, error: io::Error) -> String {
    format!("process children: {}: {error}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_lists_merge_threads_and_distinguish_missing_observations() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pid = NonZeroU32::MIN;
        let tasks = root.path().join("1/task");
        assert!(read_children(root.path(), pid).is_err());
        for (thread, children) in [("1", "6 10"), ("2", "3 5")] {
            let path = tasks.join(thread);
            fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            fs::write(path.join("children"), children).map_err(|error| error.to_string())?;
        }
        assert_eq!(read_children(root.path(), pid)?, [3, 5, 6, 10]);
        fs::create_dir(tasks.join("4")).map_err(|error| error.to_string())?;
        assert!(read_children(root.path(), pid).is_err());
        Ok(())
    }
}

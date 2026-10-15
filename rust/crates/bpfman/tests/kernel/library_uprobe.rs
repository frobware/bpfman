//! Real host cache lookup and host-PID maps across a PID-namespace boundary.
use super::support::*;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
};

struct Worker(Child);

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn load(c: &Context, kind: &str) -> String {
    c.json(
        &rust(),
        &[
            "program",
            "load",
            "file",
            fixture("uprobe_counter.bpf.o").to_str().expect("fixture"),
            "--programs",
            &format!("{kind}:uprobe_counter"),
            "-o",
            "json",
        ],
    )["programs"][0]["record"]["program_id"]
        .as_u64()
        .expect("program id")
        .to_string()
}

fn cache(key: &str, path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut data = b"glibc-ld.so.cache1.1".to_vec();
    data.extend_from_slice(&1_u32.to_ne_bytes());
    data.resize(72, 0);
    data[52..56].copy_from_slice(&72_u32.to_ne_bytes());
    data[56..60].copy_from_slice(&(73 + key.len() as u32).to_ne_bytes());
    data.extend_from_slice(key.as_bytes());
    data.push(0);
    data.extend_from_slice(path.as_os_str().as_bytes());
    data.push(0);
    data
}

fn host_cache(c: &Context, cache: &Path, args: &[&str], success: bool) -> std::process::Output {
    let output = Command::new("unshare")
        .args(["--mount", "--", "sh", "-c", "mount --make-rprivate / && mount -t tmpfs tmpfs /etc && cp \"$1\" /etc/ld.so.cache && shift && exec \"$@\"", "cache-namespace"])
        .arg(cache).arg(rust()).arg("--runtime-dir").arg(c.layout.root()).args(args)
        .output().expect("host cache CLI");
    assert_eq!(
        output.status.success(),
        success,
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub(super) fn exercise() {
    for kind in ["uprobe", "uretprobe"] {
        let c = Context::new();
        let id = load(&c, kind);
        let pid = std::process::id().to_string();
        // No host cache is required: the mapped libc wins first.
        for query in ["libc", "libc.so", "libc.so.6"] {
            let attached = c.json(
                &rust(),
                &[
                    "link",
                    "attach",
                    "uprobe",
                    &id,
                    query,
                    "--fn-name",
                    "getpid",
                    "--pid",
                    &pid,
                    "-o",
                    "json",
                ],
            );
            assert_eq!(attached["record"]["details"]["target"], query);
            let link = attached["record"]["id"]
                .as_u64()
                .expect("link id")
                .to_string();
            c.run(&rust(), &["link", "detach", &link], true);
        }
        let temp = tempfile::tempdir().expect("cache fixture");
        let maps = fs::read_to_string("/proc/self/maps").expect("maps");
        let libc = maps
            .lines()
            .find_map(|line| {
                line.split_whitespace()
                    .last()
                    .filter(|path| path.ends_with("/libc.so.6"))
            })
            .expect("loaded libc");
        let cache_file = temp.path().join("cache");
        fs::write(&cache_file, cache("libbpfmanhost.so.1", Path::new(libc)))
            .expect("synthetic cache");
        for query in ["libbpfmanhost", "libbpfmanhost.so", "libbpfmanhost.so.1"] {
            let output = host_cache(
                &c,
                &cache_file,
                &[
                    "link",
                    "attach",
                    "uprobe",
                    &id,
                    query,
                    "--fn-name",
                    "getpid",
                    "-o",
                    "json",
                ],
                true,
            );
            let attached: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
            assert_eq!(attached["record"]["details"]["target"], query);
            let link = attached["record"]["id"]
                .as_u64()
                .expect("link id")
                .to_string();
            assert!(
                c.json(&rust(), &["link", "get", &link, "-o", "json"])["status"]["pin_present"]
                    .as_bool()
                    .expect("persisted link")
            );
            c.run(&rust(), &["link", "detach", &link], true);
        }
        // Readable maps with no match fall through to the cache.
        let output = host_cache(
            &c,
            &cache_file,
            &[
                "link",
                "attach",
                "uprobe",
                &id,
                "libbpfmanhost",
                "--fn-name",
                "getpid",
                "--pid",
                &pid,
                "-o",
                "json",
            ],
            true,
        );
        let attached: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
        let link = attached["record"]["id"]
            .as_u64()
            .expect("link id")
            .to_string();
        c.run(&rust(), &["link", "detach", &link], true);
        // A maps read error is terminal even though this cache could answer.
        let output = host_cache(
            &c,
            &cache_file,
            &[
                "link",
                "attach",
                "uprobe",
                &id,
                "libbpfmanhost",
                "--fn-name",
                "getpid",
                "--pid",
                "2147483647",
            ],
            false,
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("maps"));
        host_cache(
            &c,
            &cache_file,
            &[
                "link",
                "attach",
                "uprobe",
                &id,
                "libbpfmanmissing",
                "--fn-name",
                "getpid",
            ],
            false,
        );
        assert_eq!(
            c.json(&rust(), &["link", "list", "-o", "json"])["links"],
            serde_json::json!([])
        );
        c.run(&rust(), &["program", "unload", &id], true);
        c.no_artifacts();
    }
    pid_namespace_maps();
}

fn pid_namespace_maps() {
    let c = Context::new();
    let temp = tempfile::tempdir().expect("PID namespace fixture");
    use std::os::unix::ffi::OsStrExt;
    let native = temp
        .path()
        .join(std::ffi::OsStr::from_bytes(b"native-\xff"));
    fs::create_dir(&native).expect("native path directory");
    let alias = native.join("libbpfmanpidns.so.1");
    fs::write(&alias, "host alias is not ELF").expect("host alias");
    let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bin/bpfman-shell");
    let mut worker = Worker(Command::new("unshare").args(["--mount", "--pid", "--fork", "--kill-child=KILL", "--mount-proc", "--", "sh", "-c", "mount --make-rprivate / && mount -t tmpfs tmpfs /etc && mount --bind \"$1\" \"$2\" && printf 'ready\\n' && BPFMAN_SHELL_MODE=uprobe-fire-worker exec \"$2\" \"$3/sentinel\" \"$3/ack\" 1 1", "pidns-worker"]).arg(shell).arg(&alias).arg(temp.path()).stdout(Stdio::piped()).spawn().expect("PID namespace worker"));
    let mut ready = String::new();
    BufReader::new(worker.0.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .expect("ready");
    assert_eq!(ready, "ready\n");
    let launcher = worker.0.id();
    let children = fs::read_to_string(format!("/proc/{launcher}/task/{launcher}/children"))
        .expect("host children");
    let pid = children.split_whitespace().next().expect("host PID");
    // Wait for exec, rather than racing sh's pre-exec readiness marker.
    let started = std::time::Instant::now();
    loop {
        let maps = fs::read(format!("/proc/{pid}/maps")).expect("host PID maps");
        if bpfman_core::uprobe_library_from_maps(&maps, "libbpfmanpidns").is_some() {
            break;
        }
        assert!(started.elapsed() < TIMEOUT, "worker exec");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    for kind in ["uprobe", "uretprobe"] {
        let id = load(&c, kind);
        let attached = c.json(
            &rust(),
            &[
                "link",
                "attach",
                "uprobe",
                &id,
                "libbpfmanpidns",
                "--fn-name",
                "bpfman_shell_uprobe_call_malloc",
                "--pid",
                pid,
                "--container-pid",
                pid,
                "-o",
                "json",
            ],
        );
        assert_eq!(attached["record"]["details"]["target"], "libbpfmanpidns");
        let link = attached["record"]["id"]
            .as_u64()
            .expect("link id")
            .to_string();
        c.run(&rust(), &["link", "detach", &link], true);
        // Without a process filter this namespace has no cache to answer.
        // A failed child-side lookup must leave no pending intent or link pin.
        let failure = c.run(
            &rust(),
            &[
                "link",
                "attach",
                "uprobe",
                &id,
                "libbpfmanpidns",
                "--fn-name",
                "bpfman_shell_uprobe_call_malloc",
                "--container-pid",
                pid,
            ],
            false,
        );
        assert!(String::from_utf8_lossy(&failure.stderr).contains("ld.so.cache"));
        assert_eq!(
            c.json(&rust(), &["link", "list", "-o", "json"])["links"],
            serde_json::json!([])
        );
        c.run(&rust(), &["program", "unload", &id], true);
    }
    drop(worker);
    c.no_artifacts();
}

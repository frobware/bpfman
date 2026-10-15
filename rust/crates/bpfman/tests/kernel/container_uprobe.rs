//! A namespace-only bind target discriminates helper lookup from host lookup.
use super::support::*;
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};

struct Namespace(Child);

impl Drop for Namespace {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn exercise() {
    for kind in ["uprobe", "uretprobe"] {
        let c = Context::new();
        let temp = tempfile::tempdir().expect("namespace fixture");
        let target = temp.path().join("target");
        fs::write(&target, "host target is deliberately not ELF").expect("host target");
        let maps = fs::read_to_string("/proc/self/maps").expect("maps");
        let libc = maps
            .lines()
            .find_map(|line| {
                line.split_whitespace()
                    .last()
                    .filter(|path| path.ends_with("/libc.so.6"))
            })
            .expect("loaded libc");
        let mut process = Namespace(Command::new("unshare").args(["--mount", "--", "sh", "-c", "mount --make-rprivate / && mount --bind \"$1\" \"$2\" && printf 'ready\\n' && exec sleep 60", "namespace-worker", libc, target.to_str().expect("path")]).stdout(Stdio::piped()).spawn().expect("namespace process"));
        let mut ready = String::new();
        BufReader::new(process.0.stdout.take().expect("stdout"))
            .read_line(&mut ready)
            .expect("readiness");
        assert_eq!(ready, "ready\n");
        let container_pid = process.0.id().to_string();
        assert_eq!(
            fs::read_to_string(&target).expect("host spelling"),
            "host target is deliberately not ELF"
        );
        let binary = rust();
        let loaded = c.json(
            &binary,
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
        );
        let program = loaded["programs"][0]["record"]["program_id"]
            .as_u64()
            .expect("program id")
            .to_string();
        // Host lookup and a symbol-resolution failure after successful handoff
        // both compensate completely. A vanished namespace also leaves no intent.
        c.run(
            &binary,
            &[
                "link",
                "attach",
                "uprobe",
                &program,
                target.to_str().expect("path"),
                "--fn-name",
                "getpid",
            ],
            false,
        );
        c.run(
            &binary,
            &[
                "link",
                "attach",
                "uprobe",
                &program,
                target.to_str().expect("path"),
                "--fn-name",
                "bpfman_missing_symbol",
                "--container-pid",
                &container_pid,
            ],
            false,
        );
        assert_eq!(
            c.json(&binary, &["link", "list", "-o", "json"])["links"],
            serde_json::json!([])
        );
        let attached = c.json(
            &binary,
            &[
                "link",
                "attach",
                "uprobe",
                &program,
                target.to_str().expect("path"),
                "--fn-name",
                "getpid",
                "--container-pid",
                &container_pid,
                "-o",
                "json",
            ],
        );
        let link = attached["record"]["id"]
            .as_u64()
            .expect("link id")
            .to_string();
        // The attach CLI and its helper have exited; the pinned link survives.
        let reopened = c.json(&binary, &["link", "get", &link, "-o", "json"]);
        assert_eq!(
            reopened["record"]["details"]["container_pid"],
            container_pid.parse::<u32>().expect("pid")
        );
        assert!(reopened["status"]["pin_present"].as_bool().expect("pinned"));
        drop(process);
        // Teardown never needs the namespace or target again.
        c.run(&binary, &["link", "detach", &link], true);
        c.run(
            &binary,
            &[
                "link",
                "attach",
                "uprobe",
                &program,
                target.to_str().expect("path"),
                "--fn-name",
                "getpid",
                "--container-pid",
                &container_pid,
            ],
            false,
        );
        assert_eq!(
            c.json(&binary, &["link", "list", "-o", "json"])["links"],
            serde_json::json!([])
        );
        c.run(&binary, &["program", "unload", &program], true);
        c.no_artifacts();
    }
}

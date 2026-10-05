//! SQLite-specific interchange with Go. Sharing persisted state with Go is
//! separate from the behavioural contract required of every store backend.

use super::support::*;
use std::path::PathBuf;

fn go() -> PathBuf {
    std::env::var_os("BPFMAN_GO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/bpfman"))
}

pub(super) fn exercise() {
    let c = Context::new();

    for (loader, unloader) in [(rust(), go()), (go(), rust())] {
        let pid = id(&c.load_cli(&loader));
        let pid_text = pid.to_string();
        c.present(pid);

        assert_eq!(
            c.json(&rust(), &["program", "get", &pid_text, "-o", "json"]),
            c.json(&go(), &["program", "get", &pid_text, "-o", "json"])
        );
        assert_eq!(
            c.json(&rust(), &["program", "list", "-o", "json"]),
            c.json(&go(), &["program", "list", "-o", "json"])
        );
        assert_eq!(
            c.run(&rust(), &["program", "get", &pid_text], true).stdout,
            c.run(&go(), &["program", "get", &pid_text], true).stdout
        );
        // A program loaded and attached by either implementation remains fully
        // observable and can be unloaded directly by the other implementation.
        let attached = c.json(
            &loader,
            &[
                "link",
                "attach",
                "tracepoint",
                &pid_text,
                "syscalls/sys_enter_kill",
                "-m",
                "test=attached-interchange",
                "-o",
                "json",
            ],
        );
        assert_eq!(
            c.json(&rust(), &["program", "get", &pid_text, "-o", "json"]),
            c.json(&go(), &["program", "get", &pid_text, "-o", "json"])
        );
        assert_eq!(
            c.run(&rust(), &["program", "get", &pid_text], true).stdout,
            c.run(&go(), &["program", "get", &pid_text], true).stdout
        );
        c.run(&unloader, &["program", "unload", &pid_text], true);
        c.run(
            &unloader,
            &["link", "get", &attached["record"]["id"].to_string()],
            false,
        );
        c.absent(pid);
        assert_eq!(
            c.json(&rust(), &["program", "list", "-o", "json"])["programs"],
            serde_json::json!([])
        );
        c.no_artifacts();
    }

    for (loader, unloader) in [(rust(), go()), (go(), rust())] {
        let loaded = c.json(
            &loader,
            &[
                "program",
                "load",
                "file",
                fixture("xdp_pass.bpf.o").to_str().expect("path"),
                "--programs",
                "xdp:pass",
                "-m",
                "test=xdp-interchange",
                "-o",
                "json",
            ],
        );
        let pid = id(&loaded["programs"][0]);
        let pid_text = pid.to_string();
        assert_eq!(
            c.json(&rust(), &["program", "get", &pid_text, "-o", "json"]),
            c.json(&go(), &["program", "get", &pid_text, "-o", "json"])
        );
        assert_eq!(
            c.json(&rust(), &["program", "list", "-o", "json"]),
            c.json(&go(), &["program", "list", "-o", "json"])
        );
        assert_eq!(
            c.run(&rust(), &["program", "get", &pid_text], true).stdout,
            c.run(&go(), &["program", "get", &pid_text], true).stdout
        );
        c.run(&unloader, &["program", "unload", &pid_text], true);
        c.absent(pid);
        c.no_artifacts();
    }
}

//! Public runtime replacement with real packet counters and atomic store failures.
#![allow(clippy::panic)]
use super::{
    faults::{Faults, Point},
    support::*,
    xdp_attach::Interface,
    xdp_switch::traffic,
};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;

pub(super) fn exercise<S>(backend: S, selection: &'static str)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for (mode, keep_first) in [
        (XdpMode::Drv, false),
        (XdpMode::Drv, true),
        (XdpMode::Skb, false),
        (XdpMode::Skb, true),
        (XdpMode::Hw, false),
        (XdpMode::Hw, true),
    ] {
        let c = Context::with_store(selection);
        let interface = Interface::new();
        let faults = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("store"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        let load = || {
            app.load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel,
                    &fixture("xdp_counter.bpf.o"),
                    ProgramSpec::Xdp("xdp_stats".try_into().expect("symbol")),
                    Default::default(),
                )
                .expect("prepare"),
            )
            .expect("load")
            .record
            .id
        };
        let a = load();
        let b = load();
        let request = |id, proceed_on| XdpAttach {
            netns: Default::default(),
            mode,
            program_id: id,
            interface: interface.name(),
            priority: 50,
            proceed_on,
            metadata: Default::default(),
        };
        let config = tempfile::NamedTempFile::new().expect("config");
        std::fs::write(
            config.path(),
            format!(
                "[interfaces.{}]\nxdp_mode = \"{}\"\n",
                interface.name().as_str(),
                mode.as_str()
            ),
        )
        .expect("write config");
        c.json(
            &rust(),
            &[
                "--config",
                config.path().to_str().expect("config path"),
                "link",
                "attach",
                "xdp",
                &a.to_string(),
                interface.name().as_str(),
                "--priority",
                "50",
                "-o",
                "json",
            ],
        );
        let first = app
            .list_link_records()
            .expect("links")
            .pop()
            .expect("first");
        let LinkDetails::Xdp(details) = &first.details else {
            panic!("XDP");
        };
        let old = app.get_xdp_dispatcher(details.key).expect("old");
        let output = std::process::Command::new("ip")
            .args(["-j", "-d", "link", "show", "dev", interface.name().as_str()])
            .output()
            .expect("interface mode");
        assert!(output.status.success());
        let links: serde_json::Value = serde_json::from_slice(&output.stdout).expect("link JSON");
        // Veth has native and generic XDP, but cannot offload to hardware.
        // Linux IFLA_XDP_ATTACHED modes: native driver = 1, generic SKB = 2.
        let expected = if mode == XdpMode::Drv { 1 } else { 2 };
        assert_eq!(
            links[0]["xdp"]["attached"][0]["mode"], expected,
            "requested {mode:?}"
        );
        faults.set(Some(Point::XdpReplace));
        let error = app
            .attach_xdp(request(b, Default::default()))
            .expect_err("failed publication");
        assert_eq!(error.unresolved(), 0);
        assert_eq!(error.restoration_attempts().len(), 1);
        assert!(error.restoration_attempts()[0].is_ok());
        assert_eq!(app.get_xdp_dispatcher(details.key).expect("restored"), old);
        traffic(&c, &[a], &[b]);
        faults.set(None);
        // New members win same-priority ties, and a zero mask must stop the chain.
        let stopped = app
            .attach_xdp(request(b, 0.try_into().expect("mask")))
            .expect("stopping member");
        traffic(&c, &[b], &[a]);
        app.detach_xdp(stopped.id).expect("remove stopping member");
        let second = app
            .attach_xdp(request(b, Default::default()))
            .expect("second");
        let chain = app.get_xdp_dispatcher(details.key).expect("chain");
        assert_eq!(
            chain
                .members()
                .iter()
                .map(|m| m.member.id)
                .collect::<Vec<_>>(),
            [second.id, first.id]
        );
        assert_eq!(
            chain.members()[0].outer_link_id,
            old.members()[0].outer_link_id
        );
        traffic(&c, &[a, b], &[]);
        let (removed, survivor, active, inactive) = if keep_first {
            (second.id, first.id, a, b)
        } else {
            (first.id, second.id, b, a)
        };
        faults.set(Some(Point::XdpReplace));
        let error = app
            .detach_xdp(removed)
            .expect_err("failed detach publication");
        assert_eq!(error.unresolved(), 0);
        assert_eq!(error.restoration_attempts().len(), 1);
        assert_eq!(
            app.get_xdp_dispatcher(details.key).expect("old chain"),
            chain
        );
        traffic(&c, &[a, b], &[]);
        faults.set(None);
        app.detach_xdp(removed).expect("detach member");
        let remaining = app.get_xdp_dispatcher(details.key).expect("survivor");
        assert_eq!(remaining.members().len(), 1);
        assert_eq!(remaining.members()[0].member.id, survivor);
        assert_eq!(
            remaining.members()[0].outer_link_id,
            old.members()[0].outer_link_id
        );
        traffic(&c, &[active], &[inactive]);
        app.detach_xdp(survivor).expect("last detach");
        assert!(app.get_xdp_dispatcher(details.key).is_err());
        for id in [a, b] {
            assert_eq!(app.unload(id).expect("unload").unresolved(), 0);
        }
    }
}

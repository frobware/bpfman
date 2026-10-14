//! Missing selector admission, retained orphan receipts and reopened unload recovery.
#![allow(clippy::panic)]
use super::{
    faults::{Faults, Point},
    support::*,
    tc::{count, request, traffic},
    xdp_delivery::Network,
};
use bpfman_kernel::{TcLifecycle, TcRemoval};
use bpfman_model::{LinkDetails, ProgramSpec, TcDirection};
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram};
use bpfman_store::*;
use std::{fs, os::unix::fs::symlink};

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + TcStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for (hook, detach_first) in [
        (TcDirection::Ingress, false),
        (TcDirection::Egress, false),
        (TcDirection::Ingress, true),
        (TcDirection::Egress, true),
    ] {
        let c = Context::new();
        let original = Network::new();
        let other = Network::new();
        let aliases = tempfile::tempdir().expect("namespace aliases");
        let alias = aliases.path().join("selected");
        let saved = aliases.path().join("saved");
        let replacement = aliases.path().join("replacement");
        symlink(original.namespace().as_str(), &alias).expect("namespace selector");
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
                    &fixture("tc_ingress.bpf.o"),
                    ProgramSpec::Tc("tc_ingress".try_into().expect("symbol")),
                    Default::default(),
                )
                .expect("prepare"),
            )
            .expect("load")
            .record
            .id
        };
        let id = load();
        let other_id = detach_first.then(load);
        let mut links = Vec::new();
        for direction in [hook, hook, hook.opposite(), hook.opposite()] {
            let mut r = request(id, &original, direction);
            // Both hooks on one interface; the alias alone is the stored selector.
            r.interface = "in0".parse().expect("interface");
            r.netns = alias
                .to_str()
                .expect("selector path")
                .parse()
                .expect("selector");
            links.push(app.attach_tc(r).expect("repeated orphan members").id);
        }
        if let Some(other_id) = other_id {
            let mut r = request(other_id, &original, hook);
            r.interface = "in0".parse().expect("interface");
            r.netns = alias
                .to_str()
                .expect("selector path")
                .parse()
                .expect("selector");
            app.attach_tc(r)
                .expect("another program in orphan dispatcher");
        }
        app.attach_tc(request(id, &other, hook))
            .expect("independent live scope");
        let before = app.list_link_records().expect("complete membership");
        let snapshots = app.list_tc_dispatchers().expect("snapshots");
        let snapshot = snapshots
            .iter()
            .find(|s| s.members().iter().any(|m| m.member.id == links[0]))
            .expect("orphan snapshot");

        fs::rename(&alias, &saved).expect("hide original selector");
        symlink(other.namespace().as_str(), &alias).expect("rebound selector");
        assert!(
            app.detach_tc(links[0]).is_err(),
            "changed namespace is not absence"
        );
        assert_eq!(app.list_link_records().expect("unchanged"), before);
        assert!(
            before
                .iter()
                .all(|m| std::path::Path::new(&m.pin_path).exists())
        );
        fs::rename(&alias, &replacement).expect("hide replacement");

        let kernel = bpfman_kernel_aya::Kernel;
        let orphan = c.writer(|w| {
            match kernel
                .observe_tc_removal(w, snapshot)
                .expect("observe orphan")
            {
                TcRemoval::Orphan(orphan) => orphan,
                TcRemoval::Live(_, _) => panic!("absent selector must yield orphan pins"),
            }
        });
        let foreign = Context::new();
        let failure = foreign
            .writer(|w| kernel.remove_tc_orphan(w, orphan))
            .expect_err("wrong runtime");
        fs::rename(&replacement, &alias).expect("selector reappears after admission");
        let failure = c
            .writer(|w| kernel.remove_tc_orphan(w, failure.remaining))
            .expect_err("reappeared selector retains all ownership");
        assert!(
            before
                .iter()
                .all(|m| std::path::Path::new(&m.pin_path).exists())
        );
        fs::rename(&alias, &replacement).expect("hide replacement again");
        drop(failure.remaining); // Observation receipts have no implicit destructive effects.

        let LinkDetails::Tc(d) = &snapshot.members()[0].member.details else {
            panic!("TC")
        };
        let marker = c.layout.tc_ownership_path(d.key, d.revision);
        let evidence = fs::read(&marker).expect("ownership evidence");
        fs::write(&marker, [9]).expect("corrupt orphan ownership");
        assert!(
            app.unload(id)
                .expect_err("all scopes preflight before effects")
                .report()
                .is_none()
        );
        assert_eq!(app.list_link_records().expect("unchanged intent"), before);
        assert!(
            before
                .iter()
                .all(|m| std::path::Path::new(&m.pin_path).exists())
        );
        fs::write(&marker, evidence).expect("restore evidence");

        if let Some(other_id) = other_id {
            app.detach_tc(links[0])
                .expect("orphan detach removes complete selected hook");
            assert!(app.get_tc_dispatcher(d.key).is_err());
            assert_eq!(
                app.list_link_records().expect("other scopes survive").len(),
                3
            );
            for untouched in snapshots
                .iter()
                .filter(|s| s.members()[0].details.key != d.key)
            {
                assert_eq!(
                    app.get_tc_dispatcher(untouched.members()[0].details.key)
                        .expect("other hook and live namespace unchanged"),
                    *untouched
                );
            }
            assert!(
                app.get(other_id)
                    .expect("other program retained")
                    .links
                    .is_empty()
            );
            assert_eq!(
                app.unload(other_id)
                    .expect("unload other program")
                    .unresolved(),
                0
            );
            assert_program_absent(other_id);
        }

        // Store failure retains each scope's conditional deletion after all owned
        // pins are removed. Other hooks and live namespaces still get their pass.
        faults.set(Some(Point::TcDelete));
        let error = app.unload(id).expect_err("record publication failure");
        let report = error.report().expect("retained prerequisites");
        assert!(report.attempts().is_empty());
        let scopes = if detach_first { 2 } else { 3 };
        assert_eq!(report.tc_attempts().len(), scopes);
        assert!(
            report
                .tc_attempts()
                .iter()
                .all(|a| a.outcome().is_err_and(|e| e.unresolved() == 1))
        );
        assert!(
            before
                .iter()
                .all(|m| !std::path::Path::new(&m.pin_path).exists())
        );
        let counted = count(&c, id);
        traffic(&other, 3, hook);
        assert_eq!(
            count(&c, id),
            counted,
            "independent live scope stopped execution"
        );
        drop(app);
        faults.set(None);
        let app = Bpfman::new(
            ActiveStore::open(faults, &c.layout, TIMEOUT).expect("reopen"),
            kernel,
            TIMEOUT,
        );
        let report = app
            .retry_unload(error)
            .expect("finish complete snapshots without re-adoption");
        assert_eq!(report.unresolved(), 0);
        assert_eq!(
            report.tc_attempts().len(),
            scopes,
            "repeated members consumed once per scope"
        );
        c.absent(id);
        c.no_artifacts();
        // The original namespace still exists under another name. Orphan cleanup
        // released managed freplace references, without claiming namespace destruction.
        assert_program_absent(id);
    }
}

use super::*;
use std::num::{NonZeroU32, NonZeroU64};

fn key(kind: &str, nsid: u64, ifindex: u32) -> DispatcherKey {
    let nsid = NonZeroU64::new(nsid).expect("nsid");
    let ifindex = NonZeroU32::new(ifindex).expect("ifindex");
    if kind == "xdp" {
        DispatcherKey::Xdp(bpfman_model::XdpKey { nsid, ifindex })
    } else {
        DispatcherKey::Tc(bpfman_model::TcKey {
            nsid,
            ifindex,
            direction: if kind == "tc-ingress" {
                bpfman_model::TcDirection::Ingress
            } else {
                bpfman_model::TcDirection::Egress
            },
        })
    }
}

fn seed() -> rusqlite::Result<rusqlite::Connection> {
    let c = crate::test_support::database()?;
    crate::test_support::seed(&c)?;
    c.execute_batch("PRAGMA foreign_keys=ON; DELETE FROM links;")?;
    for (i, (kind, nsid, ifindex)) in [
        ("xdp", 1, 7),
        ("tc-ingress", 1, 7),
        ("tc-egress", 1, 7),
        ("xdp", 2, 7),
        ("xdp", 1, 8),
    ]
    .into_iter()
    .enumerate()
    {
        let dispatcher = i as u32 + 100;
        c.execute("INSERT INTO dispatchers(type,nsid,ifindex,program_id,created_at,updated_at) VALUES(?1,?2,?3,?4,'now','now')", (kind, nsid, ifindex, dispatcher))?;
        for slot in 0..2 {
            let id = i as u32 * 2 + slot + 10;
            c.execute(
                "INSERT INTO links(id,kind,kernel_prog_id,created_at) VALUES(?1,?2,42,'now')",
                (id, if kind == "xdp" { "xdp" } else { "tc" }),
            )?;
            if kind == "xdp" {
                c.execute("INSERT INTO link_xdp_details(id,interface,ifindex,priority,position,proceed_on,nsid,dispatcher_program_id) VALUES(?1,'fake0',?2,50,?3,'[]',?4,?5)", (id, ifindex, slot, nsid, dispatcher))?;
            } else {
                c.execute("INSERT INTO link_tc_details(id,interface,ifindex,direction,priority,position,proceed_on,nsid,dispatcher_program_id) VALUES(?1,'fake0',?2,?3,50,?4,'[]',?5,?6)", (id, ifindex, if kind == "tc-ingress" { "ingress" } else { "egress" }, slot, nsid, dispatcher))?;
            }
        }
    }
    Ok(c)
}

fn counts(c: &rusqlite::Connection) -> rusqlite::Result<(u32, u32, u32, u32, u32)> {
    c.query_row("SELECT (SELECT count(*) FROM dispatchers), (SELECT count(*) FROM links), (SELECT count(*) FROM link_xdp_details)+(SELECT count(*) FROM link_tc_details), (SELECT count(*) FROM managed_programs), (SELECT count(*) FROM map_sets)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
}

#[test]
fn atomic_deletion_is_scoped_to_full_key_and_preserves_programs() -> rusqlite::Result<()> {
    let mut c = seed()?;
    for (i, (kind, nsid, ifindex)) in [
        ("tc-ingress", 1, 7),
        ("xdp", 1, 7),
        ("tc-egress", 1, 7),
        ("xdp", 2, 7),
        ("xdp", 1, 8),
    ]
    .into_iter()
    .enumerate()
    {
        let key = key(kind, nsid, ifindex);
        let tx = c.transaction()?;
        assert!(delete(&tx, key)?);
        tx.commit()?;
        assert_eq!(
            counts(&c)?,
            (4 - i as u32, 8 - i as u32 * 2, 8 - i as u32 * 2, 2, 1)
        );
        let tx = c.transaction()?;
        assert!(!delete(&tx, key)?);
        // A missing result is rolled back by the adapter.
        drop(tx);
    }
    Ok(())
}

#[test]
fn statement_failures_and_ignored_deletes_roll_back_all_members() -> rusqlite::Result<()> {
    for kind in ["xdp", "tc-ingress", "tc-egress"] {
        for table in [
            "links",
            "dispatchers",
            if kind == "xdp" {
                "link_xdp_details"
            } else {
                "link_tc_details"
            },
        ] {
            for action in ["ABORT", "IGNORE"] {
                let mut c = seed()?;
                let before = counts(&c)?;
                c.execute_batch(&format!("CREATE TRIGGER reject_delete BEFORE DELETE ON {table} BEGIN SELECT RAISE({action}{}); END", if action == "ABORT" { ", 'injected'" } else { "" }))?;
                let tx = c.transaction()?;
                assert!(
                    delete(&tx, key(kind, 1, 7)).is_err(),
                    "{kind} {table} {action}"
                );
                drop(tx);
                assert_eq!(counts(&c)?, before);
                c.execute_batch("DROP TRIGGER reject_delete")?;
                let tx = c.transaction()?;
                assert!(delete(&tx, key(kind, 1, 7))?);
                tx.commit()?;
                assert_eq!(counts(&c)?, (4, 8, 8, 2, 1));
            }
        }
    }
    Ok(())
}

#[test]
fn absent_header_preserves_orphan_members_and_empty_headers_are_deleted() -> rusqlite::Result<()> {
    let mut c = seed()?;
    c.execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM dispatchers WHERE type='xdp' AND nsid=1 AND ifindex=7; PRAGMA foreign_keys=ON;")?;
    let before = counts(&c)?;
    let tx = c.transaction()?;
    assert!(!delete(&tx, key("xdp", 1, 7))?);
    drop(tx);
    assert_eq!(counts(&c)?, before);
    c.execute_batch("DELETE FROM links WHERE id IN(12,13)")?;
    let tx = c.transaction()?;
    assert!(delete(&tx, key("tc-ingress", 1, 7))?);
    tx.commit()?;
    assert_eq!(counts(&c)?, (3, 8, 8, 2, 1));
    Ok(())
}

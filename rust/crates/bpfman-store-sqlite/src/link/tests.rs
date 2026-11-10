use super::*;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn targeted_dispatcher_reads_validate_siblings_before_selecting_program_members() -> Result {
    for (kind, direction) in [("xdp", ""), ("tc", "ingress"), ("tc", "egress")] {
        let c = crate::test_support::database()?;
        crate::test_support::seed(&c)?;
        c.execute_batch("DELETE FROM links")?;
        c.execute("UPDATE managed_programs SET program_type=?1", [kind])?;
        let dispatcher_type = if kind == "xdp" {
            "xdp".into()
        } else {
            format!("tc-{direction}")
        };
        c.execute("INSERT INTO dispatchers(type,nsid,ifindex,program_id,kernel_link_id,priority,filter_handle,created_at,updated_at)
            VALUES(?1,1,7,100,200,50,1,'2026-10-04T12:00:00Z','2026-10-04T12:00:00Z')", [dispatcher_type])?;
        for (link, program, slot) in [(10, 42, 0), (11, 7, 1)] {
            c.execute(
                "INSERT INTO links(id,kind,kernel_prog_id,kernel_link_id,pin_path,created_at)
                VALUES(?1,?2,?3,?4,?5,'2026-10-04T12:00:00Z')",
                (
                    link,
                    kind,
                    program,
                    link + 300,
                    format!("/pins/link_{link}"),
                ),
            )?;
            if kind == "xdp" {
                c.execute("INSERT INTO link_xdp_details(id,interface,ifindex,priority,position,proceed_on,nsid,dispatcher_program_id)
                    VALUES(?1,'fake0',7,50,?2,'[2,31]',1,100)", (link, slot))?;
            } else {
                c.execute("INSERT INTO link_tc_details(id,interface,ifindex,direction,priority,position,proceed_on,nsid,dispatcher_program_id)
                    VALUES(?1,'fake0',7,?2,50,?3,'[3,30]',1,100)", (link, direction, slot))?;
            }
        }
        let id = NonZeroU32::new(42).expect("program");
        let selected = read_by_program(&c, id)?;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id.get(), 10);
        assert_eq!(
            read_by_program(&c, NonZeroU32::new(7).expect("program"))?[0]
                .id
                .get(),
            11
        );

        c.execute("UPDATE links SET metadata_json='broken' WHERE id=11", [])?;
        assert!(
            read_by_program(&c, id).is_err(),
            "corrupt sibling in {kind}/{direction}"
        );
        c.execute("UPDATE links SET metadata_json='{}' WHERE id=11", [])?;
        assert_eq!(read_by_program(&c, id)?, selected);
        let table = if kind == "xdp" {
            "link_xdp_details"
        } else {
            "link_tc_details"
        };
        c.execute(&format!("UPDATE {table} SET position=3 WHERE id=11"), [])?;
        assert!(
            read_by_program(&c, id).is_err(),
            "noncontiguous sibling in {kind}/{direction}"
        );
        c.execute(&format!("UPDATE {table} SET position=1 WHERE id=11"), [])?;
        c.execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM dispatchers")?;
        assert!(
            read_by_program(&c, id).is_err(),
            "missing header in {kind}/{direction}"
        );
    }
    Ok(())
}

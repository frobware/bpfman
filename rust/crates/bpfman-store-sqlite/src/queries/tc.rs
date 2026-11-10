//! Fixed SQL for one complete TC dispatcher snapshot.

use bpfman_model::TcKey;
use bpfman_store::TcCommit;
use rusqlite::{Connection, Transaction, named_params};

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Row {
    pub id: i64,
    pub program: i64,
    pub program_kind: String,
    pub program_name: String,
    pub program_pin: String,
    pub kernel: i64,
    pub pin: String,
    pub metadata: String,
    pub created: String,
    pub interface: String,
    pub nsid: i64,
    pub ifindex: i64,
    pub priority: i64,
    pub position: i64,
    pub proceed_on: String,
    pub netns: String,
    pub dispatcher_netns: String,
    pub dispatcher: i64,
    pub revision: i64,
    pub filter_handle: i64,
    pub filter_priority: i64,
    pub direction: String,
    pub dispatcher_created: String,
    pub dispatcher_updated: String,
}

macro_rules! snapshot_sql {
    ($predicate:literal) => { concat!("SELECT l.id, l.kernel_prog_id AS program, p.program_type AS program_kind,
            p.program_name, p.pin_path AS program_pin, l.kernel_link_id AS kernel,
            l.pin_path AS pin, l.metadata_json AS metadata, l.created_at AS created,
            x.interface, x.nsid, x.ifindex, x.priority, x.position, x.proceed_on,
            COALESCE(x.netns,'') AS netns, COALESCE(d.netns,'') AS dispatcher_netns,
            x.dispatcher_program_id AS dispatcher, d.revision, d.filter_handle, d.priority AS filter_priority, x.direction,
            d.created_at AS dispatcher_created, d.updated_at AS dispatcher_updated
         FROM links l LEFT JOIN link_tc_details x ON x.id=l.id
         LEFT JOIN dispatchers d ON d.program_id=x.dispatcher_program_id
            AND d.type=('tc-' || x.direction) AND d.nsid=x.nsid AND d.ifindex=x.ifindex
         LEFT JOIN managed_programs p ON p.program_id=l.kernel_prog_id
         WHERE l.kind='tc' ", $predicate, " ORDER BY l.id") };
}

const ALL_ROWS: &str = snapshot_sql!("");
const ROWS_BY_KEY: &str =
    snapshot_sql!("AND x.nsid=:nsid AND x.ifindex=:ifindex AND x.direction=:direction");
const ROW_BY_LINK: &str = snapshot_sql!("AND l.id=:link");

pub(crate) fn rows(
    conn: &Connection,
    key: Option<TcKey>,
    link: Option<i64>,
) -> rusqlite::Result<Vec<Row>> {
    match (key, link) {
        (None, None) => query_rows(conn, ALL_ROWS, []),
        (Some(k), None) => query_rows(
            conn,
            ROWS_BY_KEY,
            named_params! { ":nsid": k.nsid.get(), ":ifindex": k.ifindex.get(), ":direction": k.direction.as_str() },
        ),
        (None, Some(id)) => query_rows(conn, ROW_BY_LINK, named_params! { ":link": id }),
        (Some(k), Some(id)) => query_rows(
            conn,
            snapshot_sql!(
                "AND x.nsid=:nsid AND x.ifindex=:ifindex AND x.direction=:direction AND l.id=:link"
            ),
            named_params! { ":nsid": k.nsid.get(), ":ifindex": k.ifindex.get(), ":direction": k.direction.as_str(), ":link": id },
        ),
    }
}

fn query_rows(
    conn: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> rusqlite::Result<Vec<Row>> {
    conn.prepare_cached(sql)?
        .query_map(parameters, |r| {
            Ok(Row {
                id: r.get("id")?,
                program: r.get("program")?,
                program_kind: r.get("program_kind")?,
                program_name: r.get("program_name")?,
                program_pin: r.get("program_pin")?,
                kernel: r.get("kernel")?,
                pin: r.get("pin")?,
                metadata: r.get("metadata")?,
                created: r.get("created")?,
                interface: r.get("interface")?,
                nsid: r.get("nsid")?,
                ifindex: r.get("ifindex")?,
                priority: r.get("priority")?,
                position: r.get("position")?,
                proceed_on: r.get("proceed_on")?,
                netns: r.get("netns")?,
                dispatcher_netns: r.get("dispatcher_netns")?,
                dispatcher: r.get("dispatcher")?,
                revision: r.get("revision")?,
                filter_handle: r.get("filter_handle")?,
                filter_priority: r.get("filter_priority")?,
                direction: r.get("direction")?,
                dispatcher_created: r.get("dispatcher_created")?,
                dispatcher_updated: r.get("dispatcher_updated")?,
            })
        })?
        .collect()
}

pub(crate) fn vacant(conn: &Connection, key: TcKey) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT NOT EXISTS(SELECT 1 FROM dispatchers
            WHERE type=:type AND nsid=:nsid AND ifindex=:ifindex)
         AND NOT EXISTS(SELECT 1 FROM link_tc_details
            WHERE nsid=:nsid AND ifindex=:ifindex AND direction=:direction) AS vacant",
    )?
    .query_row(
        named_params! { ":nsid": key.nsid.get(), ":ifindex": key.ifindex.get(), ":type": key.direction.dispatcher_type(), ":direction": key.direction.as_str() },
        |r| r.get("vacant"),
    )
}

pub(crate) fn is_tc(conn: &Connection, id: u32) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM managed_programs
            WHERE program_id=:id AND program_type='tc') AS present",
    )?
    .query_row(named_params! { ":id": id }, |r| r.get("present"))
}

pub(crate) fn insert(
    tx: &Transaction<'_>,
    request: &TcCommit<'_>,
    pin: &str,
    metadata: &str,
    actions: &str,
    existing: Option<i64>,
    header: bool,
) -> rusqlite::Result<i64> {
    let d = request.details;
    if header {
        tx.prepare_cached(
        "INSERT INTO dispatchers(type,nsid,ifindex,revision,program_id,priority,filter_handle,
            netns,created_at,updated_at)
         VALUES(:type,:nsid,:ifindex,:revision,:program,:filter_priority,:filter_handle,:netns,:created,:created)",
    )?
    .execute(named_params! {
        ":type": d.key.direction.dispatcher_type(),
        ":revision": d.revision.get(),
        ":nsid": d.key.nsid.get(),
        ":ifindex": d.key.ifindex.get(),
        ":program": d.dispatcher_id.get(),
        ":filter_priority": d.filter_priority,
        ":filter_handle": d.filter_handle.get(),
        ":netns": d.netns.as_str(),
        ":created": request.created_at,
    })?;
    }
    tx.prepare_cached(
        "INSERT INTO links(id,kind,kernel_prog_id,kernel_link_id,pin_path,metadata_json,created_at)
         VALUES(:id,'tc',:program,:kernel,:pin,:metadata,:created)",
    )?
    .execute(named_params! {
        ":id": existing,
        ":program": request.program_id.get(),
        ":kernel": request.extension_link_id.get(),
        ":pin": pin,
        ":metadata": metadata,
        ":created": request.created_at,
    })?;
    let id = tx.last_insert_rowid();

    tx.prepare_cached(
        "INSERT INTO link_tc_details(id,interface,ifindex,direction,priority,position,proceed_on,
            netns,nsid,dispatcher_program_id)
         VALUES(:id,:interface,:ifindex,:direction,:priority,:position,:actions,:netns,:nsid,:dispatcher)",
    )?
    .execute(named_params! {
        ":id": id,
        ":direction": d.key.direction.as_str(),
        ":interface": d.interface.as_str(),
        ":ifindex": d.key.ifindex.get(),
        ":priority": d.priority,
        ":position": d.slot.index(),
        ":actions": actions,
        ":netns": d.netns.as_str(),
        ":nsid": d.key.nsid.get(),
        ":dispatcher": d.dispatcher_id.get(),
    })?;

    Ok(id)
}

pub(crate) fn remove_member(tx: &Transaction<'_>, id: i64) -> rusqlite::Result<()> {
    let links = tx
        .prepare_cached("DELETE FROM links WHERE id=:id")?
        .execute(named_params! { ":id": id })?;
    if links != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

pub(crate) fn remove_dispatcher(tx: &Transaction<'_>, row: &Row) -> rusqlite::Result<()> {
    let dispatchers = tx
        .prepare_cached(
            "DELETE FROM dispatchers WHERE type=:type AND nsid=:nsid
            AND ifindex=:ifindex AND program_id=:program",
        )?
        .execute(named_params! {
            ":type": format!("tc-{}", row.direction),
            ":nsid": row.nsid,
            ":ifindex": row.ifindex,
            ":program": row.dispatcher,
        })?;
    if dispatchers != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }

    Ok(())
}

pub(crate) fn updated(tx: &Transaction<'_>, key: TcKey, timestamp: &str) -> rusqlite::Result<()> {
    tx.prepare_cached("UPDATE dispatchers SET updated_at=:updated WHERE type=:type AND nsid=:nsid AND ifindex=:ifindex")?.execute(named_params! { ":updated": timestamp, ":type": key.direction.dispatcher_type(), ":nsid": key.nsid.get(), ":ifindex": key.ifindex.get() })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_point_selection_uses_the_existing_index() -> rusqlite::Result<()> {
        let c = crate::test_support::database()?;
        let plan = c
            .prepare(&format!("EXPLAIN QUERY PLAN {ROWS_BY_KEY}"))?
            .query_map(
                named_params! { ":nsid": 1, ":ifindex": 7, ":direction": "ingress" },
                |row| row.get::<_, String>("detail"),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(
            plan.iter()
                .any(|line| line.contains("idx_link_tc_by_attach_point")),
            "{plan:?}"
        );
        Ok(())
    }
}

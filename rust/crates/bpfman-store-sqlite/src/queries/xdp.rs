//! Fixed SQL for one complete XDP dispatcher snapshot.

use bpfman_model::XdpKey;
use bpfman_store::XdpCommit;
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
    pub outer: i64,
    pub dispatcher_created: String,
    pub dispatcher_updated: String,
}

pub(crate) fn rows(
    conn: &Connection,
    key: Option<XdpKey>,
    link: Option<i64>,
) -> rusqlite::Result<Vec<Row>> {
    let mut statement = conn.prepare_cached(
        "SELECT l.id, l.kernel_prog_id AS program, p.program_type AS program_kind,
            p.program_name, p.pin_path AS program_pin, l.kernel_link_id AS kernel,
            l.pin_path AS pin, l.metadata_json AS metadata, l.created_at AS created,
            x.interface, x.nsid, x.ifindex, x.priority, x.position, x.proceed_on,
            COALESCE(x.netns,'') AS netns, COALESCE(d.netns,'') AS dispatcher_netns,
            x.dispatcher_program_id AS dispatcher, d.revision, d.kernel_link_id AS outer,
            d.created_at AS dispatcher_created, d.updated_at AS dispatcher_updated
         FROM links l LEFT JOIN link_xdp_details x ON x.id=l.id
         LEFT JOIN dispatchers d ON d.program_id=x.dispatcher_program_id
            AND d.type='xdp' AND d.nsid=x.nsid AND d.ifindex=x.ifindex
         LEFT JOIN managed_programs p ON p.program_id=l.kernel_prog_id
         WHERE l.kind='xdp' AND (:nsid IS NULL OR x.nsid=:nsid)
            AND (:ifindex IS NULL OR x.ifindex=:ifindex)
            AND (:link IS NULL OR l.id=:link) ORDER BY l.id",
    )?;
    let parameters = named_params! {
        ":nsid": key.map(|k| k.nsid.get()),
        ":ifindex": key.map(|k| k.ifindex.get()),
        ":link": link,
    };

    statement
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
                outer: r.get("outer")?,
                dispatcher_created: r.get("dispatcher_created")?,
                dispatcher_updated: r.get("dispatcher_updated")?,
            })
        })?
        .collect()
}

pub(crate) fn vacant(conn: &Connection, key: XdpKey) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT NOT EXISTS(SELECT 1 FROM dispatchers
            WHERE type='xdp' AND nsid=:nsid AND ifindex=:ifindex)
         AND NOT EXISTS(SELECT 1 FROM link_xdp_details
            WHERE nsid=:nsid AND ifindex=:ifindex) AS vacant",
    )?
    .query_row(
        named_params! { ":nsid": key.nsid.get(), ":ifindex": key.ifindex.get() },
        |r| r.get("vacant"),
    )
}

pub(crate) fn is_xdp(conn: &Connection, id: u32) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM managed_programs
            WHERE program_id=:id AND program_type='xdp') AS present",
    )?
    .query_row(named_params! { ":id": id }, |r| r.get("present"))
}

pub(crate) fn insert(
    tx: &Transaction<'_>,
    request: &XdpCommit<'_>,
    pin: &str,
    metadata: &str,
    actions: &str,
) -> rusqlite::Result<i64> {
    let d = request.details;
    tx.prepare_cached(
        "INSERT INTO dispatchers(type,nsid,ifindex,revision,program_id,kernel_link_id,
            netns,created_at,updated_at)
         VALUES('xdp',:nsid,:ifindex,:revision,:program,:outer,:netns,:created,:created)",
    )?
    .execute(named_params! {
        ":nsid": d.key.nsid.get(),
        ":ifindex": d.key.ifindex.get(),
        ":revision": d.revision.get(),
        ":program": d.dispatcher_id.get(),
        ":outer": request.outer_link_id.get(),
        ":netns": d.netns.as_str(),
        ":created": request.created_at,
    })?;
    tx.prepare_cached(
        "INSERT INTO links(kind,kernel_prog_id,kernel_link_id,pin_path,metadata_json,created_at)
         VALUES('xdp',:program,:kernel,:pin,:metadata,:created)",
    )?
    .execute(named_params! {
        ":program": request.program_id.get(),
        ":kernel": request.extension_link_id.get(),
        ":pin": pin,
        ":metadata": metadata,
        ":created": request.created_at,
    })?;
    let id = tx.last_insert_rowid();

    tx.prepare_cached(
        "INSERT INTO link_xdp_details(id,interface,ifindex,priority,position,proceed_on,
            netns,nsid,dispatcher_program_id)
         VALUES(:id,:interface,:ifindex,:priority,0,:actions,:netns,:nsid,:dispatcher)",
    )?
    .execute(named_params! {
        ":id": id,
        ":interface": d.interface.as_str(),
        ":ifindex": d.key.ifindex.get(),
        ":priority": d.priority,
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
            "DELETE FROM dispatchers WHERE type='xdp' AND nsid=:nsid
            AND ifindex=:ifindex AND program_id=:program",
        )?
        .execute(named_params! {
            ":nsid": row.nsid,
            ":ifindex": row.ifindex,
            ":program": row.dispatcher,
        })?;
    if dispatchers != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }

    Ok(())
}

pub(crate) fn program(c: &Connection, id: u32) -> rusqlite::Result<(String, String)> {
    c.prepare_cached(
        "SELECT program_name, pin_path FROM managed_programs
         WHERE program_id=:id AND program_type='xdp'",
    )?
    .query_row(named_params! { ":id": id }, |r| {
        Ok((r.get("program_name")?, r.get("pin_path")?))
    })
}

pub(crate) fn update_dispatcher(tx: &Transaction<'_>, row: &Row) -> rusqlite::Result<()> {
    let changed = tx
        .prepare_cached(
            "UPDATE dispatchers SET program_id=:program, revision=:revision, updated_at=:updated
         WHERE type='xdp' AND nsid=:nsid AND ifindex=:ifindex",
        )?
        .execute(named_params! {
            ":program": row.dispatcher,
            ":revision": row.revision,
            ":updated": row.dispatcher_updated,
            ":nsid": row.nsid,
            ":ifindex": row.ifindex,
        })?;

    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

pub(crate) fn insert_member(tx: &Transaction<'_>, row: &Row) -> rusqlite::Result<i64> {
    let changed = tx.prepare_cached(
        "INSERT INTO links(id,kind,kernel_prog_id,kernel_link_id,pin_path,metadata_json,created_at)
         VALUES(NULLIF(:id,0),'xdp',:program,:kernel,:pin,:metadata,:created)",
    )?
    .execute(named_params! {
        ":id": row.id,
        ":program": row.program,
        ":kernel": row.kernel,
        ":pin": row.pin,
        ":metadata": row.metadata,
        ":created": row.created,
    })?;

    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    let id = tx.last_insert_rowid();
    let changed = tx
        .prepare_cached(
            "INSERT INTO link_xdp_details(id,interface,ifindex,priority,position,proceed_on,
            netns,nsid,dispatcher_program_id)
         VALUES(:id,:interface,:ifindex,:priority,:position,:actions,:netns,:nsid,:dispatcher)",
        )?
        .execute(named_params! {
            ":id": id,
            ":interface": row.interface,
            ":ifindex": row.ifindex,
            ":priority": row.priority,
            ":position": row.position,
            ":actions": row.proceed_on,
            ":netns": row.netns,
            ":nsid": row.nsid,
            ":dispatcher": row.dispatcher,
        })?;

    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(id)
}

pub(crate) fn dispatcher_keys(conn: &Connection) -> rusqlite::Result<Vec<(String, i64, i64)>> {
    conn.prepare_cached("SELECT type, nsid, ifindex FROM dispatchers ORDER BY type, nsid, ifindex")?
        .query_map([], |r| {
            Ok((r.get("type")?, r.get("nsid")?, r.get("ifindex")?))
        })?
        .collect()
}

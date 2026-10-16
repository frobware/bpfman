//! Typed, adapter-private entry points for fixed SQLite statements.
//!
//! Every call binds all named parameters and decodes named columns. The Rust
//! signatures check callers; SQLite checks SQL and storage types at execution.
//! Cached statements belong to a connection and never outlive its checkout or
//! transaction. Schema DDL remains in the Go migrations used by `create`.

use std::num::NonZeroU32;

use rusqlite::{Connection, OptionalExtension, Transaction, named_params};

use crate::LoadRecord;
pub(crate) mod tc;
pub(crate) mod xdp;

#[derive(Clone, Eq, PartialEq)]
pub(super) struct StoredLinkRow {
    pub(super) id: i64,
    pub(super) kind: String,
    pub(super) program_id: i64,
    pub(super) program_kind: Option<String>,
    pub(super) kernel_id: Option<i64>,
    pub(super) pin_path: Option<String>,
    pub(super) metadata: String,
    pub(super) created_at: String,
    pub(super) group: Option<String>,
    pub(super) name: Option<String>,
    pub(super) interface: Option<String>,
    pub(super) ifindex: Option<i64>,
    pub(super) direction: Option<String>,
    pub(super) priority: Option<i64>,
    pub(super) netns: Option<String>,
    pub(super) nsid: Option<i64>,
    pub(super) function: Option<String>,
    pub(super) offset: Option<i64>,
    pub(super) retprobe: Option<i64>,
    pub(super) tracing_function: Option<String>,
    pub(super) attach_func: Option<String>,
    pub(super) user_target: Option<String>,
    pub(super) user_function: Option<String>,
    pub(super) user_offset: Option<i64>,
    pub(super) user_pid: Option<i64>,
    pub(super) container_pid: Option<i64>,
    pub(super) user_retprobe: Option<i64>,
}

pub(super) fn stored_links(
    connection: &Connection,
    id: Option<i64>,
) -> rusqlite::Result<Vec<StoredLinkRow>> {
    let mut statement = connection.prepare_cached(
        "SELECT l.id, l.kind, l.kernel_prog_id AS program_id,
                p.program_type AS program_kind, l.kernel_link_id AS kernel_id,
                l.pin_path, l.metadata_json AS metadata, l.created_at,
                t.tp_group AS tp_group, t.tp_name AS tp_name,
                x.interface, x.ifindex, x.direction, x.priority, x.netns, x.nsid, k.fn_name, k.offset, k.retprobe,
                u.target AS user_target, u.fn_name AS user_function, u.offset AS user_offset,
                u.pid AS user_pid, u.container_pid, u.retprobe AS user_retprobe,
                CASE l.kind WHEN 'fentry' THEN fe.fn_name WHEN 'fexit' THEN fx.fn_name WHEN 'lsm' THEN ls.hook_name END AS tracing_function, p.attach_func
         FROM links l
         LEFT JOIN managed_programs p ON p.program_id = l.kernel_prog_id
         LEFT JOIN link_tracepoint_details t ON t.id = l.id
         LEFT JOIN link_tcx_details x ON x.id = l.id
         LEFT JOIN link_kprobe_details k ON k.id = l.id
         LEFT JOIN link_uprobe_details u ON u.id = l.id
         LEFT JOIN link_fentry_details fe ON fe.id = l.id
         LEFT JOIN link_fexit_details fx ON fx.id = l.id
         LEFT JOIN link_lsm_details ls ON ls.id = l.id
         WHERE (:id IS NULL OR l.id = :id) ORDER BY l.id",
    )?;
    statement
        .query_map(named_params! { ":id": id }, |row| {
            Ok(StoredLinkRow {
                id: row.get("id")?,
                kind: row.get("kind")?,
                program_id: row.get("program_id")?,
                program_kind: row.get("program_kind")?,
                kernel_id: row.get("kernel_id")?,
                pin_path: row.get("pin_path")?,
                metadata: row.get("metadata")?,
                created_at: row.get("created_at")?,
                group: row.get("tp_group")?,
                name: row.get("tp_name")?,
                interface: row.get("interface")?,
                ifindex: row.get("ifindex")?,
                direction: row.get("direction")?,
                priority: row.get("priority")?,
                netns: row.get("netns")?,
                nsid: row.get("nsid")?,
                function: row.get("fn_name")?,
                offset: row.get("offset")?,
                retprobe: row.get("retprobe")?,
                tracing_function: row.get("tracing_function")?,
                attach_func: row.get("attach_func")?,
                user_target: row.get("user_target")?,
                user_function: row.get("user_function")?,
                user_offset: row.get("user_offset")?,
                user_pid: row.get("user_pid")?,
                container_pid: row.get("container_pid")?,
                user_retprobe: row.get("user_retprobe")?,
            })
        })?
        .collect()
}

pub(super) fn is_tracepoint(connection: &Connection, id: NonZeroU32) -> rusqlite::Result<bool> {
    connection
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM managed_programs
                       WHERE program_id = :id AND program_type = 'tracepoint') AS supported",
        )?
        .query_row(named_params! { ":id": id.get() }, |row| {
            row.get("supported")
        })
}

pub(super) fn kernel_link_exists(
    connection: &Connection,
    id: NonZeroU32,
) -> rusqlite::Result<bool> {
    connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM links WHERE kernel_link_id = :id) AS present")?
        .query_row(named_params! { ":id": id.get() }, |row| row.get("present"))
}

pub(super) struct PendingLinkInsert<'a> {
    pub(super) program_id: NonZeroU32,
    pub(super) metadata: &'a str,
    pub(super) created_at: &'a str,
    pub(super) group: &'a str,
    pub(super) name: &'a str,
}

pub(super) fn insert_pending_link(
    tx: &Transaction<'_>,
    input: PendingLinkInsert<'_>,
) -> rusqlite::Result<i64> {
    let id = tx.prepare_cached(
        "INSERT INTO links (kind, kernel_prog_id, metadata_json, created_at)
         VALUES ('tracepoint', :program, :metadata, :created) RETURNING id",
    )?.query_row(named_params! {
        ":program": input.program_id.get(), ":metadata": input.metadata, ":created": input.created_at,
    }, |row| row.get("id"))?;
    tx.prepare_cached(
        "INSERT INTO link_tracepoint_details (id, tp_group, tp_name)
         VALUES (:id, :group, :name)",
    )?
    .execute(named_params! { ":id": id, ":group": input.group, ":name": input.name })?;

    Ok(id)
}

pub(super) fn set_link_pin(tx: &Transaction<'_>, id: i64, pin: &str) -> rusqlite::Result<usize> {
    tx.prepare_cached("UPDATE links SET pin_path = :pin WHERE id = :id")?
        .execute(named_params! { ":pin": pin, ":id": id })
}

pub(super) fn finalise_link(
    tx: &Transaction<'_>,
    id: i64,
    kernel: NonZeroU32,
) -> rusqlite::Result<usize> {
    tx.prepare_cached(
        "UPDATE links SET kernel_link_id = :kernel WHERE id = :id AND kernel_link_id IS NULL",
    )?
    .execute(named_params! { ":kernel": kernel.get(), ":id": id })
}

pub(super) fn delete_link(tx: &Transaction<'_>, id: i64) -> rusqlite::Result<usize> {
    tx.prepare_cached("DELETE FROM links WHERE id = :id")?
        .execute(named_params! { ":id": id })
}

pub(super) struct LinkRow {
    pub(super) program_id: i64,
    pub(super) link_id: i64,
}

pub(super) struct SummaryRow {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) metadata: String,
}

pub(super) struct ProgramRow {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) object_path: String,
    pub(super) source_path: Option<String>,
    pub(super) pin_path: String,
    pub(super) attach_func: Option<String>,
    pub(super) global_data: Option<String>,
    pub(super) image_source: Option<String>,
    pub(super) owner: Option<String>,
    pub(super) description: Option<String>,
    pub(super) license: Option<String>,
    pub(super) gpl_compatible: i64,
    pub(super) metadata: Option<String>,
    pub(super) created_at: String,
    pub(super) updated_at: Option<String>,
    pub(super) map_set: i64,
    pub(super) map_path: String,
}

#[derive(PartialEq, Eq)]
pub(super) struct UnloadRow {
    pub(super) attach_func: Option<String>,
    pub(super) kind: String,
    pub(super) object: String,
    pub(super) pin: String,
    pub(super) map_set: i64,
    pub(super) map_path: String,
    pub(super) created: String,
    pub(super) map_created: String,
    pub(super) links: i64,
    pub(super) users: i64,
    pub(super) shared: String,
}

pub(super) struct MapSetIdentity<'a> {
    pub(super) id: NonZeroU32,
    pub(super) pin_path: &'a str,
    pub(super) created_at: &'a str,
}

pub(super) struct ProgramInsert<'a> {
    pub(super) record: &'a LoadRecord<'a>,
    pub(super) globals: &'a str,
    pub(super) object_path: &'a str,
    pub(super) pin_path: &'a str,
    pub(super) metadata: &'a str,
    pub(super) gpl_compatible: bool,
}

pub(super) fn schema_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection
        .prepare_cached("SELECT MAX(version_id) AS version FROM goose_db_version")?
        .query_row([], |row| row.get("version"))
}

pub(super) fn record_schema_version(tx: &Transaction<'_>, version: i64) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT INTO goose_db_version (version_id, is_applied) VALUES (:version, 1)",
    )?
    .execute(named_params! { ":version": version })?;

    Ok(())
}

pub(super) fn links(connection: &Connection) -> rusqlite::Result<Vec<LinkRow>> {
    connection
        .prepare_cached("SELECT kernel_prog_id, id FROM links ORDER BY id")?
        .query_map([], |row| {
            Ok(LinkRow {
                program_id: row.get("kernel_prog_id")?,
                link_id: row.get("id")?,
            })
        })?
        .collect()
}

pub(super) fn summaries(connection: &Connection) -> rusqlite::Result<Vec<SummaryRow>> {
    // As in Go, summary queries include only programs with a concrete map set.
    connection
        .prepare_cached(
            "SELECT p.program_id, p.program_name, p.program_type, p.metadata_json
             FROM managed_programs p JOIN map_sets m ON m.id = p.map_set_id
             ORDER BY p.program_id",
        )?
        .query_map([], |row| {
            Ok(SummaryRow {
                id: row.get("program_id")?,
                name: row.get("program_name")?,
                kind: row.get("program_type")?,
                metadata: row.get("metadata_json")?,
            })
        })?
        .collect()
}

pub(super) fn programs(connection: &Connection) -> rusqlite::Result<Vec<ProgramRow>> {
    connection
        .prepare_cached(
            "SELECT p.program_id, p.program_name, p.program_type, p.object_path,
                    p.source_path, p.pin_path, p.attach_func, p.global_data,
                    p.image_source, p.owner, p.description, p.license,
                    p.gpl_compatible, p.metadata_json, p.created_at, p.updated_at,
                    p.map_set_id, m.pin_path AS map_path
             FROM managed_programs p LEFT JOIN map_sets m ON m.id = p.map_set_id
             ORDER BY p.program_id",
        )?
        .query_map([], |row| {
            Ok(ProgramRow {
                id: row.get("program_id")?,
                name: row.get("program_name")?,
                kind: row.get("program_type")?,
                object_path: row.get("object_path")?,
                source_path: row.get("source_path")?,
                pin_path: row.get("pin_path")?,
                attach_func: row.get("attach_func")?,
                global_data: row.get("global_data")?,
                image_source: row.get("image_source")?,
                owner: row.get("owner")?,
                description: row.get("description")?,
                license: row.get("license")?,
                gpl_compatible: row.get("gpl_compatible")?,
                metadata: row.get("metadata_json")?,
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
                map_set: row.get("map_set_id")?,
                map_path: row.get("map_path")?,
            })
        })?
        .collect()
}

pub(super) fn unload(
    connection: &Connection,
    id: NonZeroU32,
) -> rusqlite::Result<Option<UnloadRow>> {
    connection
        .prepare_cached(
            "SELECT p.program_type, p.attach_func, p.object_path, p.pin_path, p.map_set_id,
                    m.pin_path AS map_path, p.created_at, m.created_at AS map_created,
                    (SELECT count(*) FROM links
                     WHERE kernel_prog_id = p.program_id) AS links,
                    (SELECT count(*) FROM managed_programs
                     WHERE map_set_id = p.map_set_id) AS users,
                    (SELECT json_group_array(map_name) FROM
                        (SELECT map_name FROM shared_map_pins
                         WHERE program_id = p.program_id ORDER BY map_name)) AS shared
             FROM managed_programs p LEFT JOIN map_sets m ON m.id = p.map_set_id
             WHERE p.program_id = :program_id",
        )?
        .query_row(named_params! { ":program_id": id.get() }, |row| {
            Ok(UnloadRow {
                attach_func: row.get("attach_func")?,
                kind: row.get("program_type")?,
                object: row.get("object_path")?,
                pin: row.get("pin_path")?,
                map_set: row.get("map_set_id")?,
                map_path: row.get("map_path")?,
                created: row.get("created_at")?,
                map_created: row.get("map_created")?,
                links: row.get("links")?,
                users: row.get("users")?,
                shared: row.get("shared")?,
            })
        })
        .optional()
}

pub(super) fn insert_map_set(
    tx: &Transaction<'_>,
    map: MapSetIdentity<'_>,
) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT INTO map_sets (id, pin_path, created_at)
         VALUES (:map_set_id, :pin_path, :created_at)",
    )?
    .execute(named_params! {
        ":map_set_id": map.id.get(),
        ":pin_path": map.pin_path,
        ":created_at": map.created_at,
    })?;

    Ok(())
}

pub(super) fn insert_program(
    tx: &Transaction<'_>,
    program: ProgramInsert<'_>,
) -> rusqlite::Result<()> {
    let record = program.record;
    tx.prepare_cached(
        "INSERT INTO managed_programs
         (program_id, program_name, program_type, object_path, source_path, pin_path,
          map_set_id, license, gpl_compatible, metadata_json, created_at, global_data, attach_func)
         VALUES (:program_id, :name, :kind, :object_path, :source_path, :pin_path,
                 :map_set, :license, :gpl_compatible, :metadata, :created_at, :globals, :attach_func)",
    )?
    .execute(named_params! {
        ":program_id": record.id.get(),
        ":map_set": record.map_owner.unwrap_or(record.id).get(),
        ":name": record.spec.name().as_str(),
        ":kind": record.spec.kind().as_str(),
        ":attach_func": match record.spec { bpfman_model::ProgramSpec::Fentry { target, .. } | bpfman_model::ProgramSpec::Fexit { target, .. } | bpfman_model::ProgramSpec::Lsm { hook: target, .. } => Some(target.as_str()), _ => None },
        ":object_path": program.object_path,
        ":source_path": record.source,
        ":pin_path": program.pin_path,
        ":license": record.license,
        ":gpl_compatible": program.gpl_compatible,
        ":metadata": program.metadata,
        ":globals": program.globals,
        ":created_at": record.created_at,
    })?;

    Ok(())
}

pub(super) fn delete_program(tx: &Transaction<'_>, id: NonZeroU32) -> rusqlite::Result<usize> {
    tx.prepare_cached("DELETE FROM managed_programs WHERE program_id = :program_id")?
        .execute(named_params! { ":program_id": id.get() })
}

pub(super) fn delete_map_set(
    tx: &Transaction<'_>,
    map: MapSetIdentity<'_>,
) -> rusqlite::Result<usize> {
    tx.prepare_cached(
        "DELETE FROM map_sets
         WHERE id = :map_set_id AND created_at = :created_at AND pin_path = :pin_path
         AND NOT EXISTS (SELECT 1 FROM managed_programs WHERE map_set_id = :map_set_id)",
    )?
    .execute(named_params! {
        ":map_set_id": map.id.get(),
        ":created_at": map.created_at,
        ":pin_path": map.pin_path,
    })
}

#[cfg(test)]
mod tests;

pub(super) fn insert_pending_tcx(
    tx: &Transaction<'_>,
    input: &bpfman_store::PendingTcx<'_>,
    metadata: &str,
) -> rusqlite::Result<i64> {
    let id = tx.prepare_cached("INSERT INTO links (kind, kernel_prog_id, metadata_json, created_at) VALUES ('tcx', :program, :metadata, :created) RETURNING id")?
        .query_row(named_params! { ":program":input.program_id.get(), ":metadata":metadata, ":created":input.created_at }, |row| row.get("id"))?;
    let d = input.target;
    tx.prepare_cached("INSERT INTO link_tcx_details (id, interface, ifindex, direction, priority, netns, nsid) VALUES (:id,:interface,:ifindex,:direction,:priority,:netns,:nsid)")?
        .execute(named_params!{ ":id":id, ":interface":d.interface.as_str(), ":ifindex":d.key.ifindex.get(), ":direction":d.key.direction.as_str(), ":priority":d.priority, ":netns":d.netns.as_str(), ":nsid":d.key.nsid.get() })?;
    Ok(id)
}

pub(super) fn is_tcx(connection: &Connection, id: NonZeroU32) -> rusqlite::Result<bool> {
    connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM managed_programs WHERE program_id=:id AND program_type='tcx') AS supported")?
        .query_row(named_params!{":id":id.get()}, |row| row.get("supported"))
}

pub(super) fn is_kprobe(
    connection: &Connection,
    id: NonZeroU32,
    retprobe: bool,
) -> rusqlite::Result<bool> {
    connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM managed_programs WHERE program_id=:id AND program_type=:kind) AS supported")?
        .query_row(named_params! { ":id": id.get(), ":kind": if retprobe { "kretprobe" } else { "kprobe" } }, |row| row.get("supported"))
}

pub(super) fn insert_pending_kprobe(
    tx: &Transaction<'_>,
    input: &bpfman_store::PendingKprobe<'_>,
    metadata: &str,
    offset: i64,
) -> rusqlite::Result<i64> {
    let id = tx.prepare_cached("INSERT INTO links (kind, kernel_prog_id, metadata_json, created_at) VALUES (:kind, :program, :metadata, :created) RETURNING id")?
        .query_row(named_params! { ":kind": if input.target.retprobe { "kretprobe" } else { "kprobe" }, ":program": input.program_id.get(), ":metadata": metadata, ":created": input.created_at }, |row| row.get("id"))?;
    tx.prepare_cached("INSERT INTO link_kprobe_details (id, fn_name, offset, retprobe) VALUES (:id, :function, :offset, :retprobe)")?
        .execute(named_params! { ":id": id, ":function": input.target.function.as_str(), ":offset": offset, ":retprobe": input.target.retprobe })?;
    Ok(id)
}

pub(super) struct MapSetRow {
    pub(super) id: i64,
    pub(super) pin_path: String,
}

pub(super) struct SharedMapRow {
    pub(super) name: String,
    pub(super) program: i64,
}

pub(super) fn map_sets(connection: &Connection) -> rusqlite::Result<Vec<MapSetRow>> {
    connection
        .prepare_cached("SELECT id, pin_path FROM map_sets ORDER BY id")?
        .query_map([], |row| {
            Ok(MapSetRow {
                id: row.get("id")?,
                pin_path: row.get("pin_path")?,
            })
        })?
        .collect()
}

pub(super) fn shared_maps(connection: &Connection) -> rusqlite::Result<Vec<SharedMapRow>> {
    connection
        .prepare_cached(
            "SELECT map_name, program_id FROM shared_map_pins ORDER BY map_name, program_id",
        )?
        .query_map([], |row| {
            Ok(SharedMapRow {
                name: row.get("map_name")?,
                program: row.get("program_id")?,
            })
        })?
        .collect()
}

pub(super) fn insert_shared_maps(
    tx: &Transaction<'_>,
    id: NonZeroU32,
    names: &[String],
) -> rusqlite::Result<()> {
    let mut statement = tx.prepare_cached(
        "INSERT INTO shared_map_pins (map_name, program_id) VALUES (:name, :program)",
    )?;
    for name in names {
        statement.execute(named_params! { ":name": name, ":program": id.get() })?;
    }
    Ok(())
}

pub(super) struct MapSetUsage {
    pub(super) path: String,
    pub(super) created: String,
    pub(super) users: i64,
}

pub(super) fn map_set_usage(
    connection: &Connection,
    id: NonZeroU32,
) -> rusqlite::Result<Option<MapSetUsage>> {
    connection.prepare_cached("SELECT pin_path, created_at, (SELECT count(*) FROM managed_programs WHERE map_set_id = :id) AS users FROM map_sets WHERE id = :id")?
        .query_row(named_params! { ":id": id.get() }, |row| Ok(MapSetUsage { path: row.get("pin_path")?, created: row.get("created_at")?, users: row.get("users")? })).optional()
}

pub(super) fn is_uprobe(
    connection: &Connection,
    id: NonZeroU32,
    retprobe: bool,
) -> rusqlite::Result<bool> {
    connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM managed_programs WHERE program_id=:id AND program_type=:kind) AS supported")?
        .query_row(named_params! { ":id": id.get(), ":kind": if retprobe { "uretprobe" } else { "uprobe" } }, |row| row.get("supported"))
}

pub(super) fn insert_pending_uprobe(
    tx: &Transaction<'_>,
    input: &bpfman_store::PendingUprobe<'_>,
    metadata: &str,
    offset: i64,
) -> rusqlite::Result<i64> {
    let id = tx.prepare_cached("INSERT INTO links (kind, kernel_prog_id, metadata_json, created_at) VALUES (:kind, :program, :metadata, :created) RETURNING id")?
        .query_row(named_params! { ":kind": if input.target.retprobe() { "uretprobe" } else { "uprobe" }, ":program": input.program_id.get(), ":metadata": metadata, ":created": input.created_at }, |row| row.get("id"))?;
    tx.prepare_cached("INSERT INTO link_uprobe_details (id, target, fn_name, offset, pid, container_pid, retprobe) VALUES (:id, :target, :function, :offset, :pid, :container_pid, :retprobe)")?
        .execute(named_params! { ":id": id, ":target": input.target.target(), ":function": input.target.function(), ":offset": offset, ":pid": input.target.pid().map_or(0, NonZeroU32::get), ":container_pid": input.target.container_pid().map_or(0, NonZeroU32::get), ":retprobe": input.target.retprobe() })?;
    Ok(id)
}

pub(super) fn is_tracing(
    connection: &Connection,
    input: &bpfman_store::PendingTracing<'_>,
) -> rusqlite::Result<bool> {
    connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM managed_programs WHERE program_id = :id AND program_type = :kind AND attach_func = :function) AS supported")?
        .query_row(named_params! { ":id": input.program_id.get(), ":kind": input.target.kind.program_type().as_str(), ":function": input.target.function.as_str() }, |row| row.get("supported"))
}

pub(super) fn insert_pending_tracing(
    tx: &Transaction<'_>,
    input: &bpfman_store::PendingTracing<'_>,
    metadata: &str,
) -> rusqlite::Result<i64> {
    let id = tx.prepare_cached("INSERT INTO links (kind, kernel_prog_id, metadata_json, created_at) VALUES (:kind, :program, :metadata, :created) RETURNING id")?
        .query_row(named_params! { ":kind": input.target.kind.program_type().as_str(), ":program": input.program_id.get(), ":metadata": metadata, ":created": input.created_at }, |row| row.get("id"))?;
    let sql = match input.target.kind {
        bpfman_model::TracingKind::Fentry => {
            "INSERT INTO link_fentry_details (id, fn_name) VALUES (:id, :function)"
        }
        bpfman_model::TracingKind::Fexit => {
            "INSERT INTO link_fexit_details (id, fn_name) VALUES (:id, :function)"
        }
        bpfman_model::TracingKind::Lsm => {
            "INSERT INTO link_lsm_details (id, hook_name) VALUES (:id, :function)"
        }
    };
    tx.prepare_cached(sql)?
        .execute(named_params! { ":id": id, ":function": input.target.function.as_str() })?;
    Ok(id)
}

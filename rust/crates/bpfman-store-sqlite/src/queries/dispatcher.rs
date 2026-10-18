//! Administrative record deletion deliberately does not decode live snapshots.
use bpfman_model::DispatcherKey;
use rusqlite::{Transaction, named_params};

pub(crate) fn delete(tx: &Transaction<'_>, key: DispatcherKey) -> rusqlite::Result<bool> {
    let (kind, nsid, ifindex, direction) = match key {
        DispatcherKey::Xdp(key) => ("xdp", key.nsid.get(), key.ifindex.get(), ""),
        DispatcherKey::Tc(key) => (
            key.direction.dispatcher_type(),
            key.nsid.get(),
            key.ifindex.get(),
            key.direction.as_str(),
        ),
    };
    let parameters = named_params! { ":type": kind, ":nsid": nsid, ":ifindex": ifindex };
    let present: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM dispatchers
         WHERE type=:type AND nsid=:nsid AND ifindex=:ifindex) AS present",
        )?
        .query_row(parameters, |row| row.get("present"))?;
    if !present {
        return Ok(false);
    }
    let (select, remove) = match key {
        DispatcherKey::Xdp(_) => (
            "SELECT id FROM link_xdp_details WHERE nsid=:nsid AND ifindex=:ifindex",
            "DELETE FROM links WHERE id IN (SELECT id FROM link_xdp_details WHERE nsid=:nsid AND ifindex=:ifindex)",
        ),
        DispatcherKey::Tc(_) => (
            "SELECT id FROM link_tc_details WHERE nsid=:nsid AND ifindex=:ifindex AND direction=:direction",
            "DELETE FROM links WHERE id IN (SELECT id FROM link_tc_details WHERE nsid=:nsid AND ifindex=:ifindex AND direction=:direction)",
        ),
    };
    let members = if matches!(key, DispatcherKey::Xdp(_)) {
        named_params! { ":nsid": nsid, ":ifindex": ifindex }.to_vec()
    } else {
        named_params! { ":nsid": nsid, ":ifindex": ifindex, ":direction": direction }.to_vec()
    };
    let expected = tx
        .prepare_cached(select)?
        .query_map(members.as_slice(), |row| row.get::<_, i64>("id"))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .len();
    let removed = tx.prepare_cached(remove)?.execute(members.as_slice())?;
    let remaining = tx.prepare_cached(select)?.exists(members.as_slice())?;
    if removed != expected || remaining {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    let removed = tx
        .prepare_cached(
            "DELETE FROM dispatchers WHERE type=:type AND nsid=:nsid AND ifindex=:ifindex",
        )?
        .execute(parameters)?;
    if removed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(true)
}

#[cfg(test)]
mod tests;

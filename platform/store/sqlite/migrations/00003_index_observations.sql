-- Index the relationships used by program observations and TCX hook ordering.
-- The primary keys remain the tie-breakers: SQLite appends each INTEGER
-- PRIMARY KEY to these secondary indexes.

-- +goose Up
CREATE INDEX idx_managed_programs_map_set
    ON managed_programs(map_set_id);

CREATE INDEX idx_link_tcx_by_hook_priority
    ON link_tcx_details(nsid, ifindex, direction, priority);

-- +goose Down
DROP INDEX idx_link_tcx_by_hook_priority;
DROP INDEX idx_managed_programs_map_set;

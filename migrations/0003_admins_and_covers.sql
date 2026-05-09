create table if not exists admin_dids (
    did text primary key not null,
    created_at integer not null default (unixepoch())
);

alter table songs add column cover_path text;
alter table songs add column cover_mime_type text;

create table if not exists pds_config (
  key text primary key not null,
  value text not null,
  updated_at integer not null default (unixepoch())
);

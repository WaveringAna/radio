create table if not exists chat_bans (
    did text primary key not null,
    banned_by_did text not null,
    reason text,
    created_at integer not null default (unixepoch())
);

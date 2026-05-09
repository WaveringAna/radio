create table if not exists songs (
    id text primary key not null,
    title text not null,
    artist text not null,
    album text,
    duration_seconds integer,
    file_path text not null,
    mime_type text,
    added_by_did text not null,
    created_at integer not null default (unixepoch())
);

create table if not exists radio_queue (
    id text primary key not null,
    song_id text not null references songs(id) on delete cascade,
    position integer not null,
    queued_by_did text not null,
    created_at integer not null default (unixepoch())
);

create table if not exists radio_state (
    id integer primary key check (id = 1),
    current_song_id text references songs(id) on delete set null,
    status text not null default 'stopped' check (status in ('playing', 'paused', 'stopped')),
    started_at integer,
    paused_at integer,
    position_seconds integer not null default 0,
    updated_by_did text,
    updated_at integer not null default (unixepoch())
);

insert into radio_state (id)
values (1)
on conflict(id) do nothing;

create index if not exists radio_queue_position_idx on radio_queue (position);
create index if not exists songs_created_at_idx on songs (created_at);

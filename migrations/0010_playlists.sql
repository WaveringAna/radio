-- Migration to support saved playlists and sets
create table if not exists playlists (
    id text primary key not null,
    name text not null,
    created_at integer not null default (unixepoch())
);

create table if not exists playlist_tracks (
    playlist_id text not null references playlists(id) on delete cascade,
    song_id text not null references songs(id) on delete cascade,
    position integer not null,
    primary key (playlist_id, position)
);

-- Rotation weights: how often an album comes up in shuffle relative to others.
-- 1 = light, 2 = normal (default), 4 = heavy.
alter table radio_albums add column rotation_weight integer not null default 2;

-- Airlog: every song the station actually played, used for the "recently
-- played" view and to keep shuffle from repeating songs too soon.
create table if not exists play_history (
    id integer primary key autoincrement,
    song_id text not null,
    title text not null,
    artist text not null,
    started_at integer not null
);

create index if not exists idx_play_history_started on play_history(started_at desc);
create index if not exists idx_play_history_song on play_history(song_id, started_at desc);

create table if not exists radio_albums (
  id text primary key,
  title text not null,
  position integer not null,
  is_enabled integer not null default 1,
  created_at integer not null default (unixepoch())
);

create table if not exists radio_album_tracks (
  album_id text not null references radio_albums(id) on delete cascade,
  song_id text not null references songs(id) on delete cascade,
  position integer not null,
  primary key (album_id, song_id)
);

create table if not exists radio_loop_state (
  id integer primary key check (id = 1),
  last_album_id text,
  last_track_position integer not null default 0
);

insert or ignore into radio_loop_state (id) values (1);

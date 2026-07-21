-- Version 12 is intentionally skipped: an earlier, now-reverted migration at
-- that number ("remove_album_loop") dropped these three tables on databases
-- that already applied it (notably prod). Reusing number 12 for anything else
-- would collide with that migration's checksum on those databases. This
-- migration recreates exactly what that one dropped, verbatim, so both a
-- fresh database and one that ran the old version 12 end up at the same
-- schema before the rotation/shuffle work below builds on top of it.
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

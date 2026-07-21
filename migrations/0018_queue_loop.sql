-- Loop modes for the queue, plus per-playlist shuffle-on-load.
--
-- Before this, "loop" only ever meant the album-rotation fallback that runs
-- when the queue drains. These two columns add the other two things a DJ means
-- by looping: recycling the queue itself, and pinning a set that reloads
-- whenever the queue empties.

-- 'off' | 'one' (repeat the current track) | 'queue' (finished tracks go to the back)
alter table radio_state add column loop_mode text not null default 'off';

-- When set, this playlist is re-loaded whenever the queue runs dry, ahead of
-- shuffle and the album loops.
alter table radio_state add column loop_playlist_id text references playlists(id) on delete set null;

alter table playlists add column shuffle_on_load integer not null default 0;

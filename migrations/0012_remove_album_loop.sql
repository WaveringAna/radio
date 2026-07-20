-- Album looping has been replaced by random autoqueue: when the queue is
-- empty the backend picks a random song instead of cycling through
-- admin-managed album loops. Album groupings are now computed on the fly
-- from song metadata instead of being persisted, so these tables are unused.
drop table if exists radio_album_tracks;
drop table if exists radio_albums;
drop table if exists radio_loop_state;

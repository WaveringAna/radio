-- Station-wide shuffle mode. When enabled, the empty-queue fallback plays a
-- random song from the whole library instead of stepping through album loops.
alter table radio_state add column shuffle integer not null default 0;

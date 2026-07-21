-- Marks queue rows that were auto-filled by shuffle mode (the visible
-- lookahead) so they can be distinguished from manually queued songs: manual
-- songs always play first, and shuffle rows are cleared when shuffle turns off.
alter table radio_queue add column is_shuffle integer not null default 0;

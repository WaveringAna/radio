create table if not exists chat_messages (
    id text primary key not null,
    sender_did text not null,
    body text not null,
    created_at integer not null default (unixepoch())
);

create index if not exists chat_messages_created_at_idx on chat_messages (created_at);

create table if not exists oauth_sessions (
    account_did text not null,
    session_id text not null,
    session_json text not null,
    created_at integer not null default (unixepoch()),
    updated_at integer not null default (unixepoch()),
    primary key (account_did, session_id)
);

create table if not exists oauth_auth_requests (
    state text primary key not null,
    auth_request_json text not null,
    created_at integer not null default (unixepoch())
);

create table if not exists app_sessions (
    session_token text primary key not null,
    account_did text not null,
    oauth_session_id text not null,
    created_at integer not null default (unixepoch()),
    expires_at integer not null
);

create index if not exists app_sessions_account_did_idx on app_sessions (account_did);
create index if not exists app_sessions_expires_at_idx on app_sessions (expires_at);

# syndicating radio with evil atproto

hello! i made an internet radio server.

people can sign in with atproto oauth, listen to whatever is currently playing, and complain in chat when i put the same album on again.

running one radio station is easy enough.

the slightly more interesting problem begins when someone else runs one.

i wanted radio stations to be independently hosted. each station should own its own songs, playback state, queue, chat, moderation, and database. an operator should be able to download the server, point a domain at it, and become another station in the network without asking me for permission. then a standalone radio client needs to answer a troublesome question:

> where are all the stations?

the boring answer is that i operate a canonical directory and station operators submit their urls to me.

the even more boring answer is that i put a list of stations in a json file and update it whenever somebody asks nicely.

neither of these is especially terrible. nearly every small federated project begins with a manually maintained list somewhere. unfortunately, i was already using atproto, which meant i was obligated to make the problem substantially stranger.

## the usual shape of an atproto application

a common atproto application has three major parts: users with repositories hosted on pdses, a relay aggregating repository events from those pdses, and an appview indexing that data into something clients can efficiently query.

in theory, there can be many competing appviews. a user should not be permanently tied to one company’s index, moderation system, algorithms, or client.

in practice, applications often develop one appview which nearly every client uses. the protocol is decentralized, but the application still ends up with a center.

projects such as blacksky and tangled are demonstrating that alternative appviews and infrastructure are possible. they are also demonstrating how much work it takes.

a serious appview needs to ingest the network, maintain a large index, implement the application’s semantics, deal with moderation, survive malformed records, and remain compatible with clients people already use.

i did not want to build one of those.

more importantly, i did not actually want the radio to have the usual atproto shape at all.

the songs do not belong to the listeners. the queue does not belong to the listeners. the chat history, playback clock, moderation state, audio files, and playlists do not belong in a hundred unrelated user repositories.

they belong to the station.

so what happens if we invert the architecture?

instead of one large service collecting records from many users, imagine many small services which clients can automatically discover and connect to.

each service keeps its authoritative state in its own database, exposes its own api, and operates independently. atproto is used only for the narrow parts it is especially good at: authenticated identity, signed metadata, discovery, and distribution.

this is entirely protocol-compliant and spiritually improper.

evil atproto.

## putting a tiny pds inside the radio

every station embeds a tiny, read-only atproto pds.

calling it a pds makes it sound more impressive than it really is.

it does not host user accounts. it does not accept arbitrary writes. it does not contain the station’s song library or chat history. it serves one repository belonging to the station’s service did, containing one record:

```text
pet.nkp.radio.station/self
```

conceptually, the record looks something like this:

```json
{
  "$type": "pet.nkp.radio.station",
  "url": "https://radio.example.com",
  "name": "example radio",
  "description": "music transmitted directly into your brain",
  "updatedAt": "2026-07-15T12:00:00Z"
}
```

the repository is signed by the station’s own key. the public key is exposed through the station’s did document, so infrastructure crawling the station can verify that the repository and its station record actually belong to that service. the station implements the small subset of standard atproto endpoints needed to inspect and synchronize the repository

the idea was inspired by [streamplace’s embedded pds for publishing lexicon records](https://blog.stream.place/3lut7mgni5s2k). once i saw that a service could contain a tiny repository without becoming a conventional account-hosting pds, the radio version felt fairly natural. the repository is not the source of truth for the radio. the source of truth remains the station’s sqlite database and audio directory. the embedded repository is only a signed advertisement for the service.

again, evil atproto.

this separation is the important part.

atproto records are good at saying:

> this service exists, it is controlled by this did, and this is how to connect to it.

they do not also need to say:

> here is every song, queue mutation, chat message, listener heartbeat, and playback timestamp the service has ever produced. feel free to dmca me!!!!

the first is durable public information which benefits from signing, replication, and discovery.

the second is high-volume, application-specific operational state which is more naturally handled by the station itself.

the important idea is not that every application should secretly contain a tiny pds. it is that an atproto repository can describe an independently operated service without becoming that service’s entire storage model.

## now somebody has to find it

unfortunately, embedding a repository does not cause relays to psychically detect the hostname.

a new station still needs some way to enter the network.

when the radio server starts, it sends a standard `com.atproto.sync.requestCrawl` request to one or more configured relays or syndication workers:

```json
{
  "hostname": "radio.example.com"
}
```

the request contains almost nothing. the hostname is enough to bootstrap everything else.

from there, the receiver can resolve the station’s identity, connect to its atproto endpoints, and retrieve or follow the repository containing the station record. the station now looks like another source of repository events. a general-purpose relay can carry its commits. a radio-specific indexer can recognize the `pet.nkp.radio.station` collection and add the station to a directory. the crawler did not need advance knowledge of the station’s name, api, database schema, operator, or frontend. it only needed to know that the host existed.

## the syndication worker

i wrote a syndication worker which watches for station records and exposes them to clients and its api is quite fucking huge, see:

```text
GET /stations
GET /stations/{did}
POST /xrpc/com.atproto.sync.requestCrawl
```

i joke. the worker does not ingest the actual radio application state. it does not mirror songs, audio, playlists, chat messages, queues, or playback events. it stores enough information to identify a station, checks whether that station is alive, and exposes the healthy ones through `/stations`.

a standalone frontend asks the worker for the current directory and after the user chooses a station, the frontend communicates directly with it. the worker is not in the playback path. it does not proxy audio. it does not own the chat connection. it does not become the permanent middleman between the listener and the station. it only introduces them, and anyone can run another worker, crawl a different set of sources, apply different health requirements, maintain a curated directory, or refuse to list my station because i played something unforgivable.

a frontend can consult several workers, use a hardcoded station directly, or let the user enter a url.

of course, if every client uses one public worker operated by me, then congratulations: i have recreated a central service with extra steps and i solved nothing, but at least its a very thin rust worker easily replaceable.

## cheating with hydrant

the worker does not implement atproto repository synchronization itself.

doing that would involve discovering pdses, consuming firehoses, resolving identities, fetching missing repositories, validating commits, handling backfills, persisting records, managing cursors, and keeping all of this synchronized without inventing a new and exciting form of data corruption.

instead, it embeds [hydrant](https://hydrant.klbr.net/).

hydrant is a rust atproto indexer built on the fjall key-value database. it can consume firehoses, crawl and backfill repositories, persist records, and produce an ordered, replayable stream of record events. it can run as a standalone service, but it can also be used as a library inside another rust application.

that last part is extremely cool.

rather than operating a generic indexer beside the syndication worker and making the two communicate over http, the worker constructs hydrant directly inside its own process. hydrant handles the miserable protocol-shaped parts of indexing atproto. the worker implements the tiny amount of radio-specific logic layered on top.

the worker configures hydrant in filtered mode:

```rust
hydrant
    .filter
    .set_mode(FilterMode::Filter)
    .set_signals(["pet.nkp.radio.station"])
    .set_collections(["pet.nkp.radio.station"])
    .apply()
    .await?;
```

it does not need to ingest the entire atproto network. it is interested in exactly one collection and can discard everything else. when a station calls `com.atproto.sync.requestCrawl`, the worker validates and rate-limits the hostname, then hands the station’s websocket endpoint to hydrant as another source. hydrant performs the synchronization and emits normalized repository events.

the radio-specific portion of the worker is basically a projection over that stream:

```text
hydrant record event
        │
        │ collection == pet.nkp.radio.station
        ▼
validate station record
        │
        ├── create/update → add it to the directory
        ├── delete        → remove it from the directory
        └── inactive repo → remove it from the directory
```

the worker does not ask hydrant to understand radio stations. hydrant understands dids, repositories, commits, records, cursors, firehoses, crawling, and backfills. the worker understands that a `pet.nkp.radio.station` record should become an entry in `/stations`. it also performs the application-specific checks hydrant should not care about. it requires the station to use a `did:web`, verifies that the did hostname matches the advertised station url, and periodically probes the station’s health endpoint.

hydrant is therefore not quite the radio appview by itself. it is the generic indexing engine underneath one. the thin layer which decides that a valid, healthy station record belongs in `/stations` is the application-specific appview.

hydrant is then maybe an evil appview as a library.

## what i find fun here
what is created here is different from most atproto applications where the station operators, rather than the listeners, own the repositories. the records describe services rather than user-generated objects. most application data remains outside the protocol. and clients use indexed records to discover endpoints, then communicate directly with those endpoints. atproto is merely just for federation here and not the driving force for everything going on. i think this is how you can fit atproto into much more than social media. a record could advertise a game server, collaborative editor, search engine, community archive, model inference endpoint, public dataset, or specialized api. the service could use atproto oauth to recognize users and service-auth tokens to authorize requests without moving its internal state into their repositories.

the protocol does not need to become the application. sometimes it can simply help the application’s participants find one another.
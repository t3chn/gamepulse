# GamePulse Requirements

Status: captured from the received take-home assignment on 2026-08-14

## Mandatory processing

The service runs once per hour and visits the Metacritic Games section. Each run
selects 20 games that have not been processed during the current day and inserts
or updates their information in the service database.

Daily source sequence:

1. The first daily selection uses the Games page New Releases section.
2. Later selections use SEE ALL sorted by newest and may advance through browse
   pages.
3. Each new day starts the sequence again from New Releases.

Required game information:

- title;
- cover image;
- every available platform with Metascore and Userscore;
- developer;
- description;
- video link.

The service reads reviews and creates two separate short summaries:

- what critics like and dislike;
- what users like and dislike.

Review-derived information may be refreshed when a game is processed again.

## Web interface

The service provides:

- a list of loaded games with compact information;
- a complete game detail page;
- platform filtering;
- title search;
- rating sorting;
- similar games selected only from games already stored in the database;
- navigation from a similar-game result to its detail page.

## Optional enrichment 1

For each game:

1. find YouTube letsplays;
2. choose the most popular eligible letsplay;
3. convert the blogger speech to text;
4. summarize the transcript;
5. attach the conclusion and video link to the game.

## Optional enrichment 2

The web interface may expose:

- realtime worker status and processed-record counters;
- a manual processing trigger.

## Delivery requirements

The result includes:

- a repository link;
- a live service link;
- complete visible project AI prompts and responses, with raw JSONL accepted as
  an optional transport format.

## Adopted interpretations

These are architecture decisions rather than quoted assignment facts:

- the mandatory Metacritic trailer/video link is separate from the optional
  YouTube letsplay link;
- daily uniqueness is based on a verified stable source identity, not a page
  cursor;
- rating sort uses the selected platform Metascore, or the maximum Metascore
  when no platform filter is active;
- similar-game scoring is deterministic and degrades safely when optional genre
  data is missing;
- optional YouTube work never holds a mandatory run open.

## Material unknowns

- exact Metacritic endpoint and request contract;
- YouTube and transcript provider;
- LLM provider and budget;
- assignment deadline;
- deployment target and persistent-storage behavior;
- accepted repository visibility and AI transcript format.

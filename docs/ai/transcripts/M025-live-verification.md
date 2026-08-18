# M025 live verification transcript

Sanitized export of visible evaluator-facing prompts and assistant commentary/final responses. Excludes hidden system/developer text, reasoning, tool calls and outputs, private-control correspondence, routing metadata, local paths, file URIs, internal task IDs, credentials, cookies, HR context, raw payloads, and app directives.

## Turn 1

### User

Project root: the current task working directory (<PROJECT_ROOT>)

Objective:
Run exactly one controlled source-enabled GamePulse process and return one
aggregate-only terminal verdict for the mandatory first 20 eligible New Releases
games: whether discovery, ingestion, durable settlement, persistence, review
summaries, health/readiness, and local catalogue/detail rendering all complete.

Read first:
- AGENTS.md
- docs/requirements.md
- ARCHITECTURE.md
- docs/source-contracts/metacritic-direct-http.md
- the committed runtime, source-worker, persistence, summary, and web paths
  that define the fixed first-20 selection, pacing/backoff, settlement,
  observability, health/readiness, and local rendering contracts

Route-verification hold:
- Before any project command or external access, prove this native task is the
  saved-local GamePulse route at exactly
  `8414da1ba33bbcbd5480025e3add4d4ad4a4721d`.
- Verify staged, tracked, and ordinary-untracked state are clean; report ignored
  state separately and permit only `target/` as ignored. Stop with
  `ROUTE_FAILURE` before external access on any route/baseline/state mismatch.
- Confirm the existing release/current binary is available. Do not compile,
  rebuild, install, update dependencies, or create project artifacts. If the
  binary is unavailable or unusable, return `BLOCKED_ENVIRONMENT`.
- Confirm `127.0.0.1:3000` is free and create a new writable temporary SQLite
  directory outside the repository. Do not disclose its absolute path.
- Inspect the committed runtime contract and prove it will consider at most the
  first 20 New Releases candidates before launch. If that bound is not provable,
  return `SOURCE_CONTRACT_FAILURE` before external access.

Frozen launch contract:
- Launch exactly one process once, with
  `GAMEPULSE_DATABASE_PATH` set to the new temporary SQLite file,
  `GAMEPULSE_HTTP_ADDRESS=127.0.0.1:3000`,
  `GAMEPULSE_LOG_FORMAT=human`, and
  `GAMEPULSE_SOURCE_WORK_ENABLED=true`.
- The only external access allowed is anonymous HTTPS GET to the documented
  Metacritic hosts and routes in `docs/source-contracts/metacritic-direct-http.md`.
  Do not use credentials, login, cookies, browser state, headers that identify a
  user, CDN/media/YouTube/LLM services, redirects, external image downloads,
  proxying, or any other host/route.
- Optional public HTML cover enrichment may occur only if the already committed
  runtime does it within its documented per-game fail-closed one-request bound;
  never fetch image bytes. Any unexpected host or route is terminal.
- Do not start a second process, restart, retry, manually trigger work, run a
  canary, issue a diagnostic probe, or perform follow-up source requests.
- Wall-clock ceiling is 15 minutes. Stop at the first conclusive PASS, fixed
  safe failure category, material P0/P1, route/environment/request-bound
  uncertainty, unexpected host/route, or ceiling.

Evidence and privacy:
- Inspect and report aggregate-safe evidence only: discovery count;
  queued/claimed/settled counts; fixed safe failure-category counts; persisted
  game count; required mandatory-field availability counts; separate critic and
  user summary availability counts; health/readiness; and local catalogue/detail
  rendering. Page checks must cause no external render-time request.
- Do not retain or print payloads, titles, slugs, IDs, source URLs, review text,
  response bodies, source identifiers, external image bytes, cookies,
  credentials, or local absolute paths. Do not include raw logs in the result.

Acceptance:
- PASS only when the one run atomically selects 20 eligible unique games; all
  20 mandatory ingestion jobs settle successfully; 20 games persist; every
  required mandatory field and both critic/user summaries meet the existing
  requirement contract; health/readiness pass; and local catalogue/detail render
  persisted data with no external render-time requests.
- Otherwise return exactly one of: SOURCE_CONTRACT_FAILURE,
  PARTIAL_INGESTION, BLOCKED_ENVIRONMENT, ROUTE_FAILURE, or TIMEBOX_EXCEEDED.

Cleanup and stop condition:
- At terminal result or 15 minutes, send SIGINT to the one process, verify no
  GamePulse process or listener remains, and remove the temporary SQLite state
  recoverably without exposing its path. Verify repository state is unchanged.
- Stop after this one result. Do not change code, docs, tests, configuration,
  Git metadata, or tracked repository state; do not commit, push, deploy, send
  messages, configure credentials, publish, or start another milestone.

### Assistant commentary

Вердикт: DO. Сначала подтвержу сохранённый локальный маршрут и чистое состояние, затем проведу ровно один запуск в 15‑минутном лимите без изменений репозитория.

### Assistant commentary

Маршрут подтверждён: нужный сохранённый checkout и commit, рабочее дерево чисто; в ignored только `target/`. Перед разрешённым сетевым чтением применяю обязательную проверку внешнего запуска.

### Assistant commentary

Контракт подтверждён статически: первый запрос ограничен 20 кандидатами New Releases, отбор уникальный и атомарный; дальше только допустимый Metacritic direct-HTTP с отключёнными редиректами и ретраями. Сейчас проверяю только готовность локального запуска.

### Assistant commentary

Готовность подтверждена: единственный release-бинарник исполним, порт свободен, прокси-переменных нет. Запускаю один процесс с заданными четырьмя переменными; остановка — при первом терминальном результате либо через 15 минут.

### Assistant commentary

PARTIAL_INGESTION

- Discovery: 20 selected, 20 unique.
- Jobs: 19 queued, 0 claimed, 2 settled: 1 succeeded, 1 failed.
- Source ingestion: 0/20 succeeded; fixed failure `review_continuation_link`: 6 attempts.
- Persisted games and summaries: 0.
- Health/readiness: 200/200.
- Local rendering не проверялось: остановка по первой безопасной категории ошибки.
- Процесс и listener остановлены; временное SQLite-состояние перемещено в корзину; репозиторий не изменён.

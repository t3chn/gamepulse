# M023 implementation transcript

Sanitized export of visible evaluator-facing prompts and assistant commentary/final responses. Excludes hidden system/developer text, reasoning, tool calls and outputs, private-control correspondence, routing metadata, local paths, file URIs, internal task IDs, credentials, cookies, HR context, raw payloads, and app directives.

## Turn 1

### User

Project root: the current task working directory (<PROJECT_ROOT>)

Objective:
Implement exactly M023: correct the M022 live-readiness blockers for first-run
New Releases exact-20 selection, durable retry backoff/source pacing, and
repository-owned reproducible mutation evidence. This is one bounded
implementation pass; you are the sole writer.

Read first:
- AGENTS.md
- docs/requirements.md
- ARCHITECTURE.md
- the current crawl selection, SQLite JobStore/queue, source runtime/worker,
  migrations, tests, and mise tasks relevant to those boundaries

Route-verification hold:
- Before any edit, prove this is the native saved-local GamePulse project route
  at exactly d6864966009f94088613b96d1ebff427808453c0.
- Verify staged/unstaged tracked state and ordinary-untracked state are clean;
  report ignored state separately. Stop on any mismatch. Do not create a
  worktree, task_ref, subagent, replacement route, or concurrent writer.

Scope:
- First daily New Releases selection: if New Releases initially yields fewer
  than 20 eligible unique games, continue through the documented bounded source
  sequence. Atomically commit exactly 20 eligible unique games, or fail closed
  without a partial successful run when bounded source exhaustion makes 20
  impossible. Preserve daily reset, stable source identity, deterministic
  ordering, request ceilings, validated browse continuation, replay behavior,
  and transactional selection state.
- Add deterministic regressions for initial New Releases pages below, at, and
  above 20; duplicates and already-processed candidates; bounded exhaustion;
  and no-partial-success semantics.
- Durable retry backoff and source pacing: prevent immediate retry/request
  bursts after transient timeout, 429, and provider failures. Persist retry
  eligibility so restart cannot erase it. Apply bounded deterministic backoff
  and source-lane pacing without sleeping inside database transactions, an
  unbounded loop, provider-specific dependency, credential, or live call.
  Preserve queue leases/fencing, crash safety, at-least-once processing, and
  scheduler responsiveness.
- Add clock-controlled deterministic tests for retry schedule, restart
  persistence, 429/timeout handling, pacing, success reset, terminal failure,
  and no early claim.
- Add the smallest repository-owned, documented deterministic mutation command
  or harness for the critical exact-20/source-selection invariants changed in
  M021/M023. It must run from this repository without private patches or
  control-layer state; list a bounded mutant set; enforce a hard ceiling;
  classify caught, noncompiling, and surviving mutants truthfully; fail when a
  mutant survives; and never commit generated mutation output.
- Update project requirements/architecture documentation only where the
  implemented contract needs correction, especially any earlier allowance for a
  short successful selection after source exhaustion.

Constraints:
- Do not change the fixed 300-second lease duration or fencing P2 unless you
  prove it is inseparable from the authorized retry fix. If that happens, stop
  and request an owner decision rather than widening scope.
- Do not add optional YouTube, LLM, status, manual-trigger, container, or
  live-acceptance work. Do not call Metacritic or any external service.
- Use existing dependencies unless a concrete need is proven; stop for an
  owner decision before adding one.
- Preserve crate ownership, SQLite as the durable source of truth, and the
  Cargo edge allowlist. Do not put private paths, credentials, raw source
  content, or control metadata in project files.

Acceptance criteria:
- First-run New Releases never records a successful short selection: it
  atomically commits exactly 20 eligible unique games or returns a clear
  fail-closed exhaustion outcome with no partial committed selection.
- Retry eligibility and source pacing survive a SQLite reopen and prevent an
  early claim or burst under the controlled clock; success resets retry state
  and terminal failures remain terminal.
- The committed mutation command is documented, bounded, reproducible, and
  fails if a declared mutant survives.
- All relevant focused tests pass, along with architecture fitness and full
  offline CI. The complete diff is limited to M023 and contains no generated
  mutation result/build artifact or secret-like content.

Verification:
- Run focused deterministic regressions for every acceptance item above.
- Run the new committed mutation command/harness and report its bounded mutant
  list and outcome counts.
- Run `mise run architecture`, `CARGO_NET_OFFLINE=true mise run ci`,
  `git diff --check`, and tracked/untracked/ignored state readback.
- Inspect the final diff for scope and privacy. Do not run container or live
  source checks.

Stop condition:
Stop after this first implementation pass and return the exact changed paths,
test commands/results, mutation evidence, remaining risk, and repository
state. Do not start a next milestone.

External actions:
Do not commit, push, deploy, publish, send messages, configure credentials,
download, call live sources, or mutate external services. A local commit may
be considered only after a separate Sol PASS and an explicit follow-up from
this control task.

### Assistant commentary

Вердикт: DO. Сначала проверю закреплённый маршрут, чистоту состояния и лимит ресурсов; затем прочитаю проектные контракты и внесу только M023 без внешних вызовов.

### Assistant commentary

Маршрут подтверждён: корень совпадает с текущей папкой, `HEAD` — `d6864966009f94088613b96d1ebff427808453c0`; staged/изменённые/обычные untracked отсутствуют. Отдельно: есть только игнорируемый `target/`, его не трогаю. Сейчас фиксирую фактические границы M021/M022 в коде и миграциях.

### Assistant commentary

Нашёл два реальных разрыва M022: первая короткая страница `New Releases` сейчас коммитится сразу, а очередь возвращает retry в `ready` без времени следующей попытки. Исправляю контракт так, чтобы короткая выборка не становилась успехом, а очередь держала retry/pacing в SQLite.

### Assistant commentary

Контракт не менялся: просто строка в requirements отличается от ожидаемого контекста патча. Сверяю точный фрагмент и применяю узкое документальное изменение до кода.

### Assistant commentary

Базовая реализация уже меняет только M023-границы: exact-20, fail-closed outcome, SQLite migration для retry/pacing и source runtime с интервалом 2 секунды. Сейчас запускаю компиляцию, чтобы поправить все затронутые типы и миграционные инварианты до расширения тестов.

### Assistant commentary

Фокусные selection-тесты теперь проходят; SQLite-тесты покрывают backoff после timeout/429/provider failure, reopen, success reset и pacing после reopen. Добавляю воспроизводимый локальный mutation harness: три фиксированных временных мутации, без патча рабочего дерева и без сохранения результатов.

### Assistant commentary

Очередь и runtime-regressions зелёные: backoff не допускает ранний claim, survives reopen; source pacing проверена и в SQLite, и в runtime с управляемыми часами. Запускаю новый ограниченный mutation harness; он создаёт только временные каталоги в `/tmp` и удаляет их в конце.

### Assistant commentary

Проверяю ещё одну важную грань: retry/pacing не должны превращаться в почасовую задержку после готовности. Добавляю только read-only расчёт ближайшего durable claim-time, чтобы runtime мог ждать именно его без polling и без сна в SQLite-транзакции.

### Assistant commentary

Mutation harness завершился: 3/3 mutants caught, 0 noncompiling, 0 surviving. До финального CI делаю scope/privacy-проверку diff и статуса; затем останутся architecture gate, offline CI и финальный readback.

### Assistant commentary

Проверка критических путей завершена: exact-20 остановка стоит до commit во всех трёх вариантах selection, а pacing обновляется в той же SQLite-транзакции, что и claim. Запускаю обязательные architecture и полный офлайн-CI.

### Assistant commentary

Полный CI выявил один регресс: `m009_source_ingestion` рассчитывает на немедленный второй source-claim, что теперь намеренно ограничено durable pacing. Уточняю тестовую конфигурацию и скорректирую только её контролируемое время/ожидание, затем повторю CI.

### Assistant commentary

Регрессия тестовой фикстуры устранена: она теперь даёт ровно 20 кандидатов и сохраняет проверку ingestion первого из них. Полный офлайн-CI прошёл. Повторяю mutation и architecture на финальном дереве, затем только readback diff/status.

### Assistant commentary

Финальная mutation-проверка: 3/3 mutants caught, 0 noncompiling, 0 surviving. Делаю последний readback состояния и diff; внешних вызовов и коммита не было.

### Assistant commentary

M023 реализован. Статус: `IMPLEMENTED, REVIEW_PENDING` — независимая Sol-проверка ещё нужна по правилам проекта.

Что изменено:

- Exact-20: первый короткий New Releases продолжает bounded browse; при исчерпании — `Exhausted` без частичного commit.
- Durable retry: 30/60/120/240/300 секунд, сохраняется в SQLite после reopen.
- Source pacing: durable interval 2 секунды между source-claims, без сна внутри транзакций.
- Добавлен `mise run mutation`: 3 declared mutants, временная локальная копия, offline, survivor ломает команду.
- M009-фикстура приведена к exact-20 контракту.

Изменённые пути:

- [ARCHITECTURE.md](<PROJECT_ROOT>/ARCHITECTURE.md)
- [docs/requirements.md](<PROJECT_ROOT>/docs/requirements.md)
- [docs/mutation-testing.md](<PROJECT_ROOT>/docs/mutation-testing.md)
- [mise.toml](<PROJECT_ROOT>/mise.toml)
- [scripts/mutation.sh](<PROJECT_ROOT>/scripts/mutation.sh)
- [0007_retry_backoff_and_source_pacing.sql](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/migrations/0007_retry_backoff_and_source_pacing.sql)
- [crates](<PROJECT_ROOT>/crates)

Проверки прошли:

- `cargo test -p gamepulse-application --test daily_crawl --locked` — 7 passed
- `cargo test -p gamepulse-storage-sqlite --lib --locked` — 53 passed
- `cargo test -p gamepulse --test m007_source_handler --locked` — 7 passed
- `cargo test -p gamepulse --test m009_source_ingestion --locked` — 2 passed
- `cargo test -p gamepulse --test runtime --locked` — 10 passed
- `mise run mutation` — caught=3, noncompiling=0, surviving=0
- `mise run architecture` — 8 passed
- `CARGO_NET_OFFLINE=true mise run ci` — passed
- `git diff --check` — passed

Маршрут до правок был подтверждён как native local repo на `d6864966009f94088613b96d1ebff427808453c0`, с чистыми tracked/ordinary-untracked файлами; ignored отдельно: `target/`.

Финальное состояние: изменений в index нет; modified/untracked пути только M023, ignored — `target/`. Коммитов, внешних вызовов и live source checks не было.

## Turn 2

### User

Sol review round 1 returned PASS with no findings. Under the frozen M023
authority, you are now authorized to create exactly one local commit of the
accepted aggregate diff.

Before committing:
- Re-read status and stop if HEAD is not
  d6864966009f94088613b96d1ebff427808453c0, if anything is staged, or if the
  changed/untracked paths differ from the accepted M023 aggregate.
- The accepted paths are:
  ARCHITECTURE.md
  docs/requirements.md
  docs/mutation-testing.md
  mise.toml
  scripts/mutation.sh
  crates/gamepulse-application/src/lib.rs
  crates/gamepulse-application/tests/daily_crawl.rs
  crates/gamepulse-storage-sqlite/migrations/0007_retry_backoff_and_source_pacing.sql
  crates/gamepulse-storage-sqlite/src/job_queue.rs
  crates/gamepulse-storage-sqlite/src/lib.rs
  crates/gamepulse-worker-source/src/lib.rs
  crates/gamepulse/src/main.rs
  crates/gamepulse/src/runtime.rs
  crates/gamepulse/tests/m007_source_handler.rs
  crates/gamepulse/tests/m009_source_ingestion.rs
  crates/gamepulse/tests/m014_observability.rs
  crates/gamepulse/tests/runtime.rs
- Do not stage target/ or any other path. Run the cached diff check.

Commit:
- Create one new local commit only (no amend) with this English change-only
  message: Fix GamePulse live readiness controls
- Then report the commit SHA, parent SHA, subject, exact committed paths, and
  final staged/tracked/ordinary-untracked/ignored state.

Stop condition:
Stop immediately after that one local commit and readback. Do not make another
edit, run another review, push, deploy, publish, send a message, configure
credentials, call a live source, or mutate any external service.

### Assistant commentary

Вердикт: DO. Сверяю HEAD, index и точный набор M023-путей; если всё совпадает, создам один локальный commit без иных действий.

### Assistant commentary

Проверка пройдена: HEAD совпадает, index пуст, набор modified/untracked в точности соответствует accepted M023; отдельно ignored только `target/`. Ставлю в index только эти 17 путей и проверяю cached diff.

### Assistant commentary

Cached diff без ошибок и содержит ровно accepted 17 путей. Создаю единственный локальный commit, затем только readback.

### Assistant commentary

Создан ровно один локальный commit.

- SHA: `8414da1ba33bbcbd5480025e3add4d4ad4a4721d`
- Parent: `d6864966009f94088613b96d1ebff427808453c0`
- Subject: `Fix GamePulse live readiness controls`

Точные committed paths:

- [ARCHITECTURE.md](<PROJECT_ROOT>/ARCHITECTURE.md)
- [docs/requirements.md](<PROJECT_ROOT>/docs/requirements.md)
- [docs/mutation-testing.md](<PROJECT_ROOT>/docs/mutation-testing.md)
- [mise.toml](<PROJECT_ROOT>/mise.toml)
- [scripts/mutation.sh](<PROJECT_ROOT>/scripts/mutation.sh)
- [crates/gamepulse-application/src/lib.rs](<PROJECT_ROOT>/crates/gamepulse-application/src/lib.rs)
- [crates/gamepulse-application/tests/daily_crawl.rs](<PROJECT_ROOT>/crates/gamepulse-application/tests/daily_crawl.rs)
- [0007_retry_backoff_and_source_pacing.sql](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/migrations/0007_retry_backoff_and_source_pacing.sql)
- [crates/gamepulse-storage-sqlite/src/job_queue.rs](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/src/job_queue.rs)
- [crates/gamepulse-storage-sqlite/src/lib.rs](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/src/lib.rs)
- [crates/gamepulse-worker-source/src/lib.rs](<PROJECT_ROOT>/crates/gamepulse-worker-source/src/lib.rs)
- [crates/gamepulse/src/main.rs](<PROJECT_ROOT>/crates/gamepulse/src/main.rs)
- [crates/gamepulse/src/runtime.rs](<PROJECT_ROOT>/crates/gamepulse/src/runtime.rs)
- [crates/gamepulse/tests/m007_source_handler.rs](<PROJECT_ROOT>/crates/gamepulse/tests/m007_source_handler.rs)
- [crates/gamepulse/tests/m009_source_ingestion.rs](<PROJECT_ROOT>/crates/gamepulse/tests/m009_source_ingestion.rs)
- [crates/gamepulse/tests/m014_observability.rs](<PROJECT_ROOT>/crates/gamepulse/tests/m014_observability.rs)
- [crates/gamepulse/tests/runtime.rs](<PROJECT_ROOT>/crates/gamepulse/tests/runtime.rs)

Финальное состояние: staged — пусто; tracked diff — пусто; ordinary untracked — пусто; ignored — только `target/`. Push и внешние действия не выполнялись.

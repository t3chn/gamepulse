# M020 review transcript

Sanitized export of visible evaluator-facing prompts and assistant commentary/final responses. Excludes hidden system/developer text, reasoning, tool calls and outputs, private-control correspondence, routing metadata, local paths, file URIs, internal task IDs, credentials, cookies, HR context, raw payloads, and app directives.

## Turn 1

### User

Project root: the current task working directory (<PROJECT_ROOT>)

Objective:
Perform the frozen M020 independent read-only aggregate code audit at commit 37e6aa07e91fdb4f891a475f3e500590d0dfa460. Decide whether the mandatory take-home implementation is fit to proceed to a separately authorized live 20-game acceptance. Do not fix anything.

Read first:
- AGENTS.md
- docs/requirements.md
- ARCHITECTURE.md
- README.md
- Dockerfile
- mise.toml
- docs/source-contracts/
- all relevant crate source and tests before forming a verdict

Terra evidence to evaluate, not trust blindly:
- Exact HEAD matched 37e6aa07e91fdb4f891a475f3e500590d0dfa460; tracked and ordinary-untracked state were clean; target/ only ignored.
- Offline architecture passed 8/8 including sabotage cases. Offline CI passed 150 tests; 3 were expected ignored: two manual UI fixtures and the forbidden live Metacritic canary.
- Deterministic source-disabled release demo returned 200 for readiness, liveness, and /games; it received SIGINT and cleaned its fixture. A separate temporary SQLite release check exercised /games/101 and detail navigation.
- Fixture evidence passed for daily 20-game policy, queue/runtime, persistence/upsert, critic and user summaries, catalogue/detail/query behavior, similar navigation, restart/idempotency, daily reset, retry/lease/fencing, and health/readiness.
- No live source call occurred. Docker is installed but required base images are absent; build/smoke would require a prohibited image pull. There is no remote repository, live service link, or complete AI prompt/response export. There is no single deterministic source-to-summaries 20-record end-to-end test, although component contracts pass.

Scope:
- Read the full repository and inspect the current implementation and tests.
- Review correctness against every mandatory requirement in docs/requirements.md; clean architecture and architecture-fitness functions; workspace boundaries; scheduler, queue, worker concurrency; SQLite transactions, crash safety, deduplication, idempotency, retry, lease, and fencing behavior; Metacritic adapter/parser failure modes; HTML escaping, privacy/security; observability; test adequacy; container and local delivery readiness; and evaluator-facing clarity.
- Assess the Terra evidence and run narrow deterministic checks only where needed to validate or challenge it.
- Explicitly distinguish: implementation defects; missing mandatory functionality; insufficient evidence; sandbox-only limitations; and optional/backlog work.

Constraints:
- Read-only: do not edit tracked files, Git metadata, control-layer files, or documentation; do not stage, commit, reset, repair, create a worktree, or use a subagent/task_ref.
- Do not call live Metacritic or any public endpoint; do not use credentials, external assets/services, image pulls, deploys, pushes, publication, or external mutation.
- Do not turn delivery-artifact gaps into a code defect unless docs/requirements.md makes them mandatory. Do not silently treat unexercised live source or container build as passed.

Acceptance criteria:
- Return concrete P0-P3 findings with file:line, failure scenario, and smallest fix. If none, state that clearly.
- Provide a requirement-level assessment and determine whether the missing one-shot 20-record deterministic end-to-end evidence is material.
- Return exactly one verdict: PASS_TO_LIVE_ACCEPTANCE, CHANGES_REQUIRED, or BLOCKED_ENVIRONMENT.
- State whether any non-code delivery gap blocks only final submission versus live acceptance.

Verification:
- Use existing deterministic local evidence and targeted offline inspection/tests as needed. Report exact command/check outcomes, repository-state findings, and any limitations without copying secrets or private data.

Stop condition:
One aggregate review only (30 minutes maximum). Stop after returning the required verdict. Do not request or perform fixes, verification rounds, commits, live acceptance, container pulls, deployment, publication, or another milestone.

External actions:
Do not commit, push, deploy, send messages, configure credentials, invoke live sources or external services, pull container images, or mutate external systems.

### Assistant commentary

DO: проведу один замороженный read-only аудит коммита `37e6aa0…`, без исправлений и внешних вызовов. Сначала сверю лимит, состояние репозитория и проектные контракты; затем проверю реализацию, тесты и только узкие офлайн-проверки. Остановлюсь на одном итоговом вердикте.

### Assistant commentary

Состояние подтверждено: точный `HEAD`, tracked/untracked чисто, единственный ignored-путь — `target/`. Лимит-файл отсутствует, поэтому выполняю ровно один bounded-cycle. Из прошлой истории беру только известный инвариант пагинации M015; текущий код и требования проверяю заново.

### Assistant commentary

Промежуточный вывод: компонентные проверки зелёные, но они не закрывают два обязательных сквозных свойства. Уже подтверждены: выбор заканчивает часовой запуск после одной страницы (поэтому после replay страницы из 24 элементов следующий запуск берёт лишь 4, а не добирает 20), и UI намеренно не показывает сохранённую обложку. Проверяю, нет ли более приоритетных дефектов в lease/SQLite и в генерации summary.

### Assistant commentary

Вердикт: `CHANGES_REQUIRED`

### Findings

- **P1 — Часовой запуск не гарантирует 20 игр.** Use case читает ровно одну страницу и сразу коммитит результат: [lib.rs](<PROJECT_ROOT>/crates/gamepulse-application/src/lib.rs:1520). Тест прямо закрепляет сценарий, где после первых 20 из browse-страницы на 24 элемента следующий запуск выбирает только 4: [daily_crawl.rs](<PROJECT_ROOT>/crates/gamepulse-application/tests/daily_crawl.rs:158). Это противоречит требованию «каждый запуск выбирает 20». Минимальный fix: в одном use case переходить по browse-страницам до 20 уникальных кандидатов либо явного exhaustion, затем делать один атомарный commit.

- **P1 — Likes/dislikes семантически недостоверны.** Summarizer копирует целые excerpts и считает негативными только шесть токенов: [lib.rs](<PROJECT_ROOT>/crates/gamepulse-worker-llm/src/lib.rs:23). Например, “combat is terrible and awful” попадёт в Likes, а “not bad” — в Dislikes. При этом доступный source score отбрасывается: [lib.rs](<PROJECT_ROOT>/crates/gamepulse-worker-source/src/lib.rs:1773). Минимальный fix: сохранить polarity/score в review input и строить короткие bounded extractive bullets с adversarial-тестами на явный негатив, отрицание и mixed review.

- **P1 — Обязательная обложка фактически не предоставляется.** Даже при сохранённом валидном URL список и detail всегда показывают placeholder и текст “external image not loaded”: [lib.rs](<PROJECT_ROOT>/crates/gamepulse-web/src/lib.rs:478), [lib.rs](<PROJECT_ROOT>/crates/gamepulse-web/src/lib.rs:597). Более того, незавершённое enrichment намеренно отменяется и сохраняет `NULL`: [lib.rs](<PROJECT_ROOT>/crates/gamepulse-worker-source/src/lib.rs:1612). Descriptor и индикатор наличия URL — не cover image. Минимальный fix: определить обязательный безопасный путь получения/рендеринга обложки, передавать сам URL в view и проверять `<img>` плюс fallback; запись без доступной обложки либо должна быть неeligible, либо должна иметь отдельное гарантированное завершение.

- **P2 — Lease короче допустимого source attempt.** Production lease — 300 секунд: [main.rs](<PROJECT_ROOT>/crates/gamepulse/src/main.rs:142). Один game job последовательно выполняет detail, Userscore для каждой неограниченной по количеству платформы и две review-загрузки: [lib.rs](<PROJECT_ROOT>/crates/gamepulse-worker-source/src/lib.rs:1573), каждая с timeout 20 секунд. Игра с 13 медленными платформами может превысить lease; данные уже запишутся, но fenced settlement будет отвергнут. Минимальный fix: общий attempt deadline заметно меньше lease либо продление lease; добавить тест с fake clock и длинной platform collection.

- **P2 — Нет source pacing и retry backoff.** Любой non-200 становится одинаковой ошибкой: [lib.rs](<PROJECT_ROOT>/crates/gamepulse-worker-source/src/lib.rs:2110), а retry сразу переводится в `ready`: [job_queue.rs](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/src/job_queue.rs:275). После завершения task runtime немедленно снова claim’ит работу. При `429` три попытки могут быть израсходованы подряд, что противоречит AD-5 по независимому rate-limit/retry policy. Минимальный fix: durable `not_before`/backoff и source request pacing; отдельно учитывать `429`/`Retry-After`.

P0 и P3 findings нет.

### Требования и доказательства

| Область | Оценка |
|---|---|
| Почасовой scheduler, daily reset, deduplication | Реализованы; правило ровно 20 за запуск нарушено |
| Title, platforms/scores, developer, description, video | Детерминированные component paths подтверждены |
| Cover image | Обязательная функциональность не завершена |
| Critic/user summaries | Разделение, persistence и fencing есть; смысл likes/dislikes некорректен |
| List/detail/filter/search/sort/similar navigation | Подтверждено fixture-тестами |
| SQLite atomicity, retry, lease fencing, restart/idempotency | Core queue хорошо покрыт; остаются lease-duration и retry-pacing findings |
| Architecture fitness | Cargo shape и edge allowlist подтверждены; это не доказывает полную семантическую архитектуру |
| Security/privacy | Askama escaping, bounded review inputs, fixed log categories и отсутствие очевидных секретов подтверждены |
| Optional YouTube/SSE/manual trigger | Backlog, не блокирует mandatory scope |

Отсутствие единого deterministic 20-record E2E-теста **материально**. Это не просто формальный пробел: существующий unit test закрепляет underfill до четырёх записей. Нужен один offline сценарий: selection через границу browse-страниц → 20 source jobs → 20 snapshots → 40 kind-separated summary outcomes → catalogue/detail readback. До этого live acceptance не должна начинаться.

### Проверка Terra evidence

- `HEAD` подтверждён: `37e6aa07e91fdb4f891a475f3e500590d0dfa460`.
- Tracked и ordinary-untracked state чисты; только ignored `target/`.
- `git diff --check` — чисто.
- `mise run architecture` — 8/8.
- `mise run ci` — успешно: 150 passed, 3 expected ignored.
- Live canary не запускался; публичных вызовов не было.
- Docker 29.6.1 установлен, но обе base images локально отсутствуют; pull не выполнялся. Это sandbox/evidence limitation, не code defect.
- Remote repository отсутствует; в `docs/ai/` только policy и prehistory, полного экспорта нет; live-service link отсутствует.

Repository link, live-service link и полный AI export блокируют только финальную сдачу, не отдельную live acceptance. Непроверенный container build также не мешает source acceptance, но оставляет final delivery readiness недоказанной.

Файлы и Git metadata не изменялись.

<oai-mem-citation>
<citation_entries>
MEMORY.md:34-40|note=[M015 continuation invariants and verification procedure]
rollout_summaries/2026-08-15T20-02-50-gGb5-m015_metacritic_clamp_final_read_only_review_pass.md:20-27|note=[prior M015 evidence rechecked against current tree]
</citation_entries>
<rollout_ids>
<TASK_ID>
</rollout_ids>
</oai-mem-citation>

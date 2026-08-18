# M012 implementation transcript

Sanitized export of visible evaluator-facing prompts and assistant commentary/final responses. Excludes hidden system/developer text, reasoning, tool calls and outputs, private-control correspondence, routing metadata, local paths, file URIs, internal task IDs, credentials, cookies, HR context, raw payloads, and app directives.

## Turn 1

### User

Project root: the current task working directory (<PROJECT_ROOT>)

Objective:
Implement exactly M012: bounded optional public-HTML og:image cover enrichment in the source layer, persisted validated cover URLs, and deterministic server-rendered cover placeholders, without weakening mandatory ingestion.

Read first:
- AGENTS.md
- docs/requirements.md
- ARCHITECTURE.md
- docs/source-contracts/metacritic-direct-http.md
- README.md

Route-verification hold:
- Before changing files, verify this is the saved-local GamePulse project on main at baseline 87cd8bf4495c40595dc6352cfba2cebc507c1ae0, with no tracked or ordinary-untracked changes. Report ignored paths separately. Stop on any mismatch; do not create a worktree, substitute a route, or edit.

Control-derived decision:
- Implement optional cover enrichment only in the source adapter. At most one public HTML GET may be attempted for each game-ingestion attempt; never make such a request on a catalogue/detail render.
- Parse exactly one og:image metadata declaration. Accept only a non-empty public HTTPS URL whose host is exactly www.metacritic.com. Missing, duplicate, malformed, oversized, non-HTTPS, or other-host values must fail closed to no persisted public cover URL. Do not derive or probe any CDN/image URL from a detail descriptor.
- Use a separate low-concurrency source-side HTML gate with a bounded timeout, body cap, redirects disabled, and retries disabled. A 403, 429, or challenge-like HTML response must open an in-process circuit that prevents further HTML attempts until restart. Any optional failure must preserve the game snapshot and normal mandatory job outcome.
- Do not add runs/run_items to model the 20% full-batch disablement condition. If the existing fixed 20-item source batch can be observed without them, disable after more than four parse/validation failures in that completed batch; otherwise document it as an operational revisit condition and stop rather than inventing run state.

Scope:
- Extend the source adapter, source-facing application/domain values only as required, and SQLite snapshot persistence/read models to carry one validated optional public cover URL atomically with the snapshot.
- Render only the persisted URL in catalogue/detail UI. Provide a deterministic local textual placeholder when it is absent and a browser image-load fallback when the remote image cannot load.
- Update ARCHITECTURE.md and docs/source-contracts/metacritic-direct-http.md to state this control-derived decision and its limits before the governed behavior. Add focused fixture-only source, storage migration/reopen, and in-process HTTP rendering tests.

Do not:
- Make a live HTTP request, run a source canary, start the production runtime, fetch an image/CDN URL, proxy or store image bytes, add a CDN heuristic, install dependencies, configure credentials, call a provider/model, commit, push, deploy, send messages, or mutate an external service.
- Implement runs/run_items, SSE, manual triggers, YouTube/media, M011 closure, transcript export, archival, or unrelated refactors.

Constraints:
- Preserve AD-2, AD-3, AD-5, AD-7, AD-8, AD-9, and AD-10. The binary remains the composition root; application/domain do not depend on concrete source, web, SQLite, or HTTP libraries; the eight-package Cargo graph remains unchanged.
- Treat HTML and URL values as untrusted. The HTML lane is optional and cannot change queue settlement, daily selection, review summaries, or record integrity. Do not widen the explicit host allowlist.
- Use existing dependencies only. Any need for a new dependency, different host, live evidence, or unresolved architecture contradiction is a stop.

Acceptance criteria:
- Fixture coverage proves exactly-one valid og:image acceptance; every invalid metadata/status/challenge/circuit branch preserves the mandatory snapshot and stores no public cover URL.
- Migration/reopen coverage proves atomic persistence of the URL. In-process HTTP rendering shows the validated stored image source, an absent-URL local placeholder, and a browser-load-error fallback without any source request.
- Architecture and source-contract documents accurately describe the bounded decision, including the current public-HTML/bot-protection risk and the 20% revisit threshold.

Verification:
- Run focused source, application, SQLite, and in-process HTTP tests.
- Run CARGO_NET_OFFLINE=true mise run architecture.
- Run CARGO_NET_OFFLINE=true mise run ci.
- Run git diff --check and inspect the aggregate diff for privacy and scope.
- Report tracked, untracked, and ignored checkout state. Mutation is NOT_APPLICABLE because this optional in-memory breaker cannot alter a critical durable state machine, transaction, selection policy, or run finalization; fixture/integration coverage is required for its fail-closed behavior.

Stop condition:
Stop after one 90-minute implementation pass and report exact changes, verification results, known limitations, and aggregate diff. Do not begin review, correction, commit, M011 closure, or any external action.

External actions:
Do not commit, push, deploy, send messages, configure credentials, invoke live sources or canaries, install tools or dependencies, or mutate external services.

### Assistant commentary

Вердикт: DO. Сначала подтвержу сохранённый локальный baseline и чистоту checkout; при любом расхождении остановлюсь без изменений.

### Assistant commentary

Проверка маршрута пройдена: это основной checkout `gamepulse`, `main` на `87cd8bf…`, tracked и обычные untracked-файлы отсутствуют. Отдельно есть только игнорируемый `target/`; его не трогаю. Теперь картирую существующие порты, snapshot и рендеринг, чтобы сохранить границы.

### Assistant commentary

Границы подтверждены: текущая исполняемая ingestion-ветка — M011 source adapter; `runs/run_items` в ней не доступны без нового состояния, поэтому порог 20% фиксирую как operational revisit, без выдумывания модели batch. Сначала внесу этот контракт в архитектурные документы, затем — один атомарный кодовый срез.

### Assistant commentary

Документы теперь фиксируют разрешённый узкий контракт. Дальше добавляю value object, миграцию и отдельный fail-closed HTML adapter; UI будет читать только SQLite, без нового source-вызова.

### Assistant commentary

Первый кодовый срез готов: public-HTML transport отделён от обязательного JSON client, имеет gate/circuit и возвращает только optional value. Сейчас прогоняю форматирование и компиляцию, затем добавлю fixture-ветки на отказ и reopen/rendering.

### Assistant commentary

Компиляция проходит. Добавил fixture-only проверки HTML отказов и circuit, миграцию v4→v5, atomic reopen, а также image/placeholder/fallback в server-rendered UI. Осталось запустить целевые тесты и устранить только подтверждённые сбои.

### Assistant commentary

Целевые проверки проходят: source fixtures (valid/invalid/status/challenge/circuit), SQLite 47 tests с v3/v4 reopen, application port и HTTP catalogue/detail, плюс M011 regression. Запускаю обязательные offline architecture и CI; после них только финальная проверка агрегатного diff и состояния checkout.

### Assistant commentary

Offline architecture и полный CI зелёные; live canary остался ignored и не запускался. Делаю последнюю read-only проверку diff, новых файлов и ignored `target/`; после этого остановлюсь без review/commit/внешних действий.

### Assistant commentary

Готово: M012 реализован одним bounded-срезом, без live-запросов и внешних действий.

- Source adapter: отдельный HTML client с timeout/body cap, без redirect/retry, gate на один in-flight запрос и circuit до рестарта для 403/429/challenge. Принимает только один валидный `og:image` с `https://www.metacritic.com`.
- Snapshot/SQLite: URL сохраняется атомарно с snapshot; добавлена миграция v5 и reopen-покрытие.
- UI: catalogue/detail читают только SQLite URL, выводят локальный `Cover unavailable.` без URL и browser `onerror` fallback.
- Документы фиксируют HTML/bot-protection риск и условие пересмотра 20% без `runs/run_items`.

Ключевые файлы: [source adapter](<PROJECT_ROOT>/crates/gamepulse-worker-source/src/lib.rs:569), [migration](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/migrations/0005_public_cover_url.sql:1), [rendering](<PROJECT_ROOT>/crates/gamepulse-web/src/lib.rs:220).

Проверки прошли:

- Focused source fixtures: 3/3.
- SQLite: 47/47, включая v3/v4→v5 reopen.
- Application, HTTP rendering и M011 regression: зелёные.
- `CARGO_NET_OFFLINE=true mise run architecture`: 8/8.
- `CARGO_NET_OFFLINE=true mise run ci`: зелёный.
- `git diff --check`: зелёный.

Mutation: `NOT_APPLICABLE` — breaker не меняет критическую durable state machine, transaction, selection policy или run finalization; fail-closed ветки покрыты fixture/integration тестами.

Состояние checkout: 11 изменённых tracked файлов, 2 новых ordinary-untracked файла (миграция и source-тест), ignored только `target/`. Ничего не закоммичено. Ограничение остаётся намеренным: batch-level отключение после >4 parse/validation failures документировано как revisit condition, пока нет наблюдаемого completed 20-item batch без добавления `runs/run_items`.

<oai-mem-citation>
<citation_entries>
MEMORY.md:24-27|note=[confirmed historical GamePulse repository routing and durable boundary context]
</citation_entries>
<rollout_ids>
<TASK_ID>
</rollout_ids>
</oai-mem-citation>

## Turn 2

### User

Project root: the current task working directory (<PROJECT_ROOT>)

Objective:
Complete the single owner-authorized M012 correction pass for the existing uncommitted aggregate diff from baseline 87cd8bf4495c40595dc6352cfba2cebc507c1ae0. Address exactly the accepted review findings below. You remain the sole GamePulse writer.

Read first:
- AGENTS.md
- ARCHITECTURE.md
- docs/source-contracts/metacritic-direct-http.md
- crates/gamepulse-worker-source/src/lib.rs
- crates/gamepulse-worker-source/tests/public_cover_enrichment.rs
- crates/gamepulse-storage-sqlite/migrations/0005_public_cover_url.sql
- crates/gamepulse-storage-sqlite/src/lib.rs
- the current aggregate M012 diff

Scope:
- Correct the M012 public-HTML optional-cover path, its fixture tests, and the smallest related SQLite schema validation tests.
- Update architecture/source-contract documentation only if it is required to keep the corrected behavior accurate.
- Do not change the frozen M012 scope or touch unrelated M011 behavior.

Accepted review findings and required fixes:

1. P1 — optional cover work must never consume the mandatory 300-second lease.
Current issue: ingestion awaits public_cover_enricher.enrich(...) after the otherwise mandatory work.
Required behavior:
- Start the optional cover future concurrently with the mandatory detail/platform/review work after the game identity is known.
- If the mandatory snapshot is ready first, cancel/drop the still-pending cover future and persist None.
- If the cover future completed before mandatory work is ready, its validated result may be attached.
- Do not spawn detached work. Keep cancellation structured and ensure the in-flight gate permit is released on cancellation.
- Add a deterministic fixture/integration test with a deliberately delayed or pending cover fetch that proves mandatory ingestion reaches durable settlement successfully and persists no cover instead of waiting for the optional fetch.

2. P2 — HTML parsing must be HTML-context aware and decode attribute entities safely.
Required behavior:
- Parse exactly one effective og:image declaration only in valid HTML data context.
- Ignore meta-looking text inside comments and raw-text script/style contexts.
- Preserve fail-closed duplicate semantics for effective declarations.
- Decode HTML character references in attribute values once, without recursive decoding; specifically cover the &amp;amp; case.
- Do not add a dependency. A bounded local tokenizer/parser is acceptable if it handles these contexts correctly.
- Add fixture tests for comment, script, style/raw-text, duplicate effective declarations, and entity decoding.

3. P2 — 403/429 circuit latching must occur from status/headers before body reading.
Required behavior:
- A known 403 or 429 must open the until-restart circuit immediately after response headers, before content-length validation, chunk reads, or UTF-8 decoding.
- A challenge-like successfully read HTML response must also latch the circuit.
- A body read failure on a non-blocking response remains an optional failure and preserves the mandatory snapshot.
- Refactor the transport seam only as narrowly as needed so this is testable without live HTTP.
- Add deterministic tests proving 403 and 429 latch even when a body read would fail, challenge latches, and a non-blocking body-read failure neither fails mandatory ingestion nor erroneously opens the circuit.

4. P2 — v5 validation must prove the new public_cover_url CHECK behavior.
Required behavior:
- Add a behavioral schema probe proving blank/whitespace public_cover_url insertion is rejected by SQLite.
- Add an M012-specific sabotage/reopen test where a v5-shaped schema contains the column but lacks the public_cover_url CHECK; opening it must fail validation.
- Keep existing migration/reopen coverage intact.

Mutation evidence is APPLICABLE:
- Add the smallest deterministic tests necessary to kill removal of the in-flight exclusion and neutralization of the 403/429/challenge circuit latch.
- Perform two controlled, reversible local mutation checks: (a) disable the in-flight exclusion and show the concurrency test fails; (b) neutralize the status/challenge latch and show the relevant circuit test fails. Restore each mutation with the inverse patch before proceeding. Do not leave mutation edits in the aggregate diff.
- If either mutation is genuinely impossible with the existing offline toolchain, stop and report the exact blocking proof rather than weakening the requirement.

Constraints:
- The HTML lane remains optional: no failure, timeout, parse rejection, circuit state, or cancellation may change mandatory snapshot/review/job settlement.
- Keep one HTML attempt per game ingestion, no UI source calls, persisted validated URL only, no URL on failure, no CDN heuristic, no image fetch/proxy/bytes.
- Keep no redirects, no retries, bounded timeout/body, exact existing HTTPS host allowlist, low-concurrency one-in-flight gate, and circuit semantics.
- Use existing dependencies only. Do not add runs/run_items, SSE, manual triggers, media/YouTube, or unrelated refactors.
- Do not perform any network/live source/canary call, external model/provider call, credential action, dependency installation, commit, push, deployment, transcript export, archival, M011 closure, or external mutation.

Acceptance criteria:
- All four accepted findings are fixed with focused deterministic evidence.
- The delayed-cover durable-settlement test proves the mandatory result persists with None when cover is not ready.
- Parser tests cover comments, script/style raw text, duplicate effective metadata, and one-pass &amp;amp; decoding.
- Circuit tests cover 403, 429, challenge, and body-read failure semantics.
- SQLite tests prove both live blank-value rejection and v5 CHECK sabotage rejection.
- Mutation checks provide recorded fail-then-restore evidence for in-flight exclusion and status/challenge latch behavior.
- The aggregate M012 diff remains privacy-safe, scoped, offline-only, and uncommitted.

Verification:
- Run the focused public-cover source tests, including the new delayed/circuit/parser cases.
- Run the focused SQLite storage tests, including migration/reopen and the new behavior/sabotage tests.
- Run relevant application and in-process catalogue/detail HTTP rendering tests.
- Run CARGO_NET_OFFLINE=true mise run architecture.
- Run CARGO_NET_OFFLINE=true mise run ci.
- Run git diff --check and inspect tracked, untracked, and ignored checkout state.
- Report commands/results, the exact mutation fail/restore evidence, changed files, any residual risks, and the aggregate diff status.

Stop condition:
Stop after this one correction pass and report the evidence. Do not commit, start review, begin another milestone, or do any external action. If a P0/P1, source-contract contradiction, route/root mismatch, dependency/network need, or genuine lack of progress remains, stop and state it concretely.

### Assistant commentary

Вердикт: DO. Сначала подтвержу baseline и ровно существующий M012 diff, затем прочту указанные файлы и внесу только четыре принятые коррекции. Live/source действия не запускаю.

### Assistant commentary

Baseline совпадает, а dirty tree — ровно M012 aggregate diff (11 tracked + migration и source fixtures); `target/` остаётся единственным ignored путём. Корректирую source seam так, чтобы status проверялся до чтения body, а cover future структурно отменялся при готовом обязательном snapshot.

### Assistant commentary

Исправляю контекстный HTML-разбор и затем добавлю узкие тесты для отмены, circuit и SQLite-проверок. Внешние запросы не выполняю.

### Assistant commentary

Контекстные и circuit-ветви теперь покрыты локальными fixture-тестами. Добавляю отдельную проверку, что зависшая обложка не удерживает обязательное SQLite-урегулирование.

### Assistant commentary

Фиксированные source, SQLite и durable-settlement тесты проходят. Сейчас выполняю два требуемых обратимых mutation-check: сначала gate, затем circuit; изменения будут немедленно восстановлены.

### Assistant commentary

Обе мутации дали ожидаемое падение и уже восстановлены; после восстановления все узкие source/UI/application тесты снова зелёные. Запускаю обязательные offline архитектурную и полную CI-проверки.

### Assistant commentary

Offline architecture и полный CI прошли, включая игнорируемый live-canary (не запускался). Делаю финальную проверку aggregate diff и состояния checkout без новых изменений.

### Assistant commentary

DO — M012 correction pass завершён, без коммита и внешних действий.

Исправлено:

- Optional cover запускается рядом с обязательной загрузкой и отменяется при её раннем завершении; snapshot сохраняется с `None`.
- HTML-разбор игнорирует comments/script/style, ловит effective-дубликаты и однократно декодирует entities.
- `403`/`429` открывают circuit до чтения body; challenge тоже лочит circuit.
- SQLite проверяет `public_cover_url` CHECK при открытии и отклоняет blank/whitespace URL.

Добавлены/обновлены тесты в [source adapter](<PROJECT_ROOT>/crates/gamepulse-worker-source/src/lib.rs), [fixture tests](<PROJECT_ROOT>/crates/gamepulse-worker-source/tests/public_cover_enrichment.rs), [SQLite validation](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/src/lib.rs), [durable settlement test](<PROJECT_ROOT>/crates/gamepulse/tests/m012_public_cover_settlement.rs), [architecture](<PROJECT_ROOT>/ARCHITECTURE.md) и [source contract](<PROJECT_ROOT>/docs/source-contracts/metacritic-direct-http.md).

Проверки прошли:

- Focused source: 10/10.
- SQLite: 49/49.
- Durable pending-cover settlement: 1/1.
- Catalogue/detail HTTP: 1/1.
- M011 regression: 5/5.
- `CARGO_NET_OFFLINE=true mise run architecture`: 8/8.
- `CARGO_NET_OFFLINE=true mise run ci`: успешно; live-canary остался ignored.
- `git diff --check`: чисто, включая отдельную проверку untracked файлов.

Mutation evidence:

- Убрана проверка `in_flight`: concurrency-тест детерминированно упал; inverse patch восстановлен, тест снова прошёл.
- Нейтрализованы status/challenge latches: 403, 429 и challenge-тесты упали (`2` HTML calls вместо `1`); inverse patch восстановлен.

Состояние checkout от `87cd8bf4495c40595dc6352cfba2cebc507c1ae0`: 11 tracked modified, 3 ordinary-untracked M012 файла (migration, source fixtures, durable-settlement test), ignored только `target/`. Aggregate diff остаётся uncommitted, scope/privacy проверены. Ограничение остаётся прежним: public HTML может быть недоступен из-за bot protection; batch-level 20% disablement — operational revisit при наблюдаемом завершённом batch.

<oai-mem-citation>
<citation_entries>
MEMORY.md:24-27|note=[confirmed historical GamePulse repository routing and durable boundary context]
</citation_entries>
<rollout_ids>
<TASK_ID>
</rollout_ids>
</oai-mem-citation>

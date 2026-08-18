# M021 review transcript

Sanitized export of visible evaluator-facing prompts and assistant commentary/final responses. Excludes hidden system/developer text, reasoning, tool calls and outputs, private-control correspondence, routing metadata, local paths, file URIs, internal task IDs, credentials, cookies, HR context, raw payloads, and app directives.

## Turn 1

### User

Route-verification hold.

Do not run project commands, inspect files, edit files, create a worktree, or take review action yet.

Reply only that you are waiting for the control-plane M021 review prompt. The control plane will verify this fresh native saved-local project route and register your task lineage before authorizing the read-only review.

### Assistant commentary

Ожидаю контрольный prompt на ревью M021 от control plane.

## Turn 2

### User

Project root: the current task working directory (<PROJECT_ROOT>)

Objective:
Perform exactly one independent, read-only aggregate review of the complete
uncommitted M021 diff against the three P1 defects reported by M020. Return
PASS, CORRECTION_REQUIRED, or OWNER_GATE. Do not edit anything.

Read first:
- AGENTS.md
- docs/requirements.md
- ARCHITECTURE.md
- the complete current M021 diff and focused tests
- the M020 P1 findings and M021 deterministic evidence below

M020 P1 findings to close:
1. A replayed 24-item browse page can leave four candidates, and a later hourly
   run commits those four instead of continuing to exactly 20.
2. Weak likes/dislikes token matching discards available score/polarity and
   misclassifies explicit negative, negated-positive, negated-negative, and
   mixed review text.
3. An available persisted cover URL is not rendered on catalogue/detail and
   incomplete enrichment can retain NULL.

Reported M021 deterministic evidence:
- The sole Terra writer independently verified the saved-local baseline
  37e6aa07e91fdb4f891a475f3e500590d0dfa460 and a clean tracked and ordinary
  untracked tree; target/ only was ignored before editing.
- The changed path now follows browse continuation until exactly 20 unique
  eligible candidates, explicit exhaustion, or an eight-page cap, then makes
  one atomic commit. The 24-item replay regression makes one 20-item commit;
  exhaustion and cap cases have focused tests.
- Deterministic fixtures cover positive, explicit negative, negated-positive,
  negated-negative, mixed, and unknown review text. Clear source score is
  retained only as local derived polarity for unknown-text fallback.
- Cover data is retained across incomplete enrichment and SQLite persistence,
  renders on catalogue/detail when present, and uses the existing local
  placeholder otherwise. Rendering does not fetch any external data.
- Focused regressions, mise run architecture, mise run ci, and git diff
  --check passed. A bounded continuation mutation batch produced 12 candidates:
  11 caught, 1 unviable compile mutant, 0 survived.
- M020 P2 lease-expiry and durable retry/backoff/pacing findings were explicitly
  excluded and must remain backlog only.

Review scope:
- Inspect actual aggregate diff, migrations, tests, and the writer evidence;
  do not accept the narrative alone.
- Check the exact-20 selection invariant, no partial commit at page cap,
  bounded/fail-closed continuation behavior, daily uniqueness, deterministic
  ordering, first-run New Releases, and later browse continuation.
- Check classification semantics and precedence for all six cases, critic/user
  separation, bounded/untrusted input handling, and persisted polarity mapping.
- Check cover acquisition/persistence/upsert/render path, absent fallback,
  safe URL handling, escaped server rendering, and absence of render-time
  external requests.
- Check SQLite migration/reopen compatibility, architecture ownership/Cargo
  edges, scope, privacy, dependencies, external URLs/assets, and that P2 work
  did not leak in.
- Independently run only relevant read-only deterministic checks when useful.
  Do not call any live provider, start a source-enabled runtime, use
  credentials, contact an external service, or make any change.

Report:
- State the exact commands or evidence inspected.
- List findings with priority, file:line, concrete failure scenario, and
  smallest fix. Do not invent findings.
- State whether a material P0/P1 remains, the M021 verdict, and whether the
  sole Terra writer may be authorized to commit.
- If PASS, say explicitly that the aggregate diff is accepted for one local
  commit by the same sole Terra writer. If not PASS, do not authorize commit.

Stop condition:
Stop after this one 30-minute read-only review. Do not edit, correct, commit,
push, deploy, publish, send messages, configure credentials, mutate external
systems, or start another milestone.

External actions:
Do not invoke live sources/providers or mutate any external system.

### Assistant commentary

DO — проведу один ограниченный read-only review M021: сначала сверю правила и лимит работы, затем фактический diff, тесты и только релевантные детерминированные проверки. Ничего менять или коммитить не буду.

### Assistant commentary

Правила и архитектурный контракт прочитаны полностью. Маршрут подтверждён как нужный Git-проект; теперь сверяю HEAD, весь tracked/untracked scope и затем читаю каждый изменённый фрагмент вместе с тестами.

### Assistant commentary

Фактический diff подтверждает две из трёх линий исправления на уровне кода и тестов. При проверке reopen-совместимости обнаружен риск в переходе v5→v6: миграция добавляет `NULL` polarity, но не обновляет старый hash, тогда как новый код считает hash иначе даже для `NULL`. Сейчас подтверждаю это по точной формуле и проверяю, нет ли второго P0/P1.

### Assistant commentary

Проверка формулы подтвердила дефект совместимости: v5 хранит hash только по виду и текстам, v6 при `polarity=NULL` добавляет поле `"unknown"`, а миграция 0006 hash/fingerprint не пересчитывает. Перед итогом запускаю один полный локальный deterministic gate и architecture gate; live-source и внешние вызовы не запускаются.

### Assistant commentary

# CORRECTION_REQUIRED

Найден один материальный P1.

- **P1 — v5 review inputs становятся нечитаемыми после миграции v6.** [application/lib.rs:501](<PROJECT_ROOT>/crates/gamepulse-application/src/lib.rs:501), [0006_review_excerpt_polarity.sql:1](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/migrations/0006_review_excerpt_polarity.sql:1), [review_summary.rs:224](<PROJECT_ROOT>/crates/gamepulse-storage-sqlite/src/review_summary.rs:224). Миграция добавляет старым excerpts `polarity=NULL`, не меняя `content_hash`. Новый код добавляет `"unknown"` в hash каждого такого excerpt. Поэтому легитимный v5 input с непустыми excerpts после reopen не проходит hash-проверку; сохранённый pending summary job завершается ошибкой до повторного source refresh. Минимальное исправление: сохранить точный legacy hash-формат, когда все polarity отсутствуют, а polarity-aware формат доменно версионировать, когда хотя бы одна polarity задана. Добавить reopen-тест с реальным v5 input, непустым excerpt и pending summary request.

::code-comment{title="[P1] Legacy review hashes break after v6 migration" body="Migration 0006 leaves existing content hashes unchanged but migrated NULL polarity is now encoded as an extra `unknown` field. Any v5 review input with excerpts therefore fails the hash check after reopen. Preserve the legacy encoding for all-NULL polarity inputs and use a domain-separated versioned encoding when polarity exists; cover this with a seeded v5 reopen test." file="<PROJECT_ROOT>/crates/gamepulse-application/src/lib.rs" start=501 end=508 priority=1}

Остальные проверенные линии M021 выглядят закрытыми:

- browse continuation собирает ровно 20, exhaustion допускает короткий commit, cap завершается без частичного commit;
- порядок, дневная уникальность, New Releases и atomic jobs сохранены;
- шесть sentiment-классов, critic/user separation и score fallback реализованы;
- cover сохраняется при неполном enrichment, рендерится из SQLite с Askama escaping и локальным fallback;
- P2 lease/retry/backoff/pacing в diff не проникли, Cargo edges и зависимости не менялись.

Проверено:

```text
git rev-parse --show-toplevel
git rev-parse HEAD
git status --short --branch
git diff --name-status
git diff --stat
git diff --numstat
git ls-files --others --exclude-standard
git status --short --ignored
git diff --no-ext-diff ...  # все 14 tracked paths пофайлово
git show HEAD:crates/gamepulse-application/src/lib.rs
mise run architecture
mise run ci
git diff --check
```

HEAD подтверждён: `37e6aa07e91fdb4f891a475f3e500590d0dfa460`. Прочитаны также оба untracked файла миграции/fixture. `mise run architecture` — 8/8; `mise run ci` и `git diff --check` прошли. Live canary остался ignored, внешних вызовов не было. Зелёный CI не покрывает описанный seeded-v5 сценарий.

Материальный P0 отсутствует, материальный P1 остаётся. **M021: CORRECTION_REQUIRED. Коммит sole Terra writer пока не авторизован.**

<oai-mem-citation>
<citation_entries>
MEMORY.md:30-40|note=[bounded review rules and required deterministic evidence]
MEMORY.md:3-4|note=[verified GamePulse route and continuation review context]
</citation_entries>
<rollout_ids>
<TASK_ID>
<TASK_ID>
</rollout_ids>
</oai-mem-citation>

## Turn 3

### User

Project root: the current task working directory (<PROJECT_ROOT>)

Objective:
Perform the one permitted read-only verification pass for M021. Verify that the
P1 you reported in round 1 is fixed and re-check the aggregate M021 diff for
any remaining material P0/P1. Return PASS or a conclusive failure. Do not edit.

Round-1 P1:
A v5 review input with non-empty excerpts becomes unreadable after migration
0006 because NULL polarity is newly encoded in the content hash while the
stored legacy hash remains unchanged. A pending local summary request then
fails before source refresh.

Bounded correction to verify:
- When every excerpt has absent polarity, the application preserves the exact
  legacy v5 hash encoding.
- When any polarity exists, it uses a domain-separated
  gamepulse-review-input:v2 encoding.
- A seeded v5 database with a real non-empty excerpt and pending summary request
  now reopens at v6, validates the legacy input, runs the local summary path,
  and saves the result.
- The correction did not change the exact-20 source-selection invariant, so no
  second mutation batch was run. Existing mutation evidence remains 12
  candidates, 11 caught, 1 unviable, 0 survived.

Reported verification evidence:
- application hash vectors: 3/3
- seeded v5-to-v6 migration/reopen: 1/1
- M011 summary lifecycle: 6/6
- local classifier fixtures: 5/5
- mise run architecture: 8/8
- mise run ci: passed
- git diff --check: passed
- no dependency, credential, provider/live-source call, external action, P2
  lease/retry/backoff/pacing change, staging, or commit

Review scope:
- Inspect actual corrected code, v5 seed/reopen regression, migration, hash
  separation, and all aggregate M021 changes; do not rely on the report alone.
- Confirm legacy compatibility is byte-for-byte for all absent polarity and
  polarity-aware input cannot collide with legacy format.
- Check that the pending request proof is real rather than merely a hash unit
  test.
- Reconfirm M020's three P1 outcomes, scope/privacy, migration safety, no
  external request at render time, and no P2 leakage.
- Run only relevant read-only deterministic checks if useful. Do not call any
  provider/live source or alter state.

Report:
- State exact checks/evidence inspected.
- List only actionable findings with priority, file:line, scenario, and
  smallest fix.
- State whether any material P0/P1 remains.
- If PASS, state exactly: "M021 aggregate diff is accepted for one local commit
  by the same registered Terra sole writer." If not PASS, do not authorize a
  commit.

Stop condition:
This is review round 2 of 2 and the only verification pass. Stop after this
verdict. Do not edit, commit, push, deploy, publish, send messages, configure
credentials, mutate external systems, or start another milestone. No further
writer correction is authorized.

### Assistant commentary

DO — выполню единственный read-only verification pass: сверю исправленную hash-схему и реальный v5→v6 pending-summary тест, затем повторно просмотрю весь aggregate diff и запущу только локальные детерминированные gates. Ничего менять не буду.

### Assistant commentary

Маршрут и baseline подтверждены, scope по-прежнему ограничен 15 tracked и 2 untracked путями; зависимостей и queue-policy файлов нет. Перехожу к ключевой проверке: байтовая совместимость legacy hash, доменное разделение v2 и настоящий pending-job lifecycle после v5 migration.

### Assistant commentary

Исправление по коду корректно: all-`None` идёт через дословно прежнюю последовательность полей, а любой `Some` начинает hash с отдельного первого поля `gamepulse-review-input:v2`, поэтому legacy и v2 форматы структурно не пересекаются. Seeded regression действительно создаёт v5 schema с непустым excerpt и pending summary row, мигрирует через реальный store, загружает request, запускает local summarizer и применяет fenced write. Запускаю финальные локальные gates.

### Assistant commentary

# PASS

Actionable findings отсутствуют. Материальных P0/P1 не осталось.

Round-1 P1 закрыт:

- all-`None` polarity использует байт-в-байт прежний v5 hash-формат;
- polarity-aware input начинается с отдельного поля `gamepulse-review-input:v2`, поэтому структурно не совпадает с legacy encoding;
- seeded v5 regression реально мигрирует SQLite, загружает непустой pending input, запускает local summarizer и применяет fenced summary write.

Проверено:

```text
git rev-parse --show-toplevel
git rev-parse HEAD
git status --short --branch
git diff --name-status
git diff --stat
git diff --numstat
git ls-files --others --exclude-standard
git status --short --ignored
git diff --no-ext-diff --unified=8 --
git show HEAD:crates/gamepulse-application/src/lib.rs
mise run architecture
mise run ci
git diff --check
```

Результаты: architecture 8/8, hash tests 3/3, M011 lifecycle 6/6, classifier 5/5, daily crawl 11/11, SQLite 50/50, полный CI и diff check прошли. Live canary остался ignored; внешних вызовов не было. Exact-20, sentiment/polarity, cover persistence/rendering и отсутствие P2 leakage повторно подтверждены.

M021 aggregate diff is accepted for one local commit by the same registered Terra sole writer.

<oai-mem-citation>
<citation_entries>
MEMORY.md:30-40|note=[bounded final review rules and deterministic evidence requirements]
MEMORY.md:3-4|note=[verified GamePulse route and continuation review context]
</citation_entries>
<rollout_ids>
<TASK_ID>
<TASK_ID>
</rollout_ids>
</oai-mem-citation>

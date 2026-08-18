# M012a implementation replacement transcript

Sanitized export of visible evaluator-facing prompts and assistant commentary/final responses. Excludes hidden system/developer text, reasoning, tool calls and outputs, private-control correspondence, routing metadata, local paths, file URIs, internal task IDs, credentials, cookies, HR context, raw payloads, and app directives.

## Turn 1

### User

Project root: the current task working directory (<PROJECT_ROOT>)

Objective:
Implement exactly M012a: replace the handwritten optional public-HTML og:image parser with a bounded html5ever 0.39.0 TreeSink that fails closed for real HTML contexts and duplicate relevant attributes, preserving all accepted M012 behavior outside parsing.

Read first:
- AGENTS.md
- docs/requirements.md
- ARCHITECTURE.md
- docs/source-contracts/metacritic-direct-http.md
- Cargo.toml
- crates/gamepulse-worker-source/src/lib.rs

Route-verification hold:
- Before changing files, verify this is the saved-local GamePulse project on main at baseline 87cd8bf4495c40595dc6352cfba2cebc507c1ae0 with exactly the inherited M012 aggregate diff. Report tracked, ordinary-untracked, and ignored state separately. Stop on any unrelated change, different baseline, worktree, or route mismatch.

Scope:
- In the source worker only, replace the handwritten HTML tokenizer with a minimal production html5ever TreeSink retaining only effective head meta state, decoded normalized attributes, template-content separation, and ElementFlags::had_duplicate_attributes.
- Add direct html5ever = 0.39.0 and update Cargo.lock. Keep the existing response byte cap and add narrow parser-local node, depth, and allocation bounds; no reusable general parsing subsystem.
- Remove the superseded handwritten parser and tests. Add fixture-only regressions for title, textarea, comments, script/style, template content, duplicate property/content attributes, malformed HTML, entities, zero and multiple matches, and resource bounds.

Do not:
- Add scraper, lol_html, kuchiki, tl, markup5ever_rcdom, any other dependency, or any new architecture layer.
- Change source gate/circuit, URL validation, persistence, application, queue, web rendering, or M012 behavior except where necessary to connect the parser.
- Make live/network requests, source canaries, image fetches, external model calls, credential changes, commits, pushes, deployments, transcript export, archival, M011/M012 batch closure, cost finalization, or external mutations.

Constraints:
- Only real head-descendant meta elements are effective. Template contents are not effective head descendants. A relevant head meta with duplicate attributes must fail closed. Require exactly one effective property=og:image and its content. Preserve existing HTTPS and exact www.metacritic.com host validation.
- The retained parse state must have defensible local node, depth, and attribute-allocation limits. Any malformed input, duplicate attribute, missing/extra match, or limit breach returns no public cover URL.
- Preserve the existing workspace graph and source-adapter ownership.

Acceptance criteria:
- The handwritten parser is removed; html5ever is the only HTML parser path.
- All listed regressions pass without live requests; non-data RCDATA, raw-text, comments, template content, decoded entities, duplicate attributes, malformed HTML, zero/multiple matches, and resource limits demonstrate fail-closed or correct exactly-one behavior.
- Existing accepted M012 source gate, persistence, and UI behavior stays green.

Verification:
- Run focused source tests plus existing M012 settlement/storage/web tests.
- Run CARGO_NET_OFFLINE=true mise run architecture and CARGO_NET_OFFLINE=true mise run ci, git diff --check, existing audit/advisory checks if present, and aggregate privacy/scope plus tracked/untracked/ignored inspection.
- Mutation is NOT_APPLICABLE for this parser replacement: fixture and integration tests cover fail-closed mapping and no critical durable state-machine, transaction, fencing, deduplication, or source-selection invariant changes.

Stop condition:
Stop after one 90-minute implementation pass and report exact diff and check results. Do not review, correct, commit, or begin another milestone.

External actions:
Do not commit, push, deploy, send messages, configure credentials, invoke live sources or canaries, install tools or dependencies beyond exact Cargo resolution for html5ever 0.39.0, or mutate external services.

### Assistant commentary

Вердикт: DO. Сначала проверю сохранённый маршрут, базовый коммит и точное наследованное состояние M012; до этого файлы не меняю.

### Assistant commentary

Маршрут подтверждён: единственный worktree, `main`, HEAD ровно `87cd8bf…`; 11 изменённых tracked-файлов соответствуют агрегату M012. Обычных untracked нет; ignored — только локальный `target/`. Перехожу к локальной замене парсера в source worker.

### Assistant commentary

Исправляю ранний отчёт: обычных untracked не ноль, а три наследованных M012-файла — миграция и два набора тестов. Они релевантны M012; других изменений не найдено. Зависимость `html5ever 0.39.0` уже разрешена ровно по разрешённому Cargo-пути.

### Assistant commentary

Парсер проектирую как локальный `TreeSink`, не как DOM: он хранит только дерево связей для проверки потомка `<head>`, состояние релевантного `<meta>`, отдельный fragment шаблона и ограниченные метаданные узлов. Любая ошибка HTML, дубль или превышение лимита даёт `None`.

### Assistant commentary

Замена собрана. Новые fixture-тесты покрывают `<title>`, `<textarea>`, comments, script/style, template, дубли property/content, malformed HTML, entities, ноль/несколько совпадений и лимиты depth/node/attributes; все 9 focused-тестов source worker прошли офлайн. Далее — затрагиваемые settlement/storage/web проверки.

### Assistant commentary

Затронутые source, settlement, storage и catalogue/web проверки зелёные. Запускаю обязательные offline architecture и полный CI; после них сделаю финальный scope/privacy и рабочее-дерево audit без каких-либо внешних запросов.

### Assistant commentary

`mise run architecture` и полный offline CI прошли. Live-canary в общем наборе остался `ignored`, поэтому сетевых запросов не было. Делаю финальную проверку диффа, границ и состояния дерева; затем остановлюсь без коммита и без отдельного review.

### Assistant commentary

Нашёл один недочёт в финальном fixture-аудите: в depth-fixture 65 открывающих `<div>`, но 70 закрывающих; тест проходил fail-closed, однако не доказывал именно depth-границу. Исправляю только этот fixture и повторяю его проверку.

### Assistant commentary

Готово: M012a реализован в [source worker](<PROJECT_ROOT>/crates/gamepulse-worker-source/src/lib.rs).

- Handwritten HTML-парсер удалён; используется только `html5ever 0.39.0` с bounded `TreeSink`.
- Учитываются только `<meta>` внутри реального `<head>`; template content исключён.
- Дубли атрибутов, HTML errors, несколько/ноль совпадений и лимиты node/depth/attributes fail-closed.
- Добавлены offline HTML fixtures и 11 parser-regressions.
- `CARGO_NET_OFFLINE=true mise run architecture`, `mise run ci`, focused M012/storage/web tests и `git diff --check` прошли. Live canary остался ignored.
- Mutation: NOT_APPLICABLE — durable state-machine/queue invariants не менялись.
- Коммита, сетевых запросов и внешних действий не было.

Состояние: `main` на `87cd8bf…`; 14 tracked modifications, 17 ordinary-untracked M012-файлов (включая 14 новых fixtures), ignored — только `target/` (55 422 entries). Статус: `IMPLEMENTED, REVIEW_PENDING`.

<oai-mem-citation>
<citation_entries>
MEMORY.md:24-24|note=[confirmed project route historical context]
</citation_entries>
<rollout_ids>
</rollout_ids>
</oai-mem-citation>

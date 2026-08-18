# Bounded mutation testing

Run the repository-owned exact-20 selection mutation harness from the repository root:

```text
mise run mutation
```

The command uses the current tracked source in a temporary directory under
`/tmp`, never patches the working tree, runs offline, and removes its temporary
build output on exit. It has a hard ceiling of three mutants:

1. skip the New Releases-to-browse continuation;
2. invert the exact-20 commit guard;
3. emit a duplicate selected candidate.

For every declared mutant it first compiles the focused daily-crawl test target,
then classifies the result as `caught`, `noncompiling`, or `surviving`. A
surviving mutant makes the command fail. Mutation output is terminal-only and
must not be committed.

## M033 safe diagnostic mutations

Run the repository-owned diagnostic harness from the repository root:

```text
mise run diagnostic-mutation
```

The command copies only the current Git-tracked file set to a temporary
directory outside the repository, runs offline, and removes the temporary
directory on exit. It has a hard ceiling of four
mutants:

1. allow a fourth request after the diagnostic ceiling;
2. turn a parser rejection into acceptance in the aggregate-only diagnostic
   path.
3. turn a schema-valid fail-closed diagnostic exit into a success exit.
4. turn a pre-request blocked-environment report from zero attempts into one.

For each named mutant, the harness first proves that exact baseline test passes,
then verifies one and only one literal mutation was applied. It counts a mutant
as caught only when that same named test fails. A compile, Cargo, harness, copy,
or other infrastructure failure is reported separately and never counted as a
caught mutant. A compiling survivor fails the command. The harness never calls
a public source, prints no fixture payload, and never patches the working tree.

## M038 evaluator acceptance mutations

Run the repository-owned one-shot acceptance harness from the repository root:

```text
mise run acceptance-mutation
```

The command copies the current tracked and ordinary untracked source files to a
temporary directory outside the repository, runs offline, and removes the
temporary directory on exit. This includes the inherited M038 files before a
commit without copying ignored build output. It has a hard ceiling of one
named mutant:

1. schedule the hourly-discovery job a second time.

Each mutant first proves the exact named fixture integration test passes, then
applies exactly one literal source mutation. A mutant is caught only when that
same test fails. Compilation, copy, test-harness, or mutation-setup failures
are infrastructure failures rather than caught mutants; a compiling survivor
fails the harness. The harness never invokes the acceptance command or a public
source.

## M054 durable run mutations

Run the targeted durable run harness from the repository root:

```text
bash scripts/m054_mutation.sh
```

The harness copies tracked and ordinary untracked source into a temporary
directory outside the repository, runs offline, and removes it on exit. Its
hard ceiling is five named mutants:

1. let a stale reclaimed queue token settle a run item;
2. allow an item to settle after the durable run deadline;
3. retry the source job after durable source exhaustion;
4. schedule a ninth durable SEE ALL browse page;
5. drop the fixed missing-video aggregate observation from a successful settlement.

Each mutation first proves its focused baseline fixture, applies exactly one
literal change, and is caught only when that same fixture fails. A survivor or
infrastructure failure fails the harness. It makes no network request and never
patches the working tree. This is a targeted M054 P1 harness; candidate ordering,
quota, and exact-target behavior remain covered by the focused deterministic
fixtures rather than claimed as mutants in this five-attempt pass.

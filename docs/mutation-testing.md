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

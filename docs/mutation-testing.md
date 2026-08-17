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

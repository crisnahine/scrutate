# GitHub Actions

The action in `.github/actions/scrutate` downloads a release binary for the runner (Linux, macOS
or Windows; x86_64 or arm64), checks it against the release's checksum, puts it on `PATH` and
runs one command:

```yaml
jobs:
  duplicates:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: crisnahine/scrutate/.github/actions/scrutate@v0.1.0
        with:
          version: v0.1.0
          args: index
      - name: no exact duplicate functions
        run: scrutate dupes --min-tokens 80 --threshold 1 && exit 1 || [ $? -eq 1 ]
```

`args` is split on spaces; quote an argument that has spaces (`search "parse a file" -k 5`).
Later steps of the job can call `scrutate` themselves, as the second step does. Exit codes are
0 for results, 1 for none and 2 for an error, so that step fails when duplicates are listed,
passes when there are none, and fails on an error.

The action lives in a subdirectory, so it is not listed on the Marketplace.

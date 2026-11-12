<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<picture>
<source media="(prefers-color-scheme: dark)" srcset="../../docs/assets/banner-github-dark.svg">
<img src="../../docs/assets/banner-github-light.svg" alt="GitHub Actions. How do I run scrutate in a GitHub workflow?" width="880">
</picture>
</p>

# GitHub Actions

[Documentation](../../docs/README.md) &nbsp;·&nbsp; Previous: [JetBrains IDEs](../jetbrains/README.md) &nbsp;·&nbsp; Next: [pre-commit hook](../pre-commit/README.md)

<!-- End of head. -->

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
      - id: dupes
        uses: crisnahine/scrutate/.github/actions/scrutate@v0.1.0
        with:
          version: v0.1.0
          args: dupes --min-tokens 80 --threshold 1
      - name: no exact duplicate functions
        if: steps.dupes.outputs.exit-code == '0'
        run: |
          echo "exact duplicates are listed above"
          exit 1
```

Exit codes are 0 for results, 1 for none and 2 for an error. The action's step fails only on an
error; it passes on 0 and 1 and puts the code in the `exit-code` output, so the job decides what
a result means. Above, finding duplicates (`0`) fails the job and finding none (`1`) passes it.
For `search` you would gate the other way round, on `exit-code == '1'`.

`args` is split on spaces; quote an argument that has spaces or an apostrophe
(`search "don't parse twice" -k 5`). Later steps of the job can also call `scrutate` themselves:
the first step puts it on `PATH`.

The action lives in a subdirectory, so it is not listed on the Marketplace.

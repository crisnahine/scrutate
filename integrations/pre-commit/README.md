# pre-commit hook

Fail a commit when it introduces exact duplicate functions (tune `--min-tokens`):

```yaml
# .pre-commit-config.yaml
repos:
  - repo: local
    hooks:
      - id: scrutate-dupes
        name: scrutate duplicate functions
        entry: bash -c 'scrutate index >/dev/null && ! scrutate dupes --json --min-tokens 80 | grep -q "\"kind\": \"exact\""'
        language: system
        pass_filenames: false
```

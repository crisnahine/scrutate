# Scrutate bucket for Scoop

[Scoop](https://scoop.sh) manifests for [Scrutate](https://github.com/scrutate/scrutate), a code
search tool.

```pwsh
scoop bucket add scrutate https://github.com/scrutate/scoop-bucket
scoop install scrutate/scrutate
scoop update scrutate
scoop uninstall scrutate
```

Excavator (`.github/workflows/excavator.yml`) runs every 4 hours: it reads each manifest's
`checkver`, finds a new release, takes the hashes from its `SHA256SUMS` through `autoupdate`, and
commits the updated manifest to `main`.

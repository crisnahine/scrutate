# Scrutate Tap

## How do I install these formulae?

Homebrew loads a tap only once it is trusted, so trust it first:

```sh
brew trust --tap scrutate/tap
brew install scrutate/tap/scrutate
```

Or `brew tap scrutate/tap` and then `brew install scrutate`.

Or, in a `brew bundle` `Brewfile`:

```ruby
tap "scrutate/tap"
brew "scrutate"
```

## Documentation

`brew help`, `man brew` or check [Homebrew's documentation](https://docs.brew.sh).

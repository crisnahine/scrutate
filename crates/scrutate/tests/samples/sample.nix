{ pkgs ? import <nixpkgs> {} }:
let
  # Greets.
  greet = name: "hi ${name}";
  version = "1.0";
in {
  hello = pkgs.hello;
  msg = greet "x";
}

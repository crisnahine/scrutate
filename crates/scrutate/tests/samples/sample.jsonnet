// Jsonnet sample
local greet(name) = "hi " + name;
local version = "1.0";
{
  message: greet("x"),
  nested: { a: 1 },
}

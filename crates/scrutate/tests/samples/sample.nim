# Nim sample
type
  Point = object
    x: int
## Greets.
proc greet(name: string): string =
  "hi " & name
func helper(x: int): int = x * 2
method area(p: Point): float = 0.0
iterator items(p: Point): int = yield p.x
template twice(x: untyped): untyped = x * 2
macro dbg(x: untyped): untyped = x
echo greet("x")

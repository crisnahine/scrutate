import gleam/io
/// A point.
pub type Point { Point(x: Int) }
pub type Name = String
/// Greets.
pub fn greet(name: String) -> String { "hi " <> name }
fn helper(x: Int) -> Int { x * 2 }
@external(erlang, "lists", "reverse")
pub fn reverse(l: List(a)) -> List(a)
pub fn main() { io.println(greet("x")) }

import Foundation
/// A greeter.
class Greeter {
    let name: String
    init(name: String) { self.name = name }
    func greet() -> String { return "Hi \(name)" }
}
protocol Shape { func area() -> Double }
struct Pt { var x: Int }
enum Color { case red, green }
func main() { print(Greeter(name: "x").greet()) }

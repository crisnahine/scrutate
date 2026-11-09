package geometry

import "core:math"

Point :: struct {
	x, y: f64,
}

distance :: proc(a, b: Point) -> f64 {
	return math.sqrt((a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y))
}

main :: proc() {
	_ = distance(Point{0, 0}, Point{3, 4})
}

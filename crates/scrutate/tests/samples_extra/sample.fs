module Geometry

/// A point in the plane.
type Point = { X: float; Y: float }

let distance (p: Point) (q: Point) =
    sqrt ((p.X - q.X) ** 2.0 + (p.Y - q.Y) ** 2.0)

let scale factor (p: Point) = { X = p.X * factor; Y = p.Y * factor }

let origin = { X = 0.0; Y = 0.0 }

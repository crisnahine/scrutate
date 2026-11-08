const std = @import("std");
/// A point.
const Point = struct {
    x: i32,
    pub fn len(self: Point) i32 {
        return self.x;
    }
};
const Color = enum { red, green };
/// Adds.
pub fn add(a: i32, b: i32) i32 {
    return a + b;
}
test "adds" {
    try std.testing.expect(add(1, 2) == 3);
}

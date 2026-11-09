module geometry;

import std.math : sqrt;

struct Point {
    double x, y;
}

double distance(Point a, Point b) {
    return sqrt((a.x - b.x) ^^ 2 + (a.y - b.y) ^^ 2);
}

class Circle {
    Point center;
    double radius;
    double area() { return 3.14159 * radius * radius; }
}

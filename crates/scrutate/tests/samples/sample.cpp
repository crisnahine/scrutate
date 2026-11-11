// C++ sample
#include <vector>
namespace geo {
template <typename T>
class Shape {
public:
    Shape();
    virtual ~Shape();
    template <class U> void scale(U by);
    explicit operator bool() const;
    // Area of the shape.
    virtual T area() const = 0;
};
struct Point { double x, y; };
Point origin();
double dist(const Point& a, const Point& b) { return std::sqrt(a.x * b.x); }
}
int main() { geo::Point p{1, 2}; return geo::dist(p, p) > 0; }

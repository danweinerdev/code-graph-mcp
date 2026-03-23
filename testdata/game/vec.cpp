#include "vec.h"
#include <cmath>

float Vec2::length() const {
    return std::sqrt(x * x + y * y);
}

Vec2 Vec2::normalized() const {
    float len = length();
    if (len == 0) return Vec2();
    return Vec2(x / len, y / len);
}

float Vec3::length3d() const {
    return std::sqrt(x * x + y * y + z * z);
}

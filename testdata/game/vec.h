#pragma once

struct Vec2 {
    float x, y;

    Vec2() : x(0), y(0) {}
    Vec2(float x, float y) : x(x), y(y) {}

    Vec2 operator+(const Vec2& other) const { return Vec2(x + other.x, y + other.y); }
    Vec2 operator-(const Vec2& other) const { return Vec2(x - other.x, y - other.y); }
    Vec2 operator*(float scalar) const { return Vec2(x * scalar, y * scalar); }
    bool operator==(const Vec2& other) const { return x == other.x && y == other.y; }

    float length() const;
    Vec2 normalized() const;
};

struct Vec3 : public Vec2 {
    float z;

    Vec3() : Vec2(), z(0) {}
    Vec3(float x, float y, float z) : Vec2(x, y), z(z) {}

    float length3d() const;
};

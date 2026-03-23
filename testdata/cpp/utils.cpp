#include "utils.h"

namespace utils {

int clamp(int value, int low, int high) {
    if (value < low) return low;
    if (value > high) return high;
    return value;
}

float lerp(float a, float b, float t) {
    return a + (b - a) * t;
}

std::string formatString(const std::string& fmt, int value) {
    return fmt + std::to_string(value);
}

} // namespace utils

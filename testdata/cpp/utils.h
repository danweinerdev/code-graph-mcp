#pragma once

#include <string>

namespace utils {

int clamp(int value, int low, int high);
float lerp(float a, float b, float t);
std::string formatString(const std::string& fmt, int value);

} // namespace utils

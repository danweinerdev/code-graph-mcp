#pragma once

#include <string>
#include <type_traits>
#include <vector>
#include <memory>

// Using aliases for complex types
using StringVec = std::vector<std::string>;
using ByteBuffer = std::vector<unsigned char>;
using ProcessFn = std::function<void(const std::string&)>;

// Scoped enums
enum class LogLevel { Debug, Info, Warning, Error, Fatal };
enum class Status { Ok, Error, Pending, Cancelled };

// constexpr functions
constexpr int factorial(int n) {
    return n <= 1 ? 1 : n * factorial(n - 1);
}

constexpr double PI = 3.14159265358979;

// Nested namespace (C++17 style is namespace a::b, but tree-sitter may not parse it)
namespace config {
namespace defaults {

constexpr int MAX_RETRIES = 3;
constexpr int TIMEOUT_MS = 5000;

struct Settings {
    int maxRetries;
    int timeoutMs;
    LogLevel logLevel;
    bool verbose;

    static Settings createDefault() {
        return Settings{MAX_RETRIES, TIMEOUT_MS, LogLevel::Info, false};
    }
};

} // namespace defaults
} // namespace config

// RAII wrapper
template<typename T>
class UniqueHandle {
public:
    explicit UniqueHandle(T* ptr) : ptr_(ptr) {}
    ~UniqueHandle() { delete ptr_; }

    // Move semantics
    UniqueHandle(UniqueHandle&& other) noexcept : ptr_(other.ptr_) {
        other.ptr_ = nullptr;
    }

    UniqueHandle& operator=(UniqueHandle&& other) noexcept {
        if (this != &other) {
            delete ptr_;
            ptr_ = other.ptr_;
            other.ptr_ = nullptr;
        }
        return *this;
    }

    // No copy
    UniqueHandle(const UniqueHandle&) = delete;
    UniqueHandle& operator=(const UniqueHandle&) = delete;

    T* get() const { return ptr_; }
    T& operator*() const { return *ptr_; }
    T* operator->() const { return ptr_; }
    explicit operator bool() const { return ptr_ != nullptr; }

private:
    T* ptr_;
};

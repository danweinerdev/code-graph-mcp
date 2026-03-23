#pragma once

#include <stdexcept>
#include <utility>

template<typename T>
class Optional {
public:
    Optional() : hasValue_(false) {}
    Optional(const T& value) : value_(value), hasValue_(true) {}
    Optional(T&& value) : value_(std::move(value)), hasValue_(true) {}

    bool hasValue() const { return hasValue_; }
    explicit operator bool() const { return hasValue_; }

    T& value() {
        if (!hasValue_) throw std::runtime_error("no value");
        return value_;
    }

    const T& value() const {
        if (!hasValue_) throw std::runtime_error("no value");
        return value_;
    }

    T valueOr(const T& defaultVal) const {
        return hasValue_ ? value_ : defaultVal;
    }

    bool operator==(const Optional& other) const {
        if (hasValue_ != other.hasValue_) return false;
        return !hasValue_ || value_ == other.value_;
    }

    bool operator!=(const Optional& other) const {
        return !(*this == other);
    }

private:
    T value_;
    bool hasValue_;
};

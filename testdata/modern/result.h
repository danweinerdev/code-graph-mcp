#pragma once

#include <string>
#include <variant>
#include <stdexcept>

template<typename T, typename E = std::string>
class Result {
public:
    static Result ok(const T& value) {
        Result r;
        r.data_ = value;
        return r;
    }

    static Result err(const E& error) {
        Result r;
        r.data_ = error;
        return r;
    }

    bool isOk() const { return std::holds_alternative<T>(data_); }
    bool isErr() const { return std::holds_alternative<E>(data_); }

    T& value() {
        if (!isOk()) throw std::runtime_error("Result is error");
        return std::get<T>(data_);
    }

    const T& value() const {
        if (!isOk()) throw std::runtime_error("Result is error");
        return std::get<T>(data_);
    }

    E& error() {
        if (!isErr()) throw std::runtime_error("Result is ok");
        return std::get<E>(data_);
    }

    template<typename F>
    auto map(F&& fn) -> Result<decltype(fn(std::declval<T>())), E> {
        using U = decltype(fn(std::declval<T>()));
        if (isOk()) {
            return Result<U, E>::ok(fn(value()));
        }
        return Result<U, E>::err(error());
    }

    T valueOr(const T& defaultVal) const {
        return isOk() ? std::get<T>(data_) : defaultVal;
    }

private:
    std::variant<T, E> data_;
};

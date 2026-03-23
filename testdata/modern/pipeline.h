#pragma once

#include "result.h"
#include "concepts.h"
#include <functional>
#include <vector>
#include <algorithm>

// Functional-style pipeline
template<typename T>
class Pipeline {
public:
    explicit Pipeline(std::vector<T> data) : data_(std::move(data)) {}

    template<typename F>
    auto transform(F&& fn) -> Pipeline<decltype(fn(std::declval<T>()))> {
        using U = decltype(fn(std::declval<T>()));
        std::vector<U> result;
        result.reserve(data_.size());
        for (const auto& item : data_) {
            result.push_back(fn(item));
        }
        return Pipeline<U>(std::move(result));
    }

    Pipeline<T> filter(std::function<bool(const T&)> pred) {
        std::vector<T> result;
        for (const auto& item : data_) {
            if (pred(item)) {
                result.push_back(item);
            }
        }
        return Pipeline<T>(std::move(result));
    }

    template<typename Acc, typename F>
    Acc reduce(Acc init, F&& fn) const {
        Acc result = init;
        for (const auto& item : data_) {
            result = fn(result, item);
        }
        return result;
    }

    Pipeline<T> sort() {
        std::vector<T> sorted = data_;
        std::sort(sorted.begin(), sorted.end());
        return Pipeline<T>(std::move(sorted));
    }

    const std::vector<T>& collect() const { return data_; }
    size_t count() const { return data_.size(); }

    void forEach(std::function<void(const T&)> fn) const {
        for (const auto& item : data_) {
            fn(item);
        }
    }

private:
    std::vector<T> data_;
};

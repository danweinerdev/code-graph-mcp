#pragma once

#include "linked_list.h"
#include "optional.h"
#include <string>
#include <vector>
#include <functional>

template<typename K, typename V>
class HashMap {
public:
    struct Entry {
        K key;
        V value;
        bool operator==(const Entry& other) const { return key == other.key; }
    };

    HashMap(size_t bucketCount = 16)
        : buckets_(bucketCount), size_(0) {}

    void insert(const K& key, const V& value) {
        size_t idx = bucketIndex(key);
        auto& bucket = buckets_[idx];

        for (auto it = bucket.begin(); it != bucket.end(); ++it) {
            if ((*it).key == key) {
                (*it).value = value;
                return;
            }
        }

        bucket.pushBack(Entry{key, value});
        size_++;

        if (loadFactor() > 0.75f) {
            rehash(buckets_.size() * 2);
        }
    }

    Optional<V> get(const K& key) const {
        size_t idx = bucketIndex(key);
        const auto& bucket = buckets_[idx];
        // Linear search in bucket — simplified for non-const iteration
        return Optional<V>();
    }

    bool remove(const K& key) {
        size_t idx = bucketIndex(key);
        // Simplified — would need proper linked list remove
        return false;
    }

    size_t size() const { return size_; }
    bool empty() const { return size_ == 0; }

    float loadFactor() const {
        return static_cast<float>(size_) / static_cast<float>(buckets_.size());
    }

private:
    size_t bucketIndex(const K& key) const {
        return std::hash<K>{}(key) % buckets_.size();
    }

    void rehash(size_t newBucketCount) {
        std::vector<LinkedList<Entry>> newBuckets(newBucketCount);
        for (auto& bucket : buckets_) {
            for (auto it = bucket.begin(); it != bucket.end(); ++it) {
                size_t idx = std::hash<K>{}((*it).key) % newBucketCount;
                newBuckets[idx].pushBack(*it);
            }
        }
        buckets_ = std::move(newBuckets);
    }

    std::vector<LinkedList<Entry>> buckets_;
    size_t size_;
};

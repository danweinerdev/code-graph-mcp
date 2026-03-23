#pragma once

#include <cstddef>
#include <stdexcept>

template<typename T>
class LinkedList {
public:
    struct Node {
        T data;
        Node* next;
        Node(const T& val) : data(val), next(nullptr) {}
    };

    class Iterator {
    public:
        Iterator(Node* node) : current_(node) {}

        T& operator*() { return current_->data; }
        T* operator->() { return &current_->data; }

        Iterator& operator++() {
            current_ = current_->next;
            return *this;
        }

        bool operator==(const Iterator& other) const { return current_ == other.current_; }
        bool operator!=(const Iterator& other) const { return current_ != other.current_; }

    private:
        Node* current_;
    };

    LinkedList() : head_(nullptr), size_(0) {}

    ~LinkedList() {
        clear();
    }

    void pushFront(const T& value) {
        Node* newNode = new Node(value);
        newNode->next = head_;
        head_ = newNode;
        size_++;
    }

    void pushBack(const T& value) {
        Node* newNode = new Node(value);
        if (!head_) {
            head_ = newNode;
        } else {
            Node* current = head_;
            while (current->next) {
                current = current->next;
            }
            current->next = newNode;
        }
        size_++;
    }

    T& front() {
        if (!head_) throw std::runtime_error("empty list");
        return head_->data;
    }

    void popFront() {
        if (!head_) return;
        Node* old = head_;
        head_ = head_->next;
        delete old;
        size_--;
    }

    void clear() {
        while (head_) {
            popFront();
        }
    }

    bool empty() const { return head_ == nullptr; }
    size_t size() const { return size_; }

    Iterator begin() { return Iterator(head_); }
    Iterator end() { return Iterator(nullptr); }

    bool contains(const T& value) const {
        Node* current = head_;
        while (current) {
            if (current->data == value) return true;
            current = current->next;
        }
        return false;
    }

private:
    Node* head_;
    size_t size_;
};

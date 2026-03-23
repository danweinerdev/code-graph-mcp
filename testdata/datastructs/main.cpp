#include "hash_map.h"
#include "optional.h"
#include "linked_list.h"
#include <iostream>
#include <string>

void testLinkedList() {
    LinkedList<int> list;
    list.pushFront(1);
    list.pushFront(2);
    list.pushBack(3);

    for (auto it = list.begin(); it != list.end(); ++it) {
        std::cout << *it << " ";
    }
    std::cout << std::endl;

    std::cout << "Contains 2: " << list.contains(2) << std::endl;
    std::cout << "Size: " << list.size() << std::endl;

    list.popFront();
    std::cout << "Front after pop: " << list.front() << std::endl;
}

void testOptional() {
    Optional<int> empty;
    Optional<int> full(42);

    std::cout << "Has value: " << full.hasValue() << std::endl;
    std::cout << "Value: " << full.value() << std::endl;
    std::cout << "Or default: " << empty.valueOr(99) << std::endl;

    if (full) {
        std::cout << "Boolean conversion works" << std::endl;
    }
}

void testHashMap() {
    HashMap<std::string, int> map;
    map.insert("one", 1);
    map.insert("two", 2);
    map.insert("three", 3);

    std::cout << "Size: " << map.size() << std::endl;
    std::cout << "Load factor: " << map.loadFactor() << std::endl;
}

int main() {
    testLinkedList();
    testOptional();
    testHashMap();
    return 0;
}

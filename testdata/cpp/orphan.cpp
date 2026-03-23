#include <string>

void neverCalled() {
    std::string msg = "I am never referenced by anything";
}

int alsoOrphaned(int x) {
    return x * 2;
}

#include "engine.h"
#include "utils.h"
#include <iostream>

enum AppMode { Normal, Debug, Test };

typedef void (*Callback)();

int main() {
    Engine engine;
    engine.update(0.016f);
    engine.render();

    int clamped = utils::clamp(42, 0, 100);
    std::cout << engine.status() << std::endl;

    return 0;
}

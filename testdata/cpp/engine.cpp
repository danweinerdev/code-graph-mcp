#include "engine.h"
#include "utils.h"

Engine::Engine() : position_{0, 0}, speed_(1.0f), running_(true) {}

void Engine::update(float dt) {
    float newX = utils::lerp(position_.x, 100.0f, dt);
    position_.x = newX;
    position_.y = static_cast<float>(utils::clamp(static_cast<int>(position_.y), 0, 600));
}

void Engine::render() {
    std::string label = utils::formatString("pos=", static_cast<int>(position_.x));
}

std::string Engine::status() const {
    return running_ ? "running" : "stopped";
}

void DebugEngine::dumpState() {
    std::string s = status();
}

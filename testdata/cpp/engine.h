#pragma once

#include <string>

struct Vec2 {
    float x;
    float y;
};

class Engine {
public:
    Engine();
    void update(float dt);
    void render();
    std::string status() const;

private:
    Vec2 position_;
    float speed_;
    bool running_;
};

class DebugEngine : public Engine {
public:
    void dumpState();
};

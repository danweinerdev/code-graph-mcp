#pragma once

#include "vec.h"
#include <string>
#include <vector>
#include <memory>

class Component;

class Entity {
public:
    Entity(const std::string& name);
    virtual ~Entity();

    virtual void update(float dt);
    virtual void render();

    void addComponent(std::unique_ptr<Component> comp);
    Component* getComponent(const std::string& type) const;

    const std::string& name() const { return name_; }
    Vec2 position() const { return position_; }
    void setPosition(Vec2 pos) { position_ = pos; }

protected:
    std::string name_;
    Vec2 position_;
    std::vector<std::unique_ptr<Component>> components_;
};

class Component {
public:
    Component(Entity* owner) : owner_(owner) {}
    virtual ~Component() = default;

    virtual void update(float dt) = 0;
    virtual std::string type() const = 0;

protected:
    Entity* owner_;
};

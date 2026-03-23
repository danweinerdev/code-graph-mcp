#include "entity.h"

Entity::Entity(const std::string& name) : name_(name), position_() {}

Entity::~Entity() {}

void Entity::update(float dt) {
    for (auto& comp : components_) {
        comp->update(dt);
    }
}

void Entity::render() {
    // Base render — subclasses override.
}

void Entity::addComponent(std::unique_ptr<Component> comp) {
    components_.push_back(std::move(comp));
}

Component* Entity::getComponent(const std::string& type) const {
    for (const auto& comp : components_) {
        if (comp->type() == type) {
            return comp.get();
        }
    }
    return nullptr;
}

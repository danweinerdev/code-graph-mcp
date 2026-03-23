#include "player.h"
#include <algorithm>

// --- PhysicsComponent ---

PhysicsComponent::PhysicsComponent(Entity* owner, float mass)
    : Component(owner), mass_(mass), velocity_(), acceleration_() {}

void PhysicsComponent::update(float dt) {
    velocity_ = velocity_ + acceleration_ * dt;
    Vec2 newPos = owner_->position() + velocity_ * dt;
    owner_->setPosition(newPos);
    acceleration_ = Vec2();
}

void PhysicsComponent::applyForce(Vec2 force) {
    acceleration_ = acceleration_ + force * (1.0f / mass_);
}

// --- RenderComponent ---

RenderComponent::RenderComponent(Entity* owner, const std::string& sprite)
    : Component(owner), spritePath_(sprite) {}

void RenderComponent::update(float dt) {
    // Render system handles actual drawing.
}

// --- Player ---

Player::Player(const std::string& name, int health)
    : Entity(name), health_(health), maxHealth_(health) {}

void Player::update(float dt) {
    Entity::update(dt);
    if (!isAlive()) return;
}

void Player::render() {
    Entity::render();
    // Draw health bar.
}

void Player::takeDamage(int amount) {
    health_ = std::max(0, health_ - amount);
}

void Player::heal(int amount) {
    health_ = std::min(maxHealth_, health_ + amount);
}

// --- NPC ---

NPC::NPC(const std::string& name, const std::string& dialogue)
    : Entity(name), dialogue_(dialogue), hasInteracted_(false) {}

void NPC::update(float dt) {
    Entity::update(dt);
}

void NPC::interact(Player& player) {
    if (!hasInteracted_) {
        hasInteracted_ = true;
        player.heal(10);
    }
}

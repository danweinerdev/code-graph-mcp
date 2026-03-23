#pragma once

#include "entity.h"

class PhysicsComponent : public Component {
public:
    PhysicsComponent(Entity* owner, float mass);
    void update(float dt) override;
    std::string type() const override { return "physics"; }

    void applyForce(Vec2 force);
    Vec2 velocity() const { return velocity_; }

private:
    float mass_;
    Vec2 velocity_;
    Vec2 acceleration_;
};

class RenderComponent : public Component {
public:
    RenderComponent(Entity* owner, const std::string& sprite);
    void update(float dt) override;
    std::string type() const override { return "render"; }

private:
    std::string spritePath_;
};

class Player : public Entity {
public:
    Player(const std::string& name, int health);
    void update(float dt) override;
    void render() override;

    void takeDamage(int amount);
    void heal(int amount);
    bool isAlive() const { return health_ > 0; }
    int health() const { return health_; }

private:
    int health_;
    int maxHealth_;
};

class NPC : public Entity {
public:
    NPC(const std::string& name, const std::string& dialogue);
    void update(float dt) override;
    void interact(Player& player);

private:
    std::string dialogue_;
    bool hasInteracted_;
};

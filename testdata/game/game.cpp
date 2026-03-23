#include "player.h"
#include <iostream>

class World {
public:
    void addEntity(std::unique_ptr<Entity> entity) {
        entities_.push_back(std::move(entity));
    }

    void update(float dt) {
        for (auto& e : entities_) {
            e->update(dt);
        }
    }

    void render() {
        for (auto& e : entities_) {
            e->render();
        }
    }

    Entity* findByName(const std::string& name) {
        for (auto& e : entities_) {
            if (e->name() == name) return e.get();
        }
        return nullptr;
    }

private:
    std::vector<std::unique_ptr<Entity>> entities_;
};

int main() {
    World world;

    auto player = std::make_unique<Player>("Hero", 100);
    player->addComponent(std::make_unique<PhysicsComponent>(player.get(), 75.0f));
    player->addComponent(std::make_unique<RenderComponent>(player.get(), "hero.png"));
    player->setPosition(Vec2(100, 200));

    auto npc = std::make_unique<NPC>("Healer", "Need healing?");
    npc->setPosition(Vec2(300, 200));

    Player* playerPtr = player.get();
    NPC* npcPtr = dynamic_cast<NPC*>(npc.get());

    world.addEntity(std::move(player));
    world.addEntity(std::move(npc));

    for (int i = 0; i < 60; i++) {
        world.update(0.016f);
        world.render();
    }

    playerPtr->takeDamage(30);
    if (npcPtr) npcPtr->interact(*playerPtr);

    std::cout << playerPtr->name() << " HP: " << playerPtr->health() << std::endl;

    return 0;
}

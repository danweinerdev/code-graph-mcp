#pragma once

#include <string>
#include <vector>
#include <functional>
#include <unordered_map>
#include <memory>

enum class EventType {
    Click,
    KeyPress,
    Resize,
    Custom
};

struct Event {
    EventType type;
    std::string name;
    int data;

    Event(EventType t, const std::string& n, int d = 0)
        : type(t), name(n), data(d) {}
};

// C-style callback typedef
typedef void (*EventCallback)(const Event&);

// Modern callback using std::function
using EventHandler = std::function<void(const Event&)>;

class IEventListener {
public:
    virtual ~IEventListener() = default;
    virtual void onEvent(const Event& event) = 0;
};

class EventBus {
public:
    static EventBus& instance() {
        static EventBus bus;
        return bus;
    }

    void subscribe(EventType type, EventHandler handler);
    void subscribe(EventType type, IEventListener* listener);
    void subscribeCStyle(EventType type, EventCallback callback);

    void publish(const Event& event);
    void clear();

    size_t listenerCount(EventType type) const;

private:
    EventBus() = default;

    struct Subscription {
        EventHandler handler;
    };

    std::unordered_map<int, std::vector<Subscription>> handlers_;
    std::unordered_map<int, std::vector<IEventListener*>> listeners_;
    std::unordered_map<int, std::vector<EventCallback>> cCallbacks_;
};
